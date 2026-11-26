//! Path-jailing, grant resolution, and write-permission checks for local agent tools.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct GrantedFolder {
    pub path: PathBuf,
    pub allow_write: bool,
}

/// Resolve `rel_path` strictly inside `granted_root`, rejecting any `..` or outside symlink escape.
pub fn resolve_granted_path(granted_root: &Path, rel_path: &str) -> Result<PathBuf> {
    let root_canon = fs::canonicalize(granted_root)
        .with_context(|| format!("Granted folder unavailable: {}", granted_root.display()))?;

    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        bail!("Absolute paths are not allowed; use a path relative to the granted folder");
    }

    let mut candidate = root_canon.clone();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(part) => candidate.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("Path traversal outside granted folder is not allowed: {rel_path}");
            }
        }
    }

    if candidate.exists() {
        let cand_canon = fs::canonicalize(&candidate)?;
        if !cand_canon.starts_with(&root_canon) {
            bail!("Path escapes granted folder: {rel_path}");
        }
        Ok(cand_canon)
    } else {
        // For new files, verify the nearest existing ancestor is inside root_canon.
        let mut ancestor = candidate.parent();
        while let Some(p) = ancestor {
            if p.exists() {
                let anc_canon = fs::canonicalize(p)?;
                if !anc_canon.starts_with(&root_canon) {
                    bail!("Target directory escapes granted folder: {rel_path}");
                }
                break;
            }
            ancestor = p.parent();
        }
        Ok(candidate)
    }
}

/// Strip surrounding quotes and stray leading/trailing whitespace if the exact path doesn't exist.
pub(super) fn sanitize_tool_path(raw: &str) -> &str {
    let trimmed = raw.trim();
    let unquoted = if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if Path::new(unquoted).exists() {
        unquoted
    } else {
        unquoted.trim()
    }
}

/// If a tool call targets an existing absolute path outside `granted_folders`, return its canonical path.
pub fn requested_ungranted_path(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> Option<PathBuf> {
    let raw_path = match tool_name {
        "read_file" | "delete_file" => args
            .get("root")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("path").and_then(|v| v.as_str()))
            .or_else(|| args.get("file_path").and_then(|v| v.as_str()))
            .or_else(|| args.get("filepath").and_then(|v| v.as_str()))
            .or_else(|| args.get("file").and_then(|v| v.as_str())),
        "write_file" | "create_directory" | "delete_directory" => args
            .get("root")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("path").and_then(|v| v.as_str()))
            .or_else(|| args.get("directory").and_then(|v| v.as_str())),
        "move_path" => args.get("root").and_then(|v| v.as_str()),
        "list_files" => args
            .get("root")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("subdir").and_then(|v| v.as_str()))
            .or_else(|| args.get("path").and_then(|v| v.as_str()))
            .or_else(|| args.get("directory").and_then(|v| v.as_str())),
        "search_files" => args.get("root").and_then(|v| v.as_str()),
        _ => None,
    }?;

    let cleaned = sanitize_tool_path(raw_path);
    let req = Path::new(cleaned);
    if !req.is_absolute() || !req.exists() {
        return None;
    }
    let target_canon = fs::canonicalize(req).ok()?;
    let require_write = matches!(
        tool_name,
        "write_file" | "delete_file" | "create_directory" | "move_path" | "delete_directory"
    );
    let allowed_roots = collect_allowed_roots(granted_folders, require_write);
    let already_covered = allowed_roots.iter().any(|root| {
        if root.is_file() {
            target_canon == *root
        } else {
            target_canon.starts_with(root)
        }
    });
    if already_covered {
        None
    } else {
        Some(target_canon)
    }
}

