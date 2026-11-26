//! Local model discovery, hardware capability probing, and Hugging Face GGUF downloads.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod discovery;
pub mod download_manager;
pub mod gguf;
pub mod hardware;
pub mod huggingface;
pub mod inventory;
pub mod naming;
pub mod registry;

pub use discovery::{
    DiscoveredModel, discover_managed_models_in, discover_managed_models_with_cache,
};
pub(crate) use discovery::{is_auxiliary_gguf_name, mmproj_precision_rank};
pub use naming::{
    detect_gguf_max_context, detect_gguf_supports_thinking, format_bytes, friendly_model_name,
    infer_quantization, infer_supports_thinking, strip_gguf_suffix, template_supports_thinking,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_friendly_model_names_and_quantizations() {
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
        assert_eq!(
            infer_quantization("Qwen3-8B-BF16.gguf").as_deref(),
            Some("BF16")
        );
        assert_eq!(infer_quantization("Qwen3-8B.gguf"), None);
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
