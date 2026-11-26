//! Local model discovery, hardware capability probing, and Hugging Face GGUF downloads.

pub mod gguf;
pub mod hardware;
pub mod huggingface;
pub mod registry;

use std::path::{Path, PathBuf};

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

/// Extract a quantization tag (e.g. `Q4_K_M`, `Q8_0`, `IQ4_XS`) from a GGUF filename.
pub fn infer_quantization(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".gguf").unwrap_or(filename);
    let is_quant_token = |upper: &str| {
        (upper.starts_with('Q') || upper.starts_with("IQ"))
            && upper.chars().any(|c| c.is_ascii_digit())
    };

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
pub fn detect_gguf_supports_thinking(path: &std::path::Path) -> bool {
    gguf::inspect_gguf_metadata(path).supports_thinking
}

/// Inspect the GGUF header to auto-detect the model's maximum trained context length.
pub fn detect_gguf_max_context(path: &std::path::Path) -> Option<u32> {
    gguf::inspect_gguf_metadata(path).max_context
}

/// Convert a raw GGUF filename or ID (e.g. `gemma-4-e4b-it-q4_k_m`) into a clean display name (`Gemma 4 E4B`).
pub fn friendly_model_name(raw: &str) -> String {
    let stem = raw.strip_suffix(".gguf").unwrap_or(raw);
    let quant = infer_quantization(stem);

    // Strip trailing quantization tag if present.
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
        let lower = token.to_ascii_lowercase();
        if lower.is_empty()
            || matches!(
                lower.as_str(),
                "it" | "instruct" | "chat" | "gguf" | "meta" | "qat" | "ud" | "fp16" | "bf16"
                    | "f16" | "f32"
            )
        {
            continue;
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
            _ if lower.starts_with('e') && lower.ends_with('b') && lower.len() <= 4 => {
                lower.to_ascii_uppercase()
            }
            _ if lower.ends_with('b')
                && lower[..lower.len() - 1].chars().all(|c| c.is_ascii_digit() || c == '.') =>
            {
                lower.to_ascii_uppercase()
            }
            _ => {
                let mut chars = token.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        };
        if !formatted.is_empty() {
            words.push(formatted);
        }
    }

    if words.is_empty() {
        stem.to_string()
    } else {
        words.join(" ")
    }
}

/// A GGUF model discovered on disk inside the configured models directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub display_name: String,
    pub file_path: PathBuf,
    pub mmproj_path: Option<PathBuf>,
    pub hf_repo: Option<String>,
    pub quantization: Option<String>,
    pub supports_thinking: bool,
}

fn is_gguf_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

fn is_mmproj_or_clip(path: &Path) -> bool {
    let name_has_mmproj = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("mmproj"));
    name_has_mmproj || gguf::inspect_gguf_metadata(path).is_clip
}

/// Return true for auxiliary GGUF files (such as `mtp` speculative/draft heads) that are not standalone models.
pub(crate) fn is_auxiliary_gguf_name(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.contains("mmproj")
        || lower.starts_with("mtp-")
        || lower.starts_with("mtp_")
        || lower.contains("-mtp")
        || lower.contains("_mtp")
        || lower.contains(".mtp.")
}

/// Rank `mmproj` filenames by preferred precision (`BF16` -> `F16` -> `Q8_0` -> other -> `F32`).
pub(crate) fn mmproj_precision_rank(filename: &str) -> u8 {
    let lower = filename.to_ascii_lowercase();
    if lower.contains("bf16") {
        0
    } else if lower.contains("f16") || lower.contains("fp16") {
        1
    } else if lower.contains("q8_0") {
        2
    } else if lower.contains("f32") || lower.contains("fp32") {
        4
    } else {
        3
    }
}

