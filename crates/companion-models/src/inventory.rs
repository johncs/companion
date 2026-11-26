//! Parallel GGUF inventory inspection, directory sizing, and safe managed-model deletion.

use std::path::{Path, PathBuf};

use crate::gguf::{GgufMetadata, inspect_gguf_metadata};
use companion_storage::ModelRecord;

/// Snapshot of a registered model enriched with on-disk file sizes and GGUF metadata.
#[derive(Clone)]
pub struct InstalledModelInfo {
    pub record: ModelRecord,
    pub file_size: Option<u64>,
    pub gguf: GgufMetadata,
    pub mmproj_exists: bool,
    pub mmproj_gguf: Option<GgufMetadata>,
}

/// Inspect GGUF metadata and file sizes for `models` in parallel across worker threads.
pub fn stat_models_parallel(models: Vec<ModelRecord>) -> Vec<InstalledModelInfo> {
    companion_core::parallel_chunks(&models, 4, |chunk| {
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

fn remove_managed_entry(managed_dir: &Path, path: &Path, other_models: &[ModelRecord]) {
    if !path.starts_with(managed_dir) {
        return;
    }
    let path_in_use = |target: &Path| {
        other_models.iter().any(|m| {
            m.file_path
                .as_deref()
                .is_some_and(|p| Path::new(p) == target)
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

/// Safely delete managed model/mmproj files on disk unless still referenced by `other_models`.
pub fn delete_managed_model_files(
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

/// Compute the total byte size of all files under `root` in parallel across top-level entries.
pub fn compute_dir_size_parallel(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    if entries.is_empty() {
        return 0;
    }

    let has_subdirs = entries
        .iter()
        .any(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false));
    let min_parallel = if has_subdirs { 1 } else { 16 };
    companion_core::parallel_chunks(&entries, min_parallel, sum_entries_iterative)
        .into_iter()
        .sum()
}

fn visit_dir_entry(entry: &std::fs::DirEntry, dir_stack: &mut Vec<PathBuf>) -> u64 {
    let Ok(ft) = entry.file_type() else {
        return 0;
    };
    if ft.is_file() {
        entry.metadata().map(|m| m.len()).unwrap_or(0)
    } else if ft.is_dir() {
        dir_stack.push(entry.path());
        0
    } else {
        0
    }
}

fn sum_entries_iterative(entries: &[std::fs::DirEntry]) -> u64 {
    let mut total = 0u64;
    let mut dir_stack: Vec<PathBuf> = Vec::new();

    for entry in entries {
        total = total.saturating_add(visit_dir_entry(entry, &mut dir_stack));
    }

    while let Some(dir) = dir_stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            total = total.saturating_add(visit_dir_entry(&entry, &mut dir_stack));
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_core::model_params::ModelParams;
    use companion_core::types::ModelProviderType;

    fn dummy_model(id: &str, path: Option<String>) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            display_name: id.to_string(),
            provider_type: ModelProviderType::LlamaCpp,
            file_path: path,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params: ModelParams::default(),
            avg_tokens_per_sec: None,
        }
    }

    #[test]
    fn stat_models_parallel_reports_existing_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("companion_inv_stat_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let real_file = dir.join("model.gguf");
        std::fs::write(&real_file, vec![0u8; 64]).unwrap();

        let models = vec![
            dummy_model("m1", Some(real_file.to_string_lossy().to_string())),
            dummy_model(
                "m2",
                Some(dir.join("missing.gguf").to_string_lossy().into()),
            ),
            dummy_model("m3", None),
        ];

        let results = stat_models_parallel(models);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file_size, Some(64));
        assert_eq!(results[1].file_size, None);
        assert_eq!(results[2].file_size, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_managed_model_files_removes_unshared_and_preserves_shared_and_external() {
        let dir = std::env::temp_dir().join(format!("companion_inv_del_{}", uuid::Uuid::new_v4()));
        let repo_a = dir.join("repo_a");
        std::fs::create_dir_all(&repo_a).unwrap();
        let file_a = repo_a.join("a.gguf");
        std::fs::write(&file_a, b"a").unwrap();

        let model = dummy_model("a", Some(file_a.to_string_lossy().into()));
        delete_managed_model_files(&dir, &model, &[]);
        assert!(!repo_a.exists());

        let repo_b = dir.join("repo_b");
        std::fs::create_dir_all(&repo_b).unwrap();
        let b1 = repo_b.join("q4.gguf");
        let b2 = repo_b.join("q8.gguf");
        std::fs::write(&b1, b"1").unwrap();
        std::fs::write(&b2, b"2").unwrap();
        let m1 = dummy_model("b1", Some(b1.to_string_lossy().into()));
        let m2 = dummy_model("b2", Some(b2.to_string_lossy().into()));

        delete_managed_model_files(&dir, &m1, &[m2]);
        assert!(!b1.exists());
        assert!(b2.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_dir_size_handles_nested_and_flat_dirs() {
        let dir = std::env::temp_dir().join(format!("companion_pref_dir_{}", uuid::Uuid::new_v4()));
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        for i in 0..20 {
            std::fs::write(dir.join(format!("file_{i}.bin")), vec![0u8; 10]).unwrap();
        }
        std::fs::write(sub.join("deep.bin"), vec![0u8; 55]).unwrap();

        assert_eq!(compute_dir_size_parallel(&dir), 255);
        assert_eq!(compute_dir_size_parallel(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
