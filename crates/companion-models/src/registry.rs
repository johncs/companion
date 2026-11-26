//! Local GGUF model registration and disk-to-SQLite synchronization service.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use uuid::Uuid;

use crate::gguf::GgufMetadataCache;
use crate::huggingface::RECOMMENDED_STARTER_REPO;
use crate::{discover_managed_models_with_cache, friendly_model_name, infer_quantization};
use companion_core::model_params::ModelParams;
use companion_storage::{Database, ModelRecord};

/// Coordinates filesystem GGUF discovery with the SQLite model registry.
pub struct ModelRegistryService;

fn canonical_or_owned(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl ModelRegistryService {
    /// Register (or update by canonical `file_path`) a local `.gguf` model in SQLite.
    pub fn register_local_gguf(
        db: &Database,
        display_name: Option<String>,
        file_path: &Path,
        mmproj_path: Option<&Path>,
        hf_repo: Option<String>,
    ) -> Result<ModelRecord> {
        let cache = GgufMetadataCache::new();
        Self::register_local_gguf_with_cache(
            db,
            display_name,
            file_path,
            mmproj_path,
            hf_repo,
            &cache,
        )
    }

    /// Register or update a local `.gguf` model using a shared [`GgufMetadataCache`].
    pub fn register_local_gguf_with_cache(
        db: &Database,
        display_name: Option<String>,
        file_path: &Path,
        mmproj_path: Option<&Path>,
        hf_repo: Option<String>,
        cache: &GgufMetadataCache,
    ) -> Result<ModelRecord> {
        let target_canon = canonical_or_owned(file_path);
        let existing = db.list_models()?.into_iter().find(|m| {
            m.file_path
                .as_deref()
                .is_some_and(|p| canonical_or_owned(Path::new(p)) == target_canon)
        });

        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("model.gguf");
        let name = display_name.unwrap_or_else(|| friendly_model_name(filename));
        let is_starter = hf_repo.as_deref() == Some(RECOMMENDED_STARTER_REPO);

        let record = if let Some(mut rec) = existing {
            if rec.mmproj_path.is_none() {
                rec.mmproj_path = mmproj_path.map(|p| p.to_string_lossy().to_string());
            }
            if rec.hf_repo.is_none() {
                rec.hf_repo = hf_repo;
            }
            rec
        } else {
            build_new_gguf_record(NewGgufRecordArgs {
                name,
                filename,
                file_path,
                mmproj_path,
                hf_repo,
                is_starter,
                cache,
            })
        };
        db.upsert_model(&record)?;
        Ok(record)
    }

    /// Synchronize the `models` table with `models_dir` using a shared metadata cache.
    pub fn sync_managed_models(db: &Database, models_dir: &Path) -> Result<()> {
        let cache = GgufMetadataCache::new();
        Self::sync_managed_models_with_cache(db, models_dir, &cache)
    }

    /// Synchronize the `models` table with `models_dir` using a shared [`GgufMetadataCache`]:
    /// 1. Prune missing `file_path` records and stale `mmproj_path` values.
    /// 2. Auto-pair newly present `mmproj` files and register undiscovered `.gguf` models.
    /// 3. Ensure `active_model_id` always points to an existing registered model (or `None`).
    pub fn sync_managed_models_with_cache(
        db: &Database,
        models_dir: &Path,
        cache: &GgufMetadataCache,
    ) -> Result<()> {
        let discovered = discover_managed_models_with_cache(models_dir, cache);
        let discovered_by_path: HashMap<PathBuf, _> = discovered
            .iter()
            .map(|d| (canonical_or_owned(&d.file_path), d))
            .collect();

        let (mut valid_models, mut seen_paths) =
            reconcile_existing_models(db, &discovered_by_path)?;

        for item in discovered {
            let canon = canonical_or_owned(&item.file_path);
            if !seen_paths.contains(&canon) {
                let record = Self::register_local_gguf_with_cache(
                    db,
                    Some(item.display_name),
                    &item.file_path,
                    item.mmproj_path.as_deref(),
                    item.hf_repo,
                    cache,
                )?;
                seen_paths.insert(canon);
                valid_models.push(record);
            }
        }

        ensure_valid_active_model(db, &valid_models)?;
        Ok(())
    }
}

struct NewGgufRecordArgs<'a> {
    name: String,
    filename: &'a str,
    file_path: &'a Path,
    mmproj_path: Option<&'a Path>,
    hf_repo: Option<String>,
    is_starter: bool,
    cache: &'a GgufMetadataCache,
}

