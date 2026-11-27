//! Explicit application filesystem paths injected from the binary entry point.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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

/// Ensure a directory exists, creating parents if needed.
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Canonicalize `path`, falling back to the original path on error.
pub fn canonicalize_or_self(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Return `(mtime_secs_since_epoch, byte_len)` for `path`, if readable.
pub fn file_mtime_and_len(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// Create `dest_dir` if needed and write `bytes` to `dest_path`.
pub fn write_bytes_in_dir(dest_dir: &Path, dest_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    std::fs::write(dest_path, bytes)
}

/// Remove orphaned clipboard temp images (`pasted-*.png`) left behind by previous runs.
pub fn cleanup_orphaned_clipboard_files(clipboard_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(clipboard_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_pasted_png = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("pasted-") && n.ends_with(".png"));
        if is_pasted_png && path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{canonicalize_or_self, ensure_dir, file_mtime_and_len, write_bytes_in_dir};

    #[test]
    fn path_helpers_create_write_stat_and_canonicalize() {
        let base = std::env::temp_dir().join("companion_core_paths_helper_test");
        let _ = std::fs::remove_dir_all(&base);
        let sub = base.join("nested");
        ensure_dir(&sub).unwrap();
        assert!(sub.is_dir());

        let file = sub.join("sample.bin");
        write_bytes_in_dir(&sub, &file, b"hello").unwrap();
        let (_mtime, len) = file_mtime_and_len(&file).unwrap();
        assert_eq!(len, 5);
        assert!(file_mtime_and_len(&sub.join("missing.bin")).is_none());

        assert!(canonicalize_or_self(file.clone()).is_absolute());
        let missing = base.join("does_not_exist");
        assert_eq!(canonicalize_or_self(missing.clone()), missing);
        let _ = std::fs::remove_dir_all(&base);
    }
}
