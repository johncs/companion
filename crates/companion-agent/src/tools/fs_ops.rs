//! Filesystem operations jailed inside a resolved granted folder.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::path_guard::resolve_granted_path;

pub(super) const MAX_LIST_ENTRIES: usize = 200;
const MAX_SEARCH_HITS: usize = 50;
const MAX_READ_BYTES: usize = 100 * 1024;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".flatpak-builder",
    "dist",
    "build",
];

/// List files in `granted_root` (bounded, skipping `.git`, `target`, `node_modules`, and `.gitignore`).
pub fn list_granted_files(
    granted_root: &Path,
    subdir: &str,
    max_entries: usize,
) -> Result<Vec<String>> {
    let start = resolve_granted_path(granted_root, subdir)?;
    let root_canon = fs::canonicalize(granted_root)?;
    let ignored = load_gitignore_names(&root_canon);
    let mut out = Vec::new();
    collect_files(
        &root_canon,
        &start,
        &ignored,
        max_entries.min(MAX_LIST_ENTRIES),
        &mut out,
    );
    out.sort();
    Ok(out)
}

/// Read a UTF-8 file inside `granted_root` (capped at 100 KB).
pub fn read_granted_file(granted_root: &Path, rel_path: &str) -> Result<String> {
    let target = resolve_granted_path(granted_root, rel_path)?;
    let bytes = fs::read(&target).with_context(|| format!("Failed to read {rel_path}"))?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        bail!("'{rel_path}' appears to be a binary file");
    }
    if bytes.len() > MAX_READ_BYTES {
        let mut s = String::from_utf8_lossy(&bytes[..MAX_READ_BYTES]).to_string();
        s.push_str("\n\n[…truncated at 100 KB…]");
        Ok(s)
    } else {
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }
}

/// Search for `query` across text files in `granted_root`.
pub fn search_granted_files(granted_root: &Path, query: &str) -> Result<Vec<String>> {
    let q = query.trim();
    if q.is_empty() {
        bail!("Search query cannot be empty");
    }
    let q_lower = q.to_ascii_lowercase();
    let files = list_granted_files(granted_root, ".", MAX_LIST_ENTRIES)?;
    let mut hits = Vec::new();

    for rel in files {
        if hits.len() >= MAX_SEARCH_HITS {
            break;
        }
        let Ok(target) = resolve_granted_path(granted_root, &rel) else {
            continue;
        };
        let Ok(bytes) = fs::read(&target) else {
            continue;
        };
        if bytes.len() > MAX_READ_BYTES || bytes.iter().take(2048).any(|&b| b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (idx, line) in text.lines().enumerate() {
            if line.to_ascii_lowercase().contains(&q_lower) {
                hits.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
                if hits.len() >= MAX_SEARCH_HITS {
                    break;
                }
            }
        }
    }

    Ok(hits)
}

/// Create or overwrite `rel_path` inside `granted_root`.
pub fn write_granted_file(granted_root: &Path, rel_path: &str, content: &str) -> Result<String> {
    let target = resolve_granted_path(granted_root, rel_path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, content.as_bytes())
        .with_context(|| format!("Failed to write {rel_path}"))?;
    Ok(format!("Wrote {} bytes to {rel_path}", content.len()))
}

/// Delete a single file inside `granted_root` (directories are not deleted).
pub fn delete_granted_file(granted_root: &Path, rel_path: &str) -> Result<String> {
    let target = resolve_granted_path(granted_root, rel_path)?;
    if !target.exists() {
        bail!("File does not exist: {rel_path}");
    }
    if target.is_dir() {
        bail!("Cannot delete a directory with delete_file: {rel_path}");
    }
    fs::remove_file(&target).with_context(|| format!("Failed to delete {rel_path}"))?;
    Ok(format!("Deleted {rel_path}"))
}

/// Create a directory (and any missing parents) inside `granted_root`.
pub fn create_granted_directory(granted_root: &Path, rel_path: &str) -> Result<String> {
    let target = resolve_granted_path(granted_root, rel_path)?;
    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory {rel_path}"))?;
    Ok(format!("Created directory {rel_path}"))
}

/// Rename or move a file or directory strictly within `granted_root`.
pub fn move_granted_path(granted_root: &Path, from_rel: &str, to_rel: &str) -> Result<String> {
    let root_canon = fs::canonicalize(granted_root)?;
    let src = resolve_granted_path(granted_root, from_rel)?;
    if !src.exists() {
        bail!("Source path does not exist: {from_rel}");
    }
    if src == root_canon {
        bail!("Cannot move the granted root folder itself");
    }
    let dst = resolve_granted_path(granted_root, to_rel)?;
    if dst.exists() {
        bail!("Destination already exists: {to_rel}");
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&src, &dst).with_context(|| format!("Failed to move {from_rel} to {to_rel}"))?;
    Ok(format!("Moved {from_rel} to {to_rel}"))
}

/// Recursively delete a subdirectory inside `granted_root` (refuses to delete `granted_root` itself).
pub fn delete_granted_directory(granted_root: &Path, rel_path: &str) -> Result<String> {
    let root_canon = fs::canonicalize(granted_root)?;
    let target = resolve_granted_path(granted_root, rel_path)?;
    if !target.exists() {
        bail!("Directory does not exist: {rel_path}");
    }
    if !target.is_dir() {
        bail!("Path is not a directory: {rel_path}");
    }
    if target == root_canon {
        bail!("Cannot delete the granted root directory itself");
    }
    fs::remove_dir_all(&target)
        .with_context(|| format!("Failed to delete directory {rel_path}"))?;
    Ok(format!("Deleted directory {rel_path}"))
}

fn load_gitignore_names(root: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(text) = fs::read_to_string(root.join(".gitignore")) {
        for line in text.lines() {
            let trimmed = line.trim().trim_start_matches('/').trim_end_matches('/');
            if !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.contains('*') {
                set.insert(trimmed.to_string());
            }
        }
    }
    set
}

fn collect_files(
    root_canon: &Path,
    dir: &Path,
    ignored: &HashSet<String>,
    max_entries: usize,
    out: &mut Vec<String>,
) {
    if out.len() >= max_entries {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= max_entries {
            break;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if SKIP_DIRS.contains(&name) || ignored.contains(name) {
            continue;
        }
        if path.is_dir() {
            collect_files(root_canon, &path, ignored, max_entries, out);
        } else if let Ok(rel) = path.strip_prefix(root_canon) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}
