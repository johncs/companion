//! Lightweight GGUF binary header reader for context length and chat-template inspection.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::{infer_supports_thinking, template_supports_thinking};

const GGUF_MAGIC: &[u8; 4] = b"GGUF";
const MAX_KV_PAIRS: u64 = 4096;
const MAX_STRING_BYTES: usize = 256 * 1024;
const STANDARD_CONTEXT_PRESETS: &[u32] = &[
    2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576, 2097152, 4194304,
    8388608, 10485760,
];

static METADATA_CACHE: OnceLock<Mutex<HashMap<PathBuf, GgufMetadata>>> = OnceLock::new();

/// Metadata extracted from a `.gguf` header without loading model weights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GgufMetadata {
    pub max_context: Option<u32>,
    pub supports_thinking: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub is_clip: bool,
}

/// Inspect a `.gguf` file header for `<arch>.context_length` and `tokenizer.chat_template`.
/// Results are cached in memory per path so repeated UI opens are instantaneous.
pub fn inspect_gguf_metadata(path: &Path) -> GgufMetadata {
    let cache = METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(path)
    {
        return cached.clone();
    }

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

    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), meta.clone());
    }
    meta
}

/// Warm [`METADATA_CACHE`] in parallel across CPU cores on the shared Tokio runtime.
pub fn warm_gguf_metadata_cache(paths: Vec<PathBuf>) {
    for path in paths {
        companion_core::runtime().spawn_blocking(move || {
            let _ = inspect_gguf_metadata(&path);
        });
    }
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
    format!("{} ({})", format_short_context(tokens), format_thousands(tokens))
}

/// Format a token count concisely (e.g. `4K`, `128K`, `1M`, `10M`).
pub fn format_short_context(tokens: u32) -> String {
    const ONE_M: u32 = 1024 * 1024;
    if tokens >= ONE_M {
        if tokens % ONE_M == 0 {
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

    let mut arch: Option<String> = None;
    let mut arch_ctx: Option<u32> = None;
    let mut any_ctx: Option<u32> = None;
    let mut supports_thinking = false;
    let mut supports_vision = false;
    let mut supports_audio = false;
    let mut is_clip = false;

    for _ in 0..kv_count {
        let key = read_gguf_string(r, 256)?;
        let val_type = read_u32(r)?;

        if key == "general.architecture" && val_type == 8 {
            arch = read_gguf_string(r, 128);
            if let Some(a) = arch.as_deref() {
                if a == "clip" {
                    is_clip = true;
                }
                if matches!(a, "qwen2vl" | "qwen25vl" | "llava") {
                    supports_vision = true;
                }
            }
        } else if key == "clip.has_vision_encoder" {
            if read_gguf_bool(r, val_type) == Some(true) {
                supports_vision = true;
                is_clip = true;
            }
        } else if key == "clip.has_audio_encoder" {
            if read_gguf_bool(r, val_type) == Some(true) {
                supports_audio = true;
                is_clip = true;
            }
        } else if key == "tokenizer.chat_template" && val_type == 8 {
            if let Some(tmpl) = read_gguf_string(r, MAX_STRING_BYTES) {
                if template_supports_thinking(&tmpl) {
                    supports_thinking = true;
                }
                if template_supports_vision(&tmpl) {
                    supports_vision = true;
                }
                if template_supports_audio(&tmpl) {
                    supports_audio = true;
                }
            }
        } else if key.ends_with(".context_length") {
            if let Some(ctx) = read_gguf_uint_as_u32(r, val_type) {
                if arch
                    .as_deref()
                    .is_some_and(|a| key == format!("{a}.context_length"))
                {
                    arch_ctx = Some(ctx);
                } else {
                    any_ctx = any_ctx.or(Some(ctx));
                }
            }
        } else if !skip_gguf_value(r, val_type) {
            break;
        }
    }

    Some(GgufMetadata {
        max_context: arch_ctx.or(any_ctx),
        supports_thinking,
        supports_vision,
        supports_audio,
        is_clip,
    })
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
    lower.contains("<start_of_audio>")
        || lower.contains("<|audio|>")
        || lower.contains("<audio>")
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
        4 => read_u32(r),                                  // UINT32
        5 => read_u32(r).map(|v| (v as i32).max(0) as u32), // INT32
        10 => read_u64(r).map(|v| v.min(u32::MAX as u64) as u32), // UINT64
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
        4 | 5 | 6 => skip_bytes(r, 4), // UINT32, INT32, FLOAT32
        10 | 11 | 12 => skip_bytes(r, 8), // UINT64, INT64, FLOAT64
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
        4 | 5 | 6 => Some(4),
        10 | 11 | 12 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn push_str(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn parses_gguf_context_length_and_chat_template() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&3u64.to_le_bytes()); // kv_count

        push_str(&mut buf, "general.architecture");
        buf.extend_from_slice(&8u32.to_le_bytes());
        push_str(&mut buf, "gemma4");

        push_str(&mut buf, "gemma4.context_length");
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&131072u32.to_le_bytes());

        push_str(&mut buf, "tokenizer.chat_template");
        buf.extend_from_slice(&8u32.to_le_bytes());
        push_str(&mut buf, "{% if enable_thinking %}<|channel>thought{% endif %}");

        let meta = parse_gguf_reader(&mut Cursor::new(buf)).expect("valid GGUF header");
        assert_eq!(meta.max_context, Some(131072));
        assert!(meta.supports_thinking);
        assert!(!meta.is_clip);
    }

    #[test]
    fn detects_clip_projector_architecture() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"GGUF");
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        buf.extend_from_slice(&1u64.to_le_bytes()); // kv_count

        push_str(&mut buf, "general.architecture");
        buf.extend_from_slice(&8u32.to_le_bytes());
        push_str(&mut buf, "clip");

        let meta = parse_gguf_reader(&mut Cursor::new(buf)).expect("valid GGUF header");
        assert!(meta.is_clip);
    }

    #[test]
    fn builds_friendly_context_presets_capped_to_max() {
        assert_eq!(
            context_presets_up_to(131072),
            vec![2048, 4096, 8192, 16384, 32768, 65536, 131072]
        );
        assert_eq!(
            context_presets_up_to(40960),
            vec![2048, 4096, 8192, 16384, 32768, 40960]
        );
        assert_eq!(format_context_label(131072), "128K (131,072)");
        assert_eq!(format_context_label(1048576), "1M (1,048,576)");
        assert_eq!(format_short_context(10485760), "10M");
    }

    #[test]
    fn detects_vision_and_audio_capabilities_from_templates() {
        assert!(template_supports_vision("<start_of_image>describe"));
        assert!(template_supports_vision("type == 'image_url'"));
        assert!(!template_supports_vision("You are a helpful assistant."));

        assert!(template_supports_audio("<start_of_audio>transcribe"));
        assert!(template_supports_audio("<|audio|>"));
        assert!(!template_supports_audio("You are a helpful assistant."));
    }
}
