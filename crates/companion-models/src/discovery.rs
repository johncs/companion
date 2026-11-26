//! Local GGUF model discovery and `mmproj` pairing inside the managed models directory.

use std::path::{Path, PathBuf};

use crate::gguf::GgufMetadataCache;
use crate::naming::{friendly_model_name, infer_quantization};

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

fn is_mmproj_or_clip(path: &Path, cache: &GgufMetadataCache) -> bool {
    let name_has_mmproj = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().contains("mmproj"));
    name_has_mmproj || cache.inspect(path).is_clip
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

/// Scan `models_dir` using a shared [`GgufMetadataCache`] so each GGUF header is parsed once.
pub fn discover_managed_models_with_cache(
    models_dir: &Path,
    cache: &GgufMetadataCache,
) -> Vec<DiscoveredModel> {
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
        } else if is_gguf_file(&path)
            && !is_mmproj_or_clip(&path, cache)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !is_auxiliary_gguf_name(n))
        {
            top_level_ggufs.push(path);
        }
    }

    dirs.sort();
    for dir in dirs {
        scan_repo_directory(&dir, cache, &mut discovered);
    }

    top_level_ggufs.sort();
    for path in top_level_ggufs {
        discovered.push(build_discovered_model(path, None, None, cache));
    }

    discovered
}

/// Scan `models_dir` for previously downloaded `.gguf` models and pair any `mmproj` projector.
pub fn discover_managed_models_in(models_dir: &Path) -> Vec<DiscoveredModel> {
    let cache = GgufMetadataCache::new();
    discover_managed_models_with_cache(models_dir, &cache)
}

fn scan_repo_directory(
    dir: &Path,
    cache: &GgufMetadataCache,
    discovered: &mut Vec<DiscoveredModel>,
) {
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let hf_repo = dir_name
        .contains("--")
        .then(|| dir_name.replacen("--", "/", 1));

    let Ok(sub_entries) = std::fs::read_dir(dir) else {
        return;
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
        if is_mmproj_or_clip(&p, cache) {
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
        discovered.push(build_discovered_model(
            path,
            best_mmproj.clone(),
            hf_repo.clone(),
            cache,
        ));
    }
}

fn build_discovered_model(
    path: PathBuf,
    mmproj_path: Option<PathBuf>,
    hf_repo: Option<String>,
    cache: &GgufMetadataCache,
) -> DiscoveredModel {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model.gguf");
    DiscoveredModel {
        display_name: friendly_model_name(filename),
        supports_thinking: cache.inspect(&path).supports_thinking,
        quantization: infer_quantization(filename),
        file_path: path,
        mmproj_path,
        hf_repo,
    }
}
