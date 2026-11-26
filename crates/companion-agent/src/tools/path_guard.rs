//! Path-jailing, grant resolution, and write-permission checks for local agent tools.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use companion_storage::ConversationFolder;

use crate::tool::{ApprovalRequest, ToolAuthorization, ToolPrepareContext, ToolRisk};

#[derive(Debug, Clone)]
pub struct GrantedFolder {
    pub path: PathBuf,
    pub allow_write: bool,
}

/// Shared pre-flight authorization for workspace file/directory tools.
pub(super) fn prepare_workspace_auth(
    ctx: &ToolPrepareContext<'_>,
    tool_name: &str,
    risk: ToolRisk,
    raw_target: Option<&str>,
    write_target_folder: Option<PathBuf>,
    preview: String,
) -> ToolAuthorization {
    let needs_write = matches!(risk, ToolRisk::WriteWorkspace | ToolRisk::Destructive);
    let ungranted_path = resolve_ungranted_raw(ctx.granted_folders, raw_target, needs_write);
    let write_already_granted = write_target_folder.as_ref().is_some_and(|folder| {
        ctx.granted_folders
            .iter()
            .any(|g| g.allow_write && &g.path == folder)
    });
    let auto_allowed = ungranted_path.is_none()
        && (ctx.permissions.is_auto_allowed(
            ctx.conv_id,
            write_target_folder.as_deref(),
            tool_name,
            risk,
        ) || (risk == ToolRisk::WriteWorkspace && write_already_granted));

    if auto_allowed {
        ToolAuthorization::AutoAllowed
    } else {
        ToolAuthorization::RequiresApproval {
            request: ApprovalRequest::ToolAction {
                tool_name: tool_name.to_string(),
                preview,
                risk,
                ungranted_path: ungranted_path.clone(),
            },
            ungranted_path,
            write_target_folder,
        }
    }
}

/// If `candidate` is already inside (or equal to) a granted directory in `entries`, return that folder's display name.
pub fn find_covering_granted_folder(
    entries: &[ConversationFolder],
    candidate: &Path,
) -> Option<String> {
    let cand_canon = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    for entry in entries {
        let entry_path = Path::new(&entry.folder_path);
        if !entry_path.is_dir() {
            continue;
        }
        let dir_canon = fs::canonicalize(entry_path).unwrap_or_else(|_| entry_path.to_path_buf());
        if cand_canon.starts_with(&dir_canon) {
            let name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&entry.folder_path)
                .to_string();
            return Some(name);
        }
    }
    None
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

/// Resolve an ungranted canonical path from a raw path argument.
/// For non-existent absolute targets (e.g. a new file in an ungranted folder),
/// walks up to the nearest existing ancestor so the target cannot bypass approval.
pub(super) fn resolve_ungranted_raw(
    granted_folders: &[GrantedFolder],
    raw_path: Option<&str>,
    require_write: bool,
) -> Option<PathBuf> {
    let raw_path = raw_path?;
    let cleaned = sanitize_tool_path(raw_path);
    let req = Path::new(cleaned);
    if !req.is_absolute() {
        return None;
    }
    let target_canon = if req.exists() {
        fs::canonicalize(req).ok()?
    } else {
        let existing_ancestor = req.ancestors().find(|a| a.exists())?;
        let canon_ancestor = fs::canonicalize(existing_ancestor).ok()?;
        let rel = req.strip_prefix(existing_ancestor).ok()?;
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            return None;
        }
        let allowed_roots = collect_allowed_roots(granted_folders, require_write);
        let already_covered = allowed_roots.iter().any(|root| {
            if root.is_file() {
                canon_ancestor == *root
            } else {
                canon_ancestor.starts_with(root)
            }
        });
        return if already_covered {
            None
        } else {
            Some(canon_ancestor)
        };
    };
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
pub(super) fn resolve_write_folder_for_raw(
    granted_folders: &[GrantedFolder],
    raw_path: &str,
    root_override: Option<&str>,
) -> Option<PathBuf> {
    let (matched_root, _) =
        resolve_granted_root_inner(granted_folders, raw_path, root_override, false, true).ok()?;
    granted_folders
        .iter()
        .find(|g| fs::canonicalize(&g.path).ok().as_ref() == Some(&matched_root))
        .map(|g| g.path.clone())
        .or(Some(matched_root))
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
        let ov_canon =
            fs::canonicalize(ov).with_context(|| format!("Granted folder unavailable: {ov}"))?;
        if let Some(matched) = allowed_roots.into_iter().find(|r| ov_canon.starts_with(r)) {
            let rel_prefix = ov_canon
                .strip_prefix(&matched)
                .unwrap_or_else(|_| Path::new(""));
            let combined = if raw_path.trim().is_empty() || raw_path == "." {
                rel_prefix.to_string_lossy().to_string()
            } else if rel_prefix.as_os_str().is_empty() {
                raw_path.to_string()
            } else {
                rel_prefix.join(raw_path).to_string_lossy().to_string()
            };
            let rel_final = if combined.is_empty() {
                ".".to_string()
            } else {
                combined
            };
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
                    if rel_str.is_empty() {
                        ".".to_string()
                    } else {
                        rel_str
                    },
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
    fn detects_file_or_subfolder_inside_granted_directory() {
        let root = std::env::temp_dir().join("companion_folder_cover_test");
        let _ = fs::remove_dir_all(&root);
        let granted_dir = root.join("Photos");
        let sub_dir = granted_dir.join("Raw");
        let sibling_dir = root.join("Other");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::create_dir_all(&sibling_dir).unwrap();
        let inside_file = granted_dir.join("PERSON.md");
        let outside_file = sibling_dir.join("notes.txt");
        fs::write(&inside_file, "hi").unwrap();
        fs::write(&outside_file, "bye").unwrap();

        let entries = vec![ConversationFolder {
            id: "1".to_string(),
            conversation_id: "c1".to_string(),
            folder_path: granted_dir.to_string_lossy().to_string(),
            permission_mode: companion_core::types::FolderPermission::Read,
            created_at: 0,
        }];
        assert_eq!(
            find_covering_granted_folder(&entries, &inside_file).as_deref(),
            Some("Photos")
        );
        assert_eq!(
            find_covering_granted_folder(&entries, &sub_dir).as_deref(),
            Some("Photos")
        );
        assert_eq!(find_covering_granted_folder(&entries, &outside_file), None);

        let _ = fs::remove_dir_all(&root);
    }
}
