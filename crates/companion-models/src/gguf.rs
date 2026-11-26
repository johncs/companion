//! Lightweight GGUF binary header reader for context length and chat-template inspection.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use super::{infer_supports_thinking, template_supports_thinking};

const GGUF_MAGIC: &[u8; 4] = b"GGUF";
const MAX_KV_PAIRS: u64 = 4096;
const MAX_STRING_BYTES: usize = 256 * 1024;
const STANDARD_CONTEXT_PRESETS: &[u32] = &[
    2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576, 2097152, 4194304,
    8388608, 10485760,
];

/// Metadata extracted from a `.gguf` header without loading model weights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GgufMetadata {
    pub max_context: Option<u32>,
    pub supports_thinking: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub is_clip: bool,
}

type CacheEntry = (u64, SystemTime, GgufMetadata);

/// Injectable, mtime-invalidated in-memory cache of parsed `.gguf` header metadata.
#[derive(Clone, Default)]
pub struct GgufMetadataCache {
    entries: Arc<Mutex<HashMap<PathBuf, CacheEntry>>>,
}

impl GgufMetadataCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inspect `path`, returning a cached entry if file size and mtime match.
    pub fn inspect(&self, path: &Path) -> GgufMetadata {
        let sig = file_signature(path);
        if let Some((len, mtime)) = sig
            && let Ok(guard) = self.entries.lock()
            && let Some((c_len, c_mtime, meta)) = guard.get(path)
            && *c_len == len
            && *c_mtime == mtime
        {
            return meta.clone();
        }

        let meta = inspect_gguf_metadata(path);
        if let Some((len, mtime)) = sig
            && let Ok(mut guard) = self.entries.lock()
        {
            guard.insert(path.to_path_buf(), (len, mtime, meta.clone()));
        }
        meta
    }

    /// Warm this cache in parallel on the provided Tokio runtime handle.
    pub fn warm(&self, paths: Vec<PathBuf>, handle: &tokio::runtime::Handle) {
        for path in paths {
            let cache = self.clone();
            handle.spawn_blocking(move || {
                let _ = cache.inspect(&path);
            });
        }
    }
}

fn file_signature(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some((meta.len(), mtime))
}

/// Inspect a `.gguf` file header directly for `<arch>.context_length` and `tokenizer.chat_template`.
pub fn inspect_gguf_metadata(path: &Path) -> GgufMetadata {
    let mut meta = File::open(path)
        .ok()
        .map(|f| BufReader::with_capacity(64 * 1024, f))
        .and_then(|mut r| parse_gguf_reader(&mut r))
        .unwrap_or_default();

    if !meta.supports_thinking
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(infer_supports_thinking)
    {
        meta.supports_thinking = true;
    }
    meta
}

/// Return friendly context-window dropdown options capped to `max_ctx`.
pub fn context_presets_up_to(max_ctx: u32) -> Vec<u32> {
    let cap = max_ctx.max(2048);
    let mut presets: Vec<u32> = STANDARD_CONTEXT_PRESETS
        .iter()
        .copied()
        .filter(|&v| v <= cap)
        .collect();
    if !presets.contains(&cap) {
        presets.push(cap);
    }
    if presets.is_empty() {
        presets.push(cap);
    }
    presets
}

/// Format a token count cleanly for dropdown labels (e.g. `128K (131,072)`).
pub fn format_context_label(tokens: u32) -> String {
    format!(
        "{} ({})",
        format_short_context(tokens),
        format_thousands(tokens)
    )
}

/// Format a token count concisely (e.g. `4K`, `128K`, `1M`, `10M`).
pub fn format_short_context(tokens: u32) -> String {
    const ONE_M: u32 = 1024 * 1024;
    if tokens >= ONE_M {
        if tokens.is_multiple_of(ONE_M) {
            format!("{}M", tokens / ONE_M)
        } else {
            format!("{:.1}M", (tokens as f64) / (ONE_M as f64))
        }
    } else if tokens >= 1024 {
        format!("{}K", tokens / 1024)
    } else {
        tokens.to_string()
    }
}

fn format_thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + 3);
    for (idx, ch) in s.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[derive(Default)]
struct GgufParseState {
    arch: Option<String>,
    arch_ctx: Option<u32>,
    any_ctx: Option<u32>,
    supports_thinking: bool,
    supports_vision: bool,
    supports_audio: bool,
    is_clip: bool,
}

fn parse_gguf_reader<R: Read>(r: &mut R) -> Option<GgufMetadata> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).ok()?;
    if &magic != GGUF_MAGIC {
        return None;
    }

    let version = read_u32(r)?;
    if !(2..=3).contains(&version) {
        return None;
    }

    let _tensor_count = read_u64(r)?;
    let kv_count = read_u64(r)?.min(MAX_KV_PAIRS);
    let mut state = GgufParseState::default();

    for _ in 0..kv_count {
        let key = read_gguf_string(r, 256)?;
        let val_type = read_u32(r)?;
        if !parse_gguf_kv_entry(r, &key, val_type, &mut state) {
            break;
        }
    }

    Some(GgufMetadata {
        max_context: state.arch_ctx.or(state.any_ctx),
        supports_thinking: state.supports_thinking,
        supports_vision: state.supports_vision,
        supports_audio: state.supports_audio,
        is_clip: state.is_clip,
    })
}

