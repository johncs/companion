//! Parallel GGUF inventory inspection and safe managed-model file deletion.

use std::path::Path;

use crate::models::gguf::{GgufMetadata, inspect_gguf_metadata};
use crate::storage::ModelRecord;

#[derive(Clone)]
pub(super) struct InstalledModelInfo {
    pub(super) record: ModelRecord,
    pub(super) file_size: Option<u64>,
    pub(super) gguf: GgufMetadata,
    pub(super) mmproj_exists: bool,
    pub(super) mmproj_gguf: Option<GgufMetadata>,
}

pub(super) fn stat_models_parallel(models: Vec<ModelRecord>) -> Vec<InstalledModelInfo> {
    crate::core::parallel_chunks(&models, 4, |chunk| {
        chunk
            .iter()
            .map(|m| {
                let file_size = m
                    .file_path
                    .as_deref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|meta| meta.len());
                let gguf = m
                    .file_path
                    .as_deref()
                    .map(|p| inspect_gguf_metadata(Path::new(p)))
                    .unwrap_or_default();
                let mmproj_exists = m
                    .mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).exists());
                let mmproj_gguf = m
                    .mmproj_path
                    .as_deref()
                    .filter(|_| mmproj_exists)
                    .map(|p| inspect_gguf_metadata(Path::new(p)));
                InstalledModelInfo {
                    record: m.clone(),
                    file_size,
                    gguf,
                    mmproj_exists,
                    mmproj_gguf,
                }
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

pub(super) fn remove_managed_entry(managed_dir: &Path, path: &Path, other_models: &[ModelRecord]) {
    if !path.starts_with(managed_dir) {
        return;
    }
    let path_in_use = |target: &Path| {
        other_models.iter().any(|m| {
            m.file_path.as_deref().is_some_and(|p| Path::new(p) == target)
                || m.mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p) == target)
        })
    };
    if let Some(parent) = path.parent()
        && parent != managed_dir
        && parent.starts_with(managed_dir)
    {
        let dir_in_use = other_models.iter().any(|m| {
            m.file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).starts_with(parent))
                || m.mmproj_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).starts_with(parent))
        });
        if !dir_in_use {
            let _ = std::fs::remove_dir_all(parent);
            return;
        }
    }
    if !path_in_use(path) {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) fn delete_managed_model_files(
    managed_dir: &Path,
    model: &ModelRecord,
    other_models: &[ModelRecord],
) {
    if let Some(path) = &model.file_path {
        remove_managed_entry(managed_dir, Path::new(path), other_models);
    }
    if let Some(mmproj) = &model.mmproj_path {
        remove_managed_entry(managed_dir, Path::new(mmproj), other_models);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_model(id: &str, path: Option<String>) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            display_name: id.to_string(),
            provider_type: "llama_cpp".to_string(),
            file_path: path,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params: crate::core::model_params::ModelParams::default(),
            avg_tokens_per_sec: None,
        }
    }

    #[test]
    fn stat_models_parallel_reports_existing_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("companion_mm_stat_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut models = Vec::new();
        for i in 0..5 {
            let p = dir.join(format!("m{i}.gguf"));
            std::fs::write(&p, vec![7u8; (i + 1) * 10]).unwrap();
            models.push(dummy_model(&format!("m{i}"), Some(p.to_string_lossy().to_string())));
        }
        models.push(dummy_model(
            "missing",
            Some(dir.join("gone.gguf").to_string_lossy().to_string()),
        ));

        let results = stat_models_parallel(models);
        assert_eq!(results.len(), 6);
        assert_eq!(results[0].file_size, Some(10));
        assert_eq!(results[4].file_size, Some(50));
        assert_eq!(results[5].file_size, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_managed_model_removes_repo_directory_and_vision_files() {
        let dir = std::env::temp_dir().join(format!("companion_mm_del_{}", uuid::Uuid::new_v4()));
        let repo_dir = dir.join("google--gemma-4-E4B");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let model_path = repo_dir.join("model.gguf");
        let mmproj_path = repo_dir.join("mmproj-BF16.gguf");
        std::fs::write(&model_path, b"model").unwrap();
        std::fs::write(&mmproj_path, b"vision").unwrap();

        let mut model = dummy_model("m1", Some(model_path.to_string_lossy().to_string()));
        model.mmproj_path = Some(mmproj_path.to_string_lossy().to_string());

        delete_managed_model_files(&dir, &model, &[]);
        assert!(!repo_dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_managed_model_preserves_shared_repo_dir_and_mmproj() {
        let dir =
            std::env::temp_dir().join(format!("companion_mm_shared_{}", uuid::Uuid::new_v4()));
        let repo_dir = dir.join("org--shared-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let q4 = repo_dir.join("model-Q4.gguf");
        let q8 = repo_dir.join("model-Q8.gguf");
        let mmproj = repo_dir.join("mmproj-BF16.gguf");
        std::fs::write(&q4, b"q4").unwrap();
        std::fs::write(&q8, b"q8").unwrap();
        std::fs::write(&mmproj, b"vision").unwrap();

        let mut m1 = dummy_model("m1", Some(q4.to_string_lossy().to_string()));
        m1.mmproj_path = Some(mmproj.to_string_lossy().to_string());
        let mut m2 = dummy_model("m2", Some(q8.to_string_lossy().to_string()));
        m2.mmproj_path = Some(mmproj.to_string_lossy().to_string());

        delete_managed_model_files(&dir, &m1, &[m2]);
        assert!(repo_dir.exists());
        assert!(!q4.exists());
        assert!(q8.exists());
        assert!(mmproj.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
