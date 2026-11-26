//! Path-jailing, grant resolution, and write-permission checks for local agent tools.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use companion_storage::ConversationFolder;

use crate::tool::{
    ApprovalRequest, PreparedToolCall, ToolAuthorization, ToolPrepareContext, ToolRisk,
};

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
    prepared: PreparedToolCall,
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
        ToolAuthorization::AutoAllowed(prepared)
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
            prepared,
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

/// Resolve `rel_path` strictly inside `granted_root`, rejecting any `..` or symlink escape (existing or dangling).
pub fn resolve_granted_path(granted_root: &Path, rel_path: &str) -> Result<PathBuf> {
    resolve_granted_path_inner(granted_root, rel_path, true)
}

/// Resolve `rel_path` inside `granted_root` without following a symlink at the final leaf component.
pub fn resolve_granted_leaf(granted_root: &Path, rel_path: &str) -> Result<PathBuf> {
    resolve_granted_path_inner(granted_root, rel_path, false)
}

fn resolve_granted_path_inner(
    granted_root: &Path,
    rel_path: &str,
    follow_leaf_symlink: bool,
) -> Result<PathBuf> {
    let root_canon = fs::canonicalize(granted_root)
        .with_context(|| format!("Granted folder unavailable: {}", granted_root.display()))?;
    let parts = collect_rel_components(rel_path)?;
    let mut candidate = root_canon.clone();
    for (idx, part) in parts.iter().enumerate() {
        candidate.push(part);
        let is_last = idx + 1 == parts.len();
        if let Ok(meta) = fs::symlink_metadata(&candidate)
            && meta.file_type().is_symlink()
        {
            if is_last && !follow_leaf_symlink {
                return Ok(candidate);
            }
            let resolved = fs::canonicalize(&candidate).with_context(|| {
                format!("Dangling or unreadable symlink is not allowed: {rel_path}")
            })?;
            if !resolved.starts_with(&root_canon) {
                bail!("Path escapes granted folder via symlink: {rel_path}");
            }
            candidate = resolved;
        }
    }
    if let Ok(cand_canon) = fs::canonicalize(&candidate) {
        if !cand_canon.starts_with(&root_canon) {
            bail!("Path escapes granted folder: {rel_path}");
        }
        return Ok(cand_canon);
    }
    Ok(candidate)
}

fn collect_rel_components(rel_path: &str) -> Result<Vec<&std::ffi::OsStr>> {
    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        bail!("Absolute paths are not allowed; use a path relative to the granted folder");
    }
    Ok(rel
        .components()
        .map(|c| match c {
            Component::CurDir => Ok(None),
            Component::Normal(p) => Ok(Some(p)),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => Err(
                anyhow::anyhow!("Path traversal outside granted folder is not allowed: {rel_path}"),
            ),
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
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
        return resolve_root_override(&allowed_roots, raw_path, ov, require_write);
    }

    let sanitized = sanitize_tool_path(raw_path);
    let req = Path::new(sanitized);
    if req.is_absolute() {
        return resolve_absolute_target(&allowed_roots, req, sanitized, raw_path, allow_new_target);
    }

    if allowed_roots.len() == 1 {
        return Ok((allowed_roots.remove(0), raw_path.to_string()));
    }
    bail!(
        "Multiple folders are granted; specify \"root\" or an absolute path inside a granted folder"
    );
}

fn resolve_root_override(
    allowed_roots: &[PathBuf],
    raw_path: &str,
    ov: &str,
    require_write: bool,
) -> Result<(PathBuf, String)> {
    let ov_canon =
        fs::canonicalize(ov).with_context(|| format!("Granted folder unavailable: {ov}"))?;
    let Some(matched) = allowed_roots.iter().find(|r| ov_canon.starts_with(r)) else {
        bail!(
            "Directory is not {} for this chat: {ov}",
            if require_write { "writable" } else { "granted" }
        );
    };
    let rel_prefix = ov_canon
        .strip_prefix(matched)
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
    Ok((matched.clone(), rel_final))
}

fn resolve_absolute_target(
    allowed_roots: &[PathBuf],
    req: &Path,
    sanitized: &str,
    raw_path: &str,
    allow_new_target: bool,
) -> Result<(PathBuf, String)> {
    let target_canon = if let Ok(canon) = fs::canonicalize(req) {
        canon
    } else if allow_new_target {
        let anc = req
            .ancestors()
            .find(|a| a.exists())
            .context("Invalid target path")?;
        let anc_canon = fs::canonicalize(anc)
            .with_context(|| format!("Parent folder unavailable: {}", anc.display()))?;
        let rel = req.strip_prefix(anc).context("Invalid target path")?;
        if rel.components().any(|c| matches!(c, Component::ParentDir)) {
            bail!("Path traversal is not allowed: {sanitized}");
        }
        anc_canon.join(rel)
    } else {
        bail!("Path unavailable: {sanitized}");
    };
    for root in allowed_roots {
        if root.is_file() {
            if target_canon == *root {
                let parent = root.parent().context("Invalid file parent")?.to_path_buf();
                let name = root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .context("Invalid file name")?;
                return Ok((parent, name.to_string()));
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