fn build_new_gguf_record(args: NewGgufRecordArgs<'_>) -> ModelRecord {
    let default_params = if args.is_starter {
        ModelParams {
            n_gpu_layers: 0,
            ..ModelParams::default()
        }
    } else {
        ModelParams::default()
    };
    ModelRecord {
        id: Uuid::new_v4().to_string(),
        display_name: args.name,
        provider_type: companion_core::types::ModelProviderType::LlamaCpp,
        file_path: Some(args.file_path.to_string_lossy().to_string()),
        mmproj_path: args.mmproj_path.map(|p| p.to_string_lossy().to_string()),
        hf_repo: args.hf_repo,
        quantization: infer_quantization(args.filename),
        supports_thinking: args.cache.inspect(args.file_path).supports_thinking,
        default_params,
        avg_tokens_per_sec: None,
    }
}

fn reconcile_existing_models(
    db: &Database,
    discovered_by_path: &HashMap<PathBuf, &crate::discovery::DiscoveredModel>,
) -> Result<(Vec<ModelRecord>, HashSet<PathBuf>)> {
    let mut valid_models = Vec::new();
    let mut seen_paths = HashSet::new();

    for mut m in db.list_models()? {
        let Some(ref raw_path) = m.file_path else {
            db.delete_model(&m.id)?;
            continue;
        };
        let path = Path::new(raw_path);
        if !path.exists() {
            db.delete_model(&m.id)?;
            continue;
        }
        let canon = canonical_or_owned(path);
        let mut changed = false;
        if let Some(ref mp) = m.mmproj_path
            && !Path::new(mp).exists()
        {
            m.mmproj_path = None;
            changed = true;
        }
        if m.mmproj_path.is_none()
            && let Some(disc) = discovered_by_path.get(&canon)
            && let Some(ref new_mp) = disc.mmproj_path
        {
            m.mmproj_path = Some(new_mp.to_string_lossy().to_string());
            changed = true;
        }
        if changed {
            db.upsert_model(&m)?;
        }
        seen_paths.insert(canon);
        valid_models.push(m);
    }

    Ok((valid_models, seen_paths))
}

fn ensure_valid_active_model(db: &Database, valid_models: &[ModelRecord]) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_managed_models_handles_all_cases_and_active_invariant() {
        let dir = std::env::temp_dir().join(format!("companion_sync_{}", uuid::Uuid::new_v4()));
        let models_dir = dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let db = Database::open(dir.join("test.db")).expect("open test db");

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
        db.update_model_speed(&models[0].id, 42.0, 1).unwrap();
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
    fn starter_model_defaults_to_cpu_and_deduplicates_and_prunes_stale_mmproj() {
        let dir = std::env::temp_dir().join(format!("companion_starter_{}", uuid::Uuid::new_v4()));
        let models_dir = dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let db = Database::open(dir.join("test.db")).expect("open test db");
        let starter_gguf = models_dir.join("starter.gguf");
        let other_gguf = models_dir.join("other.gguf");
        let mmproj_gguf = models_dir.join("mmproj-gone.gguf");
        std::fs::write(&starter_gguf, b"GGUF").unwrap();
        std::fs::write(&other_gguf, b"GGUF").unwrap();
        std::fs::write(&mmproj_gguf, b"GGUF").unwrap();

        let starter = ModelRegistryService::register_local_gguf(
            &db,
            None,
            &starter_gguf,
            Some(&mmproj_gguf),
            Some(RECOMMENDED_STARTER_REPO.to_string()),
        )
        .unwrap();
        assert_eq!(starter.default_params.n_gpu_layers, 0);

        // Re-registering the same path deduplicates instead of creating a second row.
        let dup = ModelRegistryService::register_local_gguf(
            &db,
            None,
            &starter_gguf,
            None,
            Some(RECOMMENDED_STARTER_REPO.to_string()),
        )
        .unwrap();
        assert_eq!(dup.id, starter.id);

        // Removing mmproj on disk prunes stale mmproj_path on sync.
        std::fs::remove_file(&mmproj_gguf).unwrap();
        ModelRegistryService::sync_managed_models(&db, &models_dir).unwrap();
        let refreshed = db.get_model(&starter.id).unwrap().unwrap();
        assert_eq!(refreshed.mmproj_path, None);

        let other = ModelRegistryService::register_local_gguf(
            &db,
            None,
            &other_gguf,
            None,
            Some("other/model".to_string()),
        )
        .unwrap();
        assert_eq!(other.default_params, ModelParams::default());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
