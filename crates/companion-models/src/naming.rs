//! Model filename parsing, quantization inference, and UI-friendly display formatting.

use std::path::Path;

const STANDARD_CONTEXT_PRESETS: &[u32] = &[
    2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576, 2097152, 4194304,
    8388608, 10485760,
];

/// Strip a `.gguf` extension case-insensitively.
pub fn strip_gguf_suffix(name: &str) -> &str {
    if name.len() >= 5 && name[name.len() - 5..].eq_ignore_ascii_case(".gguf") {
        &name[..name.len() - 5]
    } else {
        name
    }
}

/// Format a byte count into a human-readable string (e.g., `4.37 GB`).
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
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
        format!("{}K", (tokens + 512) / 1024)
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

fn is_quant_token(upper: &str) -> bool {
    if matches!(upper, "F16" | "F32" | "BF16" | "FP16" | "MXFP4") {
        return true;
    }
    let rest = if let Some(r) = upper.strip_prefix("IQ") {
        r
    } else if let Some(r) = upper.strip_prefix('Q') {
        r
    } else {
        return false;
    };
    rest.as_bytes().first().is_some_and(u8::is_ascii_digit)
}

/// Extract a quantization tag (e.g. `Q4_K_M`, `Q8_0`, `IQ4_XS`, `BF16`) from a GGUF filename.
pub fn infer_quantization(filename: &str) -> Option<String> {
    let stem = strip_gguf_suffix(filename);

    for token in stem.split(['-', '.']).rev() {
        let upper = token.to_ascii_uppercase();
        if is_quant_token(&upper) {
            return Some(upper);
        }
    }

    let parts: Vec<&str> = stem.split('_').collect();
    for (idx, token) in parts.iter().enumerate().rev() {
        let upper = token.to_ascii_uppercase();
        if is_quant_token(&upper) {
            return Some(parts[idx..].join("_").to_ascii_uppercase());
        }
    }

    None
}

/// Heuristic for whether a model family supports configurable thinking/reasoning blocks.
pub fn infer_supports_thinking(name_or_repo: &str) -> bool {
    let lower = name_or_repo.to_ascii_lowercase();
    lower.contains("qwen3")
        || lower.contains("deepseek-r1")
        || lower.contains("qwq")
        || lower.contains("gemma-4")
        || lower.contains("gemma4")
        || lower.contains("phi-4-reasoning")
        || lower.contains("gpt-oss")
        || lower.contains("thinking")
        || lower.contains("reasoning")
}

/// Check whether a GGUF `tokenizer.chat_template` string contains reasoning/thinking support.
pub fn template_supports_thinking(template: &str) -> bool {
    template.contains("<|think|>")
        || template.contains("<|channel>thought")
        || template.contains("<think>")
        || template.contains("enable_thinking")
        || template.contains("<thought>")
        || template.contains("/no_think")
}

/// Inspect the GGUF header to auto-detect thinking support without loading weights.
pub fn detect_gguf_supports_thinking(path: &Path) -> bool {
    crate::gguf::inspect_gguf_metadata(path).supports_thinking
}

/// Inspect the GGUF header to auto-detect the model's maximum trained context length.
pub fn detect_gguf_max_context(path: &Path) -> Option<u32> {
    crate::gguf::inspect_gguf_metadata(path).max_context
}

fn is_param_size_token(lower: &str) -> bool {
    if lower.starts_with('e') && lower.ends_with('b') && lower.len() >= 3 && lower.len() <= 4 {
        return lower[1..lower.len() - 1]
            .chars()
            .all(|c| c.is_ascii_digit());
    }
    let Some(num) = lower.strip_suffix('b') else {
        return false;
    };
    !num.is_empty()
        && !num.starts_with('.')
        && !num.ends_with('.')
        && num.chars().any(|c| c.is_ascii_digit())
        && num.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Convert a raw GGUF filename or ID (e.g. `gemma-4-e4b-it-q4_k_m`) into a clean display name (`Gemma 4 E4B`).
pub fn friendly_model_name(raw: &str) -> String {
    let stem = strip_gguf_suffix(raw);
    let quant = infer_quantization(stem);

    let without_quant = if let Some(q) = &quant {
        let lower_stem = stem.to_ascii_lowercase();
        let lower_q = q.to_ascii_lowercase();
        if let Some(pos) = lower_stem.rfind(&lower_q) {
            stem[..pos].trim_end_matches(['-', '_', '.'])
        } else {
            stem
        }
    } else {
        stem
    };

    let mut words = Vec::new();
    for token in without_quant.split(['-', '_']) {
        if let Some(formatted) = format_model_name_token(token) {
            words.push(formatted);
        }
    }

    if words.is_empty() {
        stem.to_string()
    } else {
        words.join(" ")
    }
}

fn format_model_name_token(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    if lower.is_empty()
        || matches!(
            lower.as_str(),
            "it" | "instruct"
                | "chat"
                | "gguf"
                | "meta"
                | "qat"
                | "ud"
                | "fp16"
                | "bf16"
                | "f16"
                | "f32"
        )
    {
        return None;
    }
    let formatted = match lower.as_str() {
        "gemma" => "Gemma".to_string(),
        "qwen" => "Qwen".to_string(),
        "qwen3" => "Qwen 3".to_string(),
        "qwen2.5" => "Qwen 2.5".to_string(),
        "llama" => "Llama".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "mistral" => "Mistral".to_string(),
        "phi" => "Phi".to_string(),
        _ if is_param_size_token(&lower) => lower.to_ascii_uppercase(),
        _ => {
            let mut chars = token.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    };
    (!formatted.is_empty()).then_some(formatted)
}
