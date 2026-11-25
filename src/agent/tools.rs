//! Bounded, path-jailed tools operating strictly inside user-granted folders and files.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

const MAX_LIST_ENTRIES: usize = 200;
const MAX_SEARCH_HITS: usize = 50;
const MAX_READ_BYTES: usize = 100 * 1024;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".flatpak-builder", "dist", "build"];

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

/// List files in `granted_root` (bounded, skipping `.git`, `target`, `node_modules`, and `.gitignore`).
pub fn list_granted_files(granted_root: &Path, subdir: &str, max_entries: usize) -> Result<Vec<String>> {
    let start = resolve_granted_path(granted_root, subdir)?;
    let root_canon = fs::canonicalize(granted_root)?;
    let ignored = load_gitignore_names(&root_canon);
    let mut out = Vec::new();
    collect_files(&root_canon, &start, &ignored, max_entries.min(MAX_LIST_ENTRIES), &mut out);
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
    fs::create_dir_all(&target).with_context(|| format!("Failed to create directory {rel_path}"))?;
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
    fs::remove_dir_all(&target).with_context(|| format!("Failed to delete directory {rel_path}"))?;
    Ok(format!("Deleted directory {rel_path}"))
}

/// Build a concise human-readable preview for a permission prompt.
pub fn describe_tool_action(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> String {
    match tool_name {
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let root_override = args.get("root").and_then(|v| v.as_str());
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let exists = resolve_granted_root(granted_folders, path, root_override, true)
                .and_then(|(r, rel)| resolve_granted_path(&r, &rel))
                .map(|p| p.exists())
                .unwrap_or(false);
            let action = if exists { "Overwrite" } else { "Create" };
            let preview: String = content.lines().take(12).collect::<Vec<_>>().join("\n");
            format!("{action} file: {path}\n\n{preview}")
        }
        "delete_file" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .or_else(|| args.get("filepath"))
                .or_else(|| args.get("file"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("Delete file:\n\n{path}")
        }
        "create_directory" => {
            let path = args
                .get("path")
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("Create directory:\n\n{path}")
        }
        "move_path" => {
            let from = args.get("from").and_then(|v| v.as_str()).unwrap_or("?");
            let to = args.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Move / rename:\n\n{from}\n→ {to}")
        }
        "delete_directory" => {
            let path = args
                .get("path")
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("Delete directory (and its contents):\n\n{path}")
        }
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Read file:\n\n{path}")
        }
        "list_files" => {
            let target = args
                .get("root")
                .or_else(|| args.get("subdir"))
                .or_else(|| args.get("path"))
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            format!("List files in:\n\n{target}")
        }
        "search_files" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let root = args
                .get("root")
                .and_then(|v| v.as_str())
                .unwrap_or("granted folders");
            format!("Search for \"{query}\" in:\n\n{root}")
        }
        _ => format!("Run {tool_name} with {args}"),
    }
}