/// Return `true` if a non-destructive write tool targets a folder already granted as Read & Write.
pub fn is_write_already_granted(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> bool {
    let root_override = args.get("root").and_then(|v| v.as_str());
    match tool_name {
        "write_file" | "create_directory" => {
            let Some(path) = args
                .get("path")
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
            else {
                return false;
            };
            resolve_granted_root(granted_folders, path, root_override, true).is_ok()
        }
        "move_path" => {
            let (Some(from), Some(to)) = (
                args.get("from").and_then(|v| v.as_str()),
                args.get("to").and_then(|v| v.as_str()),
            ) else {
                return false;
            };
            let Ok((root, _)) = resolve_granted_root(granted_folders, from, root_override, true)
            else {
                return false;
            };
            resolve_granted_root(granted_folders, to, Some(&root.to_string_lossy()), true).is_ok()
        }
        _ => false,
    }
}

pub(super) fn collect_allowed_roots(
    granted_folders: &[GrantedFolder],
    require_write: bool,
) -> Vec<PathBuf> {
    let mut allowed_roots: Vec<PathBuf> = Vec::new();
    for folder in granted_folders {
        if (!require_write || folder.allow_write)
            && let Ok(canon) = fs::canonicalize(&folder.path)
            && !allowed_roots.contains(&canon)
        {
            allowed_roots.push(canon);
        }
    }
    allowed_roots
}

/// Resolve a tool's `(root_dir, relative_path)` against `granted_folders`.
pub(super) fn resolve_granted_root(
    granted_folders: &[GrantedFolder],
    raw_path: &str,
    root_override: Option<&str>,
    require_write: bool,
) -> Result<(PathBuf, String)> {
    let mut allowed_roots = collect_allowed_roots(granted_folders, require_write);
    if allowed_roots.is_empty() {
        if require_write {
            bail!("Action not permitted: no Read & Write directory or file granted");
        }
        bail!("No accessible directory or file is granted");
    }

    if let Some(ov) = root_override.map(str::trim).filter(|s| !s.is_empty()) {
        let ov_canon = fs::canonicalize(ov)
            .with_context(|| format!("Granted folder unavailable: {ov}"))?;
        if let Some(matched) = allowed_roots.into_iter().find(|r| ov_canon.starts_with(r)) {
            let rel_prefix = ov_canon.strip_prefix(&matched).unwrap_or_else(|_| Path::new(""));
            let combined = if raw_path.trim().is_empty() || raw_path == "." {
                rel_prefix.to_string_lossy().to_string()
            } else if rel_prefix.as_os_str().is_empty() {
                raw_path.to_string()
            } else {
                rel_prefix.join(raw_path).to_string_lossy().to_string()
            };
            let rel_final = if combined.is_empty() { ".".to_string() } else { combined };
            return Ok((matched, rel_final));
        }
        bail!(
            "Directory is not {} for this chat: {ov}",
            if require_write { "writable" } else { "granted" }
        );
    }

    let sanitized = sanitize_tool_path(raw_path);
    let req = Path::new(sanitized);
    if req.is_absolute() {
        let target_canon = if req.exists() {
            fs::canonicalize(req).with_context(|| format!("Path unavailable: {sanitized}"))?
        } else if require_write {
            let parent = req.parent().context("Invalid target path")?;
            let parent_canon = fs::canonicalize(parent)
                .with_context(|| format!("Parent folder unavailable: {}", parent.display()))?;
            let file_name = req.file_name().context("Invalid file name")?;
            parent_canon.join(file_name)
        } else {
            bail!("Path unavailable: {sanitized}");
        };
        for root in &allowed_roots {
            if root.is_file() {
                if target_canon == *root {
                    let parent = root.parent().context("Invalid file parent")?.to_path_buf();
                    let name = root
                        .file_name()
                        .and_then(|n| n.to_str())
                        .context("Invalid file name")?
                        .to_string();
                    return Ok((parent, name));
                }
                continue;
            }
            if let Ok(rel) = target_canon.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().to_string();
                return Ok((
                    root.clone(),
                    if rel_str.is_empty() { ".".to_string() } else { rel_str },
                ));
            }
        }
        bail!("Path is outside all permitted directories: {raw_path}");
    }

    if allowed_roots.len() == 1 {
        return Ok((allowed_roots.remove(0), raw_path.to_string()));
    }
    bail!(
        "Multiple folders are granted; specify \"root\" or an absolute path inside a granted folder"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_tool_path_strips_quotes_and_whitespace() {
        assert_eq!(sanitize_tool_path("  \"src/main.rs\"  "), "src/main.rs");
        assert_eq!(sanitize_tool_path(" 'notes.md' "), "notes.md");
        assert_eq!(sanitize_tool_path("plain.txt"), "plain.txt");
    }

    #[test]
    fn resolve_granted_root_handles_override_and_multi_root_ambiguity() {
        let base = std::env::temp_dir().join(format!("companion_pg_{}", uuid::Uuid::new_v4()));
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        let sub_a = dir_a.join("sub");
        fs::create_dir_all(&sub_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        let grants = vec![
            GrantedFolder {
                path: dir_a.clone(),
                allow_write: false,
            },
            GrantedFolder {
                path: dir_b.clone(),
                allow_write: false,
            },
        ];

        // Relative path with multiple granted roots and no override must error.
        assert!(resolve_granted_root(&grants, "file.txt", None, false).is_err());

        // Override inside a granted root resolves relative to that root.
        let (root, rel) =
            resolve_granted_root(&grants, "file.txt", Some(&sub_a.to_string_lossy()), false)
                .unwrap();
        assert_eq!(root, fs::canonicalize(&dir_a).unwrap());
        assert_eq!(rel, "sub/file.txt");

        let _ = fs::remove_dir_all(&base);
    }
}
