//! Lightweight GGUF binary header reader for context length and chat-template inspection.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use thiserror::Error;

use super::{infer_supports_thinking, template_supports_thinking};

/// Typed errors produced when parsing a `.gguf` binary header.
#[derive(Debug, Error)]
pub enum GgufError {
    #[error("Invalid GGUF magic header")]
    InvalidMagic,
    #[error("Unsupported GGUF version: {0}")]
    UnsupportedVersion(u32),
    #[error("GGUF nested array depth exceeded")]
    DepthLimitExceeded,
    #[error("Unexpected end of GGUF header")]
    Truncated,
    #[error("GGUF I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub use crate::naming::{context_presets_up_to, format_context_label, format_short_context};

const GGUF_MAGIC: &[u8; 4] = b"GGUF";
const MAX_KV_PAIRS: u64 = 4096;
const MAX_STRING_BYTES: usize = 256 * 1024;
const MAX_ARRAY_DEPTH: usize = 8;
const MAX_CACHE_ENTRIES: usize = 512;

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
        let Ok(file) = File::open(path) else {
            return GgufMetadata::default();
        };
        let sig = file_signature(&file);
        if let Some((len, mtime)) = sig
            && let Ok(guard) = self.entries.lock()
            && let Some((c_len, c_mtime, meta)) = guard.get(path)
            && *c_len == len
            && *c_mtime == mtime
        {
            return meta.clone();
        }

        let meta = inspect_open_file(path, file, sig.map(|(l, _)| l));
        if let Some((len, mtime)) = sig
            && let Ok(mut guard) = self.entries.lock()
        {
            if guard.len() >= MAX_CACHE_ENTRIES {
                guard.clear();
            }
            guard.insert(path.to_path_buf(), (len, mtime, meta.clone()));
        }
        meta
    }

    /// Warm this cache in parallel (bounded concurrency) on the provided Tokio runtime handle.
    pub fn warm(&self, paths: Vec<PathBuf>, handle: &tokio::runtime::Handle) {
        if paths.is_empty() {
            return;
        }
        let chunk_size = paths.len().div_ceil(4).max(1);
        for chunk in paths.chunks(chunk_size) {
            let chunk = chunk.to_vec();
            let cache = self.clone();
            handle.spawn_blocking(move || {
                for path in chunk {
                    let _ = cache.inspect(&path);
                }
            });
        }
    }
}

fn file_signature(file: &File) -> Option<(u64, SystemTime)> {
    let meta = file.metadata().ok()?;
    let mtime = meta.modified().ok()?;
    Some((meta.len(), mtime))
}

fn inspect_open_file(path: &Path, file: File, file_len: Option<u64>) -> GgufMetadata {
    let file_len = file_len.unwrap_or_else(|| file.metadata().map(|m| m.len()).unwrap_or(0));
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut meta = parse_gguf_reader(&mut reader, file_len).unwrap_or_default();

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

/// Inspect a `.gguf` file header directly for `<arch>.context_length` and `tokenizer.chat_template`.
pub fn inspect_gguf_metadata(path: &Path) -> GgufMetadata {
    let Ok(file) = File::open(path) else {
        return GgufMetadata::default();
    };
    let len = file.metadata().map(|m| m.len()).ok();
    inspect_open_file(path, file, len)
}

#[derive(Default)]
struct GgufParseState {
    arch: Option<String>,
    ctx_entries: HashMap<String, u32>,
    first_ctx: Option<u32>,
    supports_thinking: bool,
    supports_vision: bool,
    supports_audio: bool,
    is_clip: bool,
}

fn parse_gguf_reader<R: Read + Seek>(r: &mut R, file_len: u64) -> Option<GgufMetadata> {
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
        let key = read_gguf_string(r, 256, file_len)?;
        let val_type = read_u32(r)?;
        if parse_gguf_kv_entry(r, &key, val_type, &mut state, file_len).is_none() {
            break;
        }
    }

    let arch_ctx = state.arch.as_deref().and_then(|a| {
        state
            .ctx_entries
            .get(&format!("{a}.context_length"))
            .copied()
    });

    Some(GgufMetadata {
        max_context: arch_ctx.or(state.first_ctx),
        supports_thinking: state.supports_thinking,
        supports_vision: state.supports_vision,
        supports_audio: state.supports_audio,
        is_clip: state.is_clip,
    })
}