/// Strip surrounding quotes and stray leading/trailing whitespace if the exact path doesn't exist.
fn sanitize_tool_path(raw: &str) -> &str {
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

#[derive(Debug, Clone)]
pub struct GrantedFolder {
    pub path: PathBuf,
    pub allow_write: bool,
}

fn collect_allowed_roots(
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
fn resolve_granted_root(
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

/// Execute a tool call with `granted_folders`.
pub fn execute_tool_with_grants(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> Result<String> {
    let root_override = args.get("root").and_then(|v| v.as_str());
    match tool_name {
        "list_files" => {
            let subdir = args
                .get("subdir")
                .or_else(|| args.get("path"))
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            if root_override.is_none() && (subdir.trim().is_empty() || subdir == ".") {
                let roots = collect_allowed_roots(granted_folders, false);
                if roots.len() > 1 {
                    let mut sections = Vec::new();
                    for root in roots {
                        if root.is_file() {
                            sections.push(format!("== {} ==\n(file)", root.display()));
                            continue;
                        }
                        let files = list_granted_files(&root, ".", MAX_LIST_ENTRIES)?;
                        let body = if files.is_empty() {
                            "(empty folder)".to_string()
                        } else {
                            files.join("\n")
                        };
                        sections.push(format!("== {} ==\n{body}", root.display()));
                    }
                    return Ok(sections.join("\n\n"));
                }
                if roots.len() == 1 && roots[0].is_file() {
                    return Ok(roots[0].display().to_string());
                }
            }
            let (root, rel_sub) =
                resolve_granted_root(granted_folders, subdir, root_override, false)?;
            let files = list_granted_files(&root, &rel_sub, MAX_LIST_ENTRIES)?;
            if files.is_empty() {
                Ok("(empty folder)".to_string())
            } else {
                Ok(files.join("\n"))
            }
        }
        "read_file" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .or_else(|| args.get("filepath"))
                .or_else(|| args.get("file"))
                .and_then(|v| v.as_str())
                .context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, false)?;
            read_granted_file(&root, &rel_path)
        }
        "search_files" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .context("Missing 'query' argument")?;
            if root_override.is_none() {
                let roots = collect_allowed_roots(granted_folders, false);
                if roots.len() > 1 {
                    let mut all_hits = Vec::new();
                    for root in roots {
                        if root.is_dir() {
                            for h in search_granted_files(&root, query)? {
                                all_hits.push(format!("{}/{h}", root.display()));
                            }
                        }
                    }
                    return if all_hits.is_empty() {
                        Ok("No matches found.".to_string())
                    } else {
                        Ok(all_hits.join("\n"))
                    };
                }
            }
            let (root, _) = resolve_granted_root(granted_folders, ".", root_override, false)?;
            let hits = search_granted_files(&root, query)?;
            if hits.is_empty() {
                Ok("No matches found.".to_string())
            } else {
                Ok(hits.join("\n"))
            }
        }
        "write_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .context("Missing 'path' argument")?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .context("Missing 'content' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            write_granted_file(&root, &rel_path, content)
        }
        "delete_file" => {
            let path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .or_else(|| args.get("filepath"))
                .or_else(|| args.get("file"))
                .and_then(|v| v.as_str())
                .context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            delete_granted_file(&root, &rel_path)
        }
        "create_directory" => {
            let path = args
                .get("path")
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            create_granted_directory(&root, &rel_path)
        }
        "move_path" => {
            let from = args
                .get("from")
                .and_then(|v| v.as_str())
                .context("Missing 'from' argument")?;
            let to = args
                .get("to")
                .and_then(|v| v.as_str())
                .context("Missing 'to' argument")?;
            let (root, rel_from) =
                resolve_granted_root(granted_folders, from, root_override, true)?;
            let (_, rel_to) =
                resolve_granted_root(granted_folders, to, Some(&root.to_string_lossy()), true)?;
            move_granted_path(&root, &rel_from, &rel_to)
        }
        "delete_directory" => {
            let path = args
                .get("path")
                .or_else(|| args.get("directory"))
                .and_then(|v| v.as_str())
                .context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            delete_granted_directory(&root, &rel_path)
        }
        other => bail!("Unknown tool: {other}"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_jail_blocks_traversal_and_allows_rw() {
        let root = std::env::temp_dir().join("companion_agent_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/ignored.txt"), "skip").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        assert!(resolve_granted_path(&root, "../etc/passwd").is_err());
        assert!(resolve_granted_path(&root, "/etc/passwd").is_err());

        let files = list_granted_files(&root, ".", 50).unwrap();
        assert_eq!(files, vec!["src/main.rs".to_string()]);

        write_granted_file(&root, "notes.md", "# Notes").unwrap();
        assert_eq!(read_granted_file(&root, "notes.md").unwrap(), "# Notes");
        delete_granted_file(&root, "notes.md").unwrap();
        assert!(!root.join("notes.md").exists());
        assert!(delete_granted_file(&root, "../etc/passwd").is_err());

        create_granted_directory(&root, "docs/sub").unwrap();
        assert!(root.join("docs/sub").is_dir());
        write_granted_file(&root, "docs/sub/a.txt", "hi").unwrap();
        move_granted_path(&root, "docs/sub/a.txt", "docs/b.txt").unwrap();
        assert!(root.join("docs/b.txt").is_file());
        assert!(move_granted_path(&root, "docs/b.txt", "../escape.txt").is_err());
        assert!(delete_granted_directory(&root, ".").is_err());
        delete_granted_directory(&root, "docs").unwrap();
        assert!(!root.join("docs").exists());

        let rw = vec![GrantedFolder {
            path: root.clone(),
            allow_write: true,
        }];
        let ro = vec![GrantedFolder {
            path: root.clone(),
            allow_write: false,
        }];
        assert!(is_write_already_granted(
            &rw,
            "write_file",
            &serde_json::json!({ "path": "a.txt", "content": "hi" }),
        ));
        assert!(!is_write_already_granted(
            &ro,
            "write_file",
            &serde_json::json!({ "path": "a.txt", "content": "hi" }),
        ));
        assert!(!is_write_already_granted(
            &rw,
            "delete_file",
            &serde_json::json!({ "path": "a.txt" }),
        ));

        let hits = search_granted_files(&root, "main").unwrap();
        assert_eq!(hits.len(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn readonly_granted_folders_allow_reads_and_block_writes() {
        let granted = std::env::temp_dir().join("companion_granted_ro_test");
        let _ = fs::remove_dir_all(&granted);
        fs::create_dir_all(&granted).unwrap();
        fs::write(granted.join("readme.txt"), "hello world").unwrap();

        let grants = vec![GrantedFolder {
            path: granted.clone(),
            allow_write: false,
        }];
        let listed = execute_tool_with_grants(&grants, "list_files", &serde_json::json!({}))
            .unwrap();
        assert_eq!(listed, "readme.txt");

        let abs = granted.join("readme.txt").to_string_lossy().to_string();
        let read = execute_tool_with_grants(
            &grants,
            "read_file",
            &serde_json::json!({ "path": abs }),
        )
        .unwrap();
        assert_eq!(read, "hello world");

        assert!(
            execute_tool_with_grants(
                &grants,
                "write_file",
                &serde_json::json!({ "path": "evil.txt", "content": "nope" }),
            )
            .is_err()
        );

        let rw_grants = vec![GrantedFolder {
            path: granted.clone(),
            allow_write: true,
        }];
        execute_tool_with_grants(
            &rw_grants,
            "write_file",
            &serde_json::json!({ "path": "allowed.txt", "content": "ok" }),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(granted.join("allowed.txt")).unwrap(), "ok");
        assert!(
            execute_tool_with_grants(
                &grants,
                "read_file",
                &serde_json::json!({ "path": "/etc/passwd" }),
            )
            .is_err()
        );

        let granted2 = std::env::temp_dir().join("companion_granted_ro_test_2");
        let _ = fs::remove_dir_all(&granted2);
        fs::create_dir_all(&granted2).unwrap();
        fs::write(granted2.join("second.md"), "two").unwrap();
        let multi = vec![
            GrantedFolder {
                path: granted.clone(),
                allow_write: false,
            },
            GrantedFolder {
                path: granted2.clone(),
                allow_write: false,
            },
        ];
        let all_listed =
            execute_tool_with_grants(&multi, "list_files", &serde_json::json!({})).unwrap();
        assert!(all_listed.contains("readme.txt"));
        assert!(all_listed.contains("second.md"));

        // Single-file grant: allows reading that file, blocks sibling files in the same directory.
        let single_dir = std::env::temp_dir().join("companion_granted_single_file_test");
        let _ = fs::remove_dir_all(&single_dir);
        fs::create_dir_all(&single_dir).unwrap();
        let allowed_file = single_dir.join("allowed.md");
        let sibling_file = single_dir.join("secret.md");
        fs::write(&allowed_file, "visible").unwrap();
        fs::write(&sibling_file, "hidden").unwrap();
        let file_grant = vec![GrantedFolder {
            path: allowed_file.clone(),
            allow_write: false,
        }];
        let file_read = execute_tool_with_grants(
            &file_grant,
            "read_file",
            &serde_json::json!({ "path": allowed_file.to_string_lossy() }),
        )
        .unwrap();
        assert_eq!(file_read, "visible");
        assert!(
            execute_tool_with_grants(
                &file_grant,
                "read_file",
                &serde_json::json!({ "path": sibling_file.to_string_lossy() }),
            )
            .is_err()
        );

        let _ = fs::remove_dir_all(&single_dir);
        let _ = fs::remove_dir_all(&granted);
        let _ = fs::remove_dir_all(&granted2);
    }

    #[test]
    fn requested_ungranted_path_detects_outside_files_and_folders() {
        let dir = std::env::temp_dir().join("companion_ungranted_path_test");
        let _ = fs::remove_dir_all(&dir);
        let sub = dir.join("Aetherial Exorover");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("prompts.txt");
        fs::write(&file, "hello").unwrap();

        let ungranted = requested_ungranted_path(
            &[],
            "read_file",
            &serde_json::json!({ "path": format!(" \"{}\" ", file.to_string_lossy()) }),
        );
        assert_eq!(ungranted, Some(fs::canonicalize(&file).unwrap()));

        let content = execute_tool_with_grants(
            &[GrantedFolder {
                path: file.clone(),
                allow_write: false,
            }],
            "read_file",
            &serde_json::json!({ "path": format!("{} ", file.to_string_lossy()) }),
        )
        .unwrap();
        assert_eq!(content, "hello");

        let grants = vec![GrantedFolder {
            path: dir.clone(),
            allow_write: false,
        }];
        let covered = requested_ungranted_path(
            &grants,
            "read_file",
            &serde_json::json!({ "path": file.to_string_lossy() }),
        );
        assert_eq!(covered, None);

        let _ = fs::remove_dir_all(&dir);
    }
}