/// Scan `models_dir` (e.g. `~/.local/share/companion/models`) for previously downloaded
/// `.gguf` models and pair any `mmproj` projector in the same `owner--repo` folder.
pub fn discover_managed_models_in(models_dir: &Path) -> Vec<DiscoveredModel> {
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return Vec::new();
    };

    let mut discovered = Vec::new();
    let mut dirs = Vec::new();
    let mut top_level_ggufs = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if is_gguf_file(&path) && !is_mmproj_or_clip(&path) {
            top_level_ggufs.push(path);
        }
    }

    dirs.sort();
    for dir in dirs {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let hf_repo = dir_name
            .contains("--")
            .then(|| dir_name.replacen("--", "/", 1));

        let Ok(sub_entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut gguf_files: Vec<PathBuf> = sub_entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_gguf_file(p))
            .collect();
        gguf_files.sort();

        let mut mmproj_candidates = Vec::new();
        let mut model_files = Vec::new();
        for p in gguf_files {
            if is_mmproj_or_clip(&p) {
                mmproj_candidates.push(p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !is_auxiliary_gguf_name(n))
            {
                model_files.push(p);
            }
        }

        let best_mmproj = mmproj_candidates.into_iter().min_by_key(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX);
            (mmproj_precision_rank(name), size)
        });

        for path in model_files {
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("model.gguf");
            discovered.push(DiscoveredModel {
                display_name: friendly_model_name(filename),
                supports_thinking: detect_gguf_supports_thinking(&path),
                quantization: infer_quantization(filename),
                file_path: path,
                mmproj_path: best_mmproj.clone(),
                hf_repo: hf_repo.clone(),
            });
        }
    }

    top_level_ggufs.sort();
    for path in top_level_ggufs {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("model.gguf");
        discovered.push(DiscoveredModel {
            display_name: friendly_model_name(filename),
            supports_thinking: detect_gguf_supports_thinking(&path),
            quantization: infer_quantization(filename),
            file_path: path,
            mmproj_path: None,
            hf_repo: None,
        });
    }

    discovered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_friendly_model_names() {
        assert_eq!(friendly_model_name("gemma-4-e4b-it-q4_k_m"), "Gemma 4 E4B");
        assert_eq!(
            friendly_model_name("gemma-4-12B-it-qat-UD-Q4_K_XL.gguf"),
            "Gemma 4 12B"
        );
        assert_eq!(
            infer_quantization("model-Q4_K_M.gguf").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            infer_quantization("gemma-4-12B-it-qat-UD-Q4_K_XL.gguf").as_deref(),
            Some("Q4_K_XL")
        );
        assert_eq!(
            infer_quantization("llama_3_8b_q4_k_m.gguf").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(friendly_model_name("Qwen3-8B-Q4_K_M.gguf"), "Qwen 3 8B");
        assert_eq!(
            friendly_model_name("Meta-Llama-3.1-8B-Instruct-Q5_K_M"),
            "Llama 3.1 8B"
        );
    }

    #[test]
    fn discovers_managed_models_and_pairs_mmproj() {
        let dir = std::env::temp_dir().join(format!("companion_discover_{}", uuid::Uuid::new_v4()));
        let repo_dir = dir.join("google--gemma-4-E4B-it-qat-q4_0-gguf");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let model_path = repo_dir.join("gemma-4-e4b-it-q4_k_m.gguf");
        let mmproj_path = repo_dir.join("gemma-4-E4B-it-mmproj.gguf");
        let part_path = repo_dir.join("incomplete.gguf.part");
        std::fs::write(&model_path, b"dummy").unwrap();
        std::fs::write(&mmproj_path, b"dummy").unwrap();
        std::fs::write(&part_path, b"partial").unwrap();

        let found = discover_managed_models_in(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].display_name, "Gemma 4 E4B");
        assert_eq!(found[0].file_path, model_path);
        assert_eq!(found[0].mmproj_path.as_ref(), Some(&mmproj_path));
        assert_eq!(
            found[0].hf_repo.as_deref(),
            Some("google/gemma-4-E4B-it-qat-q4_0-gguf")
        );
        assert_eq!(found[0].quantization.as_deref(), Some("Q4_K_M"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
