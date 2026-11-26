//! Explicit application filesystem paths injected from the binary entry point.

use std::path::PathBuf;

/// Standard filesystem locations used by Companion's storage, model registry, and composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub db_path: PathBuf,
    pub models_dir: PathBuf,
    pub clipboard_dir: PathBuf,
}

impl AppPaths {
    /// Build standard Companion paths under a platform user-data root (e.g. XDG `data_dir`).
    pub fn from_data_dir(data_dir: PathBuf) -> Self {
        let base = data_dir.join("companion");
        Self {
            db_path: base.join("companion.db"),
            models_dir: base.join("models"),
            clipboard_dir: base.join("clipboard"),
        }
    }
}