fn parse_gguf_kv_entry<R: Read>(
    r: &mut R,
    key: &str,
    val_type: u32,
    state: &mut GgufParseState,
) -> bool {
    if key == "general.architecture" && val_type == 8 {
        state.arch = read_gguf_string(r, 128);
        if let Some(a) = state.arch.as_deref() {
            state.is_clip |= a == "clip";
            state.supports_vision |= matches!(a, "qwen2vl" | "qwen25vl" | "llava");
        }
    } else if key == "clip.has_vision_encoder" {
        if read_gguf_bool(r, val_type) == Some(true) {
            state.supports_vision = true;
            state.is_clip = true;
        }
    } else if key == "clip.has_audio_encoder" {
        if read_gguf_bool(r, val_type) == Some(true) {
            state.supports_audio = true;
            state.is_clip = true;
        }
    } else if key == "tokenizer.chat_template" && val_type == 8 {
        if let Some(tmpl) = read_gguf_string(r, MAX_STRING_BYTES) {
            state.supports_thinking |= template_supports_thinking(&tmpl);
            state.supports_vision |= template_supports_vision(&tmpl);
            state.supports_audio |= template_supports_audio(&tmpl);
        }
    } else if key.ends_with(".context_length") {
        if let Some(ctx) = read_gguf_uint_as_u32(r, val_type) {
            let matches_arch = state
                .arch
                .as_deref()
                .is_some_and(|a| key == format!("{a}.context_length"));
            if matches_arch {
                state.arch_ctx = Some(ctx);
            } else {
                state.any_ctx = state.any_ctx.or(Some(ctx));
            }
        }
    } else {
        return skip_gguf_value(r, val_type);
    }
    true
}

fn read_gguf_bool<R: Read>(r: &mut R, val_type: u32) -> Option<bool> {
    if matches!(val_type, 0 | 1 | 7) {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        Some(b[0] != 0)
    } else {
        skip_gguf_value(r, val_type);
        None
    }
}

fn template_supports_vision(template: &str) -> bool {
    let lower = template.to_ascii_lowercase();
    lower.contains("image_url")
        || lower.contains("<start_of_image>")
        || lower.contains("<|image|>")
        || lower.contains("<image>")
}

fn template_supports_audio(template: &str) -> bool {
    let lower = template.to_ascii_lowercase();
    lower.contains("<start_of_audio>") || lower.contains("<|audio|>") || lower.contains("<audio>")
}

fn read_u32<R: Read>(r: &mut R) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

fn read_gguf_string<R: Read>(r: &mut R, max_len: usize) -> Option<String> {
    let len = read_u64(r)? as usize;
    if len > max_len {
        let _ = skip_bytes(r, len as u64);
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn read_gguf_uint_as_u32<R: Read>(r: &mut R, val_type: u32) -> Option<u32> {
    match val_type {
        4 => read_u32(r),                                                         // UINT32
        5 => read_u32(r).map(|v| (v as i32).max(0) as u32),                       // INT32
        10 => read_u64(r).map(|v| v.min(u32::MAX as u64) as u32),                 // UINT64
        11 => read_u64(r).map(|v| (v as i64).max(0).min(u32::MAX as i64) as u32), // INT64
        _ => {
            skip_gguf_value(r, val_type);
            None
        }
    }
}

fn skip_bytes<R: Read>(r: &mut R, count: u64) -> bool {
    let mut limited = r.take(count);
    std::io::copy(&mut limited, &mut std::io::sink()).is_ok_and(|n| n == count)
}

fn skip_gguf_value<R: Read>(r: &mut R, val_type: u32) -> bool {
    match val_type {
        0 | 1 | 7 => skip_bytes(r, 1), // UINT8, INT8, BOOL
        2 | 3 => skip_bytes(r, 2),     // UINT16, INT16
        4..=6 => skip_bytes(r, 4),     // UINT32, INT32, FLOAT32
        10..=12 => skip_bytes(r, 8),   // UINT64, INT64, FLOAT64
        8 => {
            let Some(len) = read_u64(r) else {
                return false;
            };
            skip_bytes(r, len)
        }
        9 => {
            let Some(elem_type) = read_u32(r) else {
                return false;
            };
            let Some(count) = read_u64(r) else {
                return false;
            };
            if let Some(fixed_sz) = fixed_elem_size(elem_type) {
                return skip_bytes(r, count.saturating_mul(fixed_sz));
            }
            for _ in 0..count {
                if !skip_gguf_value(r, elem_type) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

fn fixed_elem_size(val_type: u32) -> Option<u64> {
    match val_type {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
#[path = "gguf_tests.rs"]
mod tests;
