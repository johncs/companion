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

/// Shared argument aliases for file/path tools.
pub const PATH_ALIASES: &[&str] = &["path", "file_path", "filepath", "file"];
/// Shared argument aliases for directory tools.
pub const DIR_ALIASES: &[&str] = &["path", "directory"];
/// Shared argument aliases for `list_files`.
pub const LIST_ALIASES: &[&str] = &["subdir", "path", "directory"];

/// Return the first non-null string argument matching any key in `keys`.
pub fn first_str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
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
#[cfg(test)]
pub fn requested_ungranted_path(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> Option<PathBuf> {
    let raw_path = extract_raw_target_path(tool_name, args);
    let require_write = matches!(
        tool_name,
        "write_file" | "delete_file" | "create_directory" | "move_path" | "delete_directory"
    );
    resolve_ungranted_raw(granted_folders, raw_path, require_write)
}

/// Shared raw target-path extraction used by `requested_ungranted_path` and `Tool::raw_target_path`.
pub fn extract_raw_target_path<'a>(tool_name: &str, args: &'a Value) -> Option<&'a str> {
    let root = first_str_arg(args, &["root"]);
    match tool_name {
        "read_file" | "delete_file" => root.or_else(|| first_str_arg(args, PATH_ALIASES)),
        "write_file" => root.or_else(|| first_str_arg(args, PATH_ALIASES)),
        "create_directory" | "delete_directory" => {
            root.or_else(|| first_str_arg(args, DIR_ALIASES))
        }
        "move_path" | "search_files" => root,
        "list_files" => root.or_else(|| first_str_arg(args, LIST_ALIASES)),
        _ => None,
    }
}

/// Resolve an ungranted canonical path from a raw path argument.
pub fn resolve_ungranted_raw(
    granted_folders: &[GrantedFolder],
    raw_path: Option<&str>,
    require_write: bool,
) -> Option<PathBuf> {
    let raw_path = raw_path?;
    let cleaned = sanitize_tool_path(raw_path);
    let req = Path::new(cleaned);
    if !req.is_absolute() || !req.exists() {
        return None;
    }
    let target_canon = fs::canonicalize(req).ok()?;
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

/// Resolve the **exact** [`GrantedFolder::path`] targeted by a write tool, if it falls under one.
pub fn resolve_target_folder_for_write(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> Option<PathBuf> {
    let root_override = first_str_arg(args, &["root"]);
    let raw_path = match tool_name {
        "write_file" | "delete_file" => first_str_arg(args, PATH_ALIASES)?,
        "create_directory" | "delete_directory" => first_str_arg(args, DIR_ALIASES)?,
        "move_path" => first_str_arg(args, &["from"])?,
        _ => return None,
    };
    let (matched_root, _) =
        resolve_granted_root_inner(granted_folders, raw_path, root_override, false, true).ok()?;
    granted_folders
        .iter()
        .find(|g| fs::canonicalize(&g.path).ok().as_ref() == Some(&matched_root))
        .map(|g| g.path.clone())
        .or(Some(matched_root))
}

/// Return `true` if a non-destructive write tool targets a folder already granted as Read & Write.
pub fn is_write_already_granted(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> bool {
    let root_override = first_str_arg(args, &["root"]);
    match tool_name {
        "write_file" => {
            let Some(path) = first_str_arg(args, PATH_ALIASES) else {
                return false;
            };
            resolve_granted_root(granted_folders, path, root_override, true).is_ok()
        }
        "create_directory" => {
            let Some(path) = first_str_arg(args, DIR_ALIASES) else {
                return false;
            };
            resolve_granted_root(granted_folders, path, root_override, true).is_ok()
        }
        "move_path" => {
            let (Some(from), Some(to)) = (
                first_str_arg(args, &["from"]),
                first_str_arg(args, &["to"]),
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
    resolve_granted_root_inner(
        granted_folders,
        raw_path,
        root_override,
        require_write,
        require_write,
    )
}

fn resolve_granted_root_inner(
    granted_folders: &[GrantedFolder],
    raw_path: &str,
    root_override: Option<&str>,
    require_write: bool,
    allow_new_target: bool,
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
        } else if allow_new_target {
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
