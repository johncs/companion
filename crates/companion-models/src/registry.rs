//! Local GGUF model registration and disk-to-SQLite synchronization service.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use companion_core::model_params::ModelParams;
use crate::huggingface::RECOMMENDED_STARTER_REPO;
use crate::{
    detect_gguf_supports_thinking, discover_managed_models_in, friendly_model_name,
    infer_quantization,
};
use companion_storage::{Database, ModelRecord};

/// Coordinates filesystem GGUF discovery with the SQLite model registry.
pub struct ModelRegistryService;

impl ModelRegistryService {
    /// Register a local `.gguf` model in SQLite (used by HF downloads, local imports, and recovery).
    pub fn register_local_gguf(
        db: &Database,
        display_name: Option<String>,
        file_path: &Path,
        mmproj_path: Option<&Path>,
        hf_repo: Option<String>,
    ) -> Result<ModelRecord> {
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("model.gguf");
        let name = display_name.unwrap_or_else(|| friendly_model_name(filename));
        let is_starter = hf_repo.as_deref() == Some(RECOMMENDED_STARTER_REPO);
        let default_params = if is_starter {
            ModelParams {
                n_gpu_layers: 0,
                ..ModelParams::default()
            }
        } else {
            ModelParams::default()
        };
        let record = ModelRecord {
            id: Uuid::new_v4().to_string(),
            display_name: name,
            provider_type: "llama_cpp".to_string(),
            file_path: Some(file_path.to_string_lossy().to_string()),
            mmproj_path: mmproj_path.map(|p| p.to_string_lossy().to_string()),
            hf_repo,
            quantization: infer_quantization(filename),
            supports_thinking: detect_gguf_supports_thinking(file_path),
            default_params,
            avg_tokens_per_sec: None,
        };
        db.upsert_model(&record)?;
        Ok(record)
    }

    /// Synchronize the `models` table with `models_dir`:
    /// 1. Prune any model record whose `file_path` is missing on disk.
    /// 2. Register any `.gguf` model found in `models_dir` that is not yet in SQLite.
    /// 3. Ensure `active_model_id` always points to an existing registered model (or `None`).
    pub fn sync_managed_models(db: &Database, models_dir: &Path) -> Result<()> {
        let mut valid_models = Vec::new();
        let mut seen_paths = HashSet::new();

        for m in db.list_models()? {
            let exists = m
                .file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).exists());
            if exists {
                if let Some(p) = &m.file_path {
                    seen_paths.insert(p.clone());
                }
                valid_models.push(m);
            } else {
                db.delete_model(&m.id)?;
            }
        }

        for item in discover_managed_models_in(models_dir) {
            let path_str = item.file_path.to_string_lossy().to_string();
            if !seen_paths.contains(&path_str) {
                let record = Self::register_local_gguf(
                    db,
                    Some(item.display_name),
                    &item.file_path,
                    item.mmproj_path.as_deref(),
                    item.hf_repo,
                )?;
                seen_paths.insert(path_str);
                valid_models.push(record);
            }
        }

        let active_id = db.get_active_model_id()?;
        let active_is_valid = active_id
            .as_deref()
            .is_some_and(|aid| valid_models.iter().any(|m| m.id == aid));
        if !active_is_valid {
            let fallback = valid_models.first().map(|m| m.id.as_str());
            db.set_active_model_id(fallback)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_managed_models_handles_all_cases_and_active_invariant() {
        let dir = std::env::temp_dir().join(format!("companion_sync_{}", uuid::Uuid::new_v4()));
        let models_dir = dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let db = Database::open(&dir.join("test.db")).expect("open test db");

        // 1. Empty DB + empty directory -> stays empty, active_model_id is None.
        ModelRegistryService::sync_managed_models(&db, &models_dir).unwrap();
        assert!(db.list_models().unwrap().is_empty());
        assert_eq!(db.get_active_model_id().unwrap(), None);

        // 2. Empty DB + model in directory -> recovers model and selects it as active.
        let repo_dir = models_dir.join("google--gemma-4-E2B-it-qat-q4_0-gguf");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let model_path = repo_dir.join("gemma-4-e2b-it-q4_0.gguf");
        std::fs::write(&model_path, b"dummy").unwrap();
        std::fs::write(repo_dir.join("gemma-4-E2B-it-mmproj.gguf"), b"dummy").unwrap();

        ModelRegistryService::sync_managed_models(&db, &models_dir).unwrap();
        let models = db.list_models().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            db.get_active_model_id().unwrap().as_deref(),
            Some(models[0].id.as_str())
        );

        // 3. Model in DB + same file on disk -> preserves record and active selection.
        db.record_model_speed(&models[0].id, 42.0).unwrap();
        ModelRegistryService::sync_managed_models(&db, &models_dir).unwrap();
        let kept = db.list_models().unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, models[0].id);
        assert_eq!(kept[0].avg_tokens_per_sec, Some(42.0));

        // 4. Model in DB + file deleted from directory -> prunes DB and clears active_model_id.
        std::fs::remove_dir_all(&repo_dir).unwrap();
        ModelRegistryService::sync_managed_models(&db, &models_dir).unwrap();
        assert!(db.list_models().unwrap().is_empty());
        assert_eq!(db.get_active_model_id().unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starter_model_defaults_to_cpu_mode() {
        let dir = std::env::temp_dir().join(format!("companion_starter_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("test.db")).expect("open test db");
        let dummy_gguf = dir.join("model.gguf");
        std::fs::write(&dummy_gguf, b"GGUF").unwrap();

        let starter = ModelRegistryService::register_local_gguf(
            &db,
            None,
            &dummy_gguf,
            None,
            Some(RECOMMENDED_STARTER_REPO.to_string()),
        )
        .unwrap();
        assert_eq!(starter.default_params.n_gpu_layers, 0);

        let other = ModelRegistryService::register_local_gguf(
            &db,
            None,
            &dummy_gguf,
            None,
            Some("other/model".to_string()),
        )
        .unwrap();
        assert_eq!(other.default_params, ModelParams::default());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