fn parse_gguf_kv_entry<R: Read + Seek>(
    r: &mut R,
    key: &str,
    val_type: u32,
    state: &mut GgufParseState,
    file_len: u64,
) -> Option<()> {
    if key == "general.architecture" && val_type == 8 {
        state.arch = read_gguf_string(r, 128, file_len);
        if let Some(a) = state.arch.as_deref() {
            state.is_clip |= a == "clip";
            state.supports_vision |= matches!(
                a,
                "qwen2vl"
                    | "qwen25vl"
                    | "qwen3vl"
                    | "llava"
                    | "gemma3"
                    | "gemma4"
                    | "llama4"
                    | "mistral3"
                    | "pixtral"
            );
        }
    } else if key == "clip.has_vision_encoder" {
        if read_gguf_bool(r, val_type, file_len)? {
            state.supports_vision = true;
            state.is_clip = true;
        }
    } else if key == "clip.has_audio_encoder" {
        if read_gguf_bool(r, val_type, file_len)? {
            state.supports_audio = true;
            state.is_clip = true;
        }
    } else if key == "tokenizer.chat_template" && val_type == 8 {
        if let Some(tmpl) = read_gguf_string(r, MAX_STRING_BYTES, file_len) {
            state.supports_thinking |= template_supports_thinking(&tmpl);
            state.supports_vision |= template_supports_vision(&tmpl);
            state.supports_audio |= template_supports_audio(&tmpl);
        }
    } else if key.ends_with(".context_length") {
        if let Some(ctx) = read_gguf_uint_as_u32(r, val_type, file_len) {
            state.ctx_entries.insert(key.to_string(), ctx);
            state.first_ctx = state.first_ctx.or(Some(ctx));
        }
    } else {
        return skip_gguf_value(r, val_type, file_len, 0);
    }
    Some(())
}

fn read_gguf_bool<R: Read + Seek>(r: &mut R, val_type: u32, file_len: u64) -> Option<bool> {
    if matches!(val_type, 0 | 1 | 7) {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok()?;
        Some(b[0] != 0)
    } else {
        skip_gguf_value(r, val_type, file_len, 0)?;
        None
    }
}

fn template_supports_vision(template: &str) -> bool {
    let lower = template.to_ascii_lowercase();
    lower.contains("image_url")
        || lower.contains("<start_of_image>")
        || lower.contains("<|image|>")
        || lower.contains("<image>")
        || lower.contains("<|vision_start|>")
        || lower.contains("<|image_pad|>")
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

fn read_gguf_string<R: Read + Seek>(r: &mut R, max_len: usize, file_len: u64) -> Option<String> {
    let raw_len = read_u64(r)?;
    let Ok(len) = usize::try_from(raw_len) else {
        return None;
    };
    if len > max_len {
        skip_bytes(r, raw_len, file_len)?;
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn read_gguf_uint_as_u32<R: Read + Seek>(r: &mut R, val_type: u32, file_len: u64) -> Option<u32> {
    match val_type {
        4 => read_u32(r),                                                         // UINT32
        5 => read_u32(r).map(|v| (v as i32).max(0) as u32),                       // INT32
        10 => read_u64(r).map(|v| v.min(u32::MAX as u64) as u32),                 // UINT64
        11 => read_u64(r).map(|v| (v as i64).max(0).min(u32::MAX as i64) as u32), // INT64
        _ => {
            skip_gguf_value(r, val_type, file_len, 0)?;
            None
        }
    }
}

fn skip_bytes<R: Seek>(r: &mut R, count: u64, file_len: u64) -> Option<()> {
    let pos = r.stream_position().ok()?;
    let target = pos.checked_add(count)?;
    if target > file_len {
        return None;
    }
    let delta = i64::try_from(count).ok()?;
    r.seek(SeekFrom::Current(delta)).ok()?;
    Some(())
}

fn skip_gguf_value<R: Read + Seek>(
    r: &mut R,
    val_type: u32,
    file_len: u64,
    depth: usize,
) -> Option<()> {
    match val_type {
        0 | 1 | 7 => skip_bytes(r, 1, file_len),
        2 | 3 => skip_bytes(r, 2, file_len),
        4..=6 => skip_bytes(r, 4, file_len),
        10..=12 => skip_bytes(r, 8, file_len),
        8 => {
            let len = read_u64(r)?;
            skip_bytes(r, len, file_len)
        }
        9 => {
            if depth >= MAX_ARRAY_DEPTH {
                return None;
            }
            let elem_type = read_u32(r)?;
            let count = read_u64(r)?;
            if let Some(fixed_sz) = fixed_elem_size(elem_type) {
                let total = count.checked_mul(fixed_sz)?;
                return skip_bytes(r, total, file_len);
            }
            for _ in 0..count {
                skip_gguf_value(r, elem_type, file_len, depth + 1)?;
            }
            Some(())
        }
        _ => None,
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
