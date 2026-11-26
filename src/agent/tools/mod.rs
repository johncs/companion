//! Bounded, path-jailed tools operating strictly inside user-granted folders and files.

mod fs_ops;
mod path_guard;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub use fs_ops::{
    create_granted_directory, delete_granted_directory, delete_granted_file, list_granted_files,
    move_granted_path, read_granted_file, search_granted_files, write_granted_file,
};
pub use path_guard::{
    DIR_ALIASES, GrantedFolder, LIST_ALIASES, PATH_ALIASES, extract_raw_target_path,
    first_str_arg, is_write_already_granted, resolve_granted_path,
    resolve_target_folder_for_write, resolve_ungranted_raw,
};

use fs_ops::MAX_LIST_ENTRIES;
use path_guard::{collect_allowed_roots, resolve_granted_root};

/// Build a concise human-readable preview for a permission prompt.
pub fn describe_tool_action(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> String {
    match tool_name {
        "write_file" => describe_write_file(granted_folders, args),
        "delete_file" => {
            let path = first_str_arg(args, PATH_ALIASES).unwrap_or("?");
            format!("Delete file:\n\n{path}")
        }
        "create_directory" => {
            let path = first_str_arg(args, DIR_ALIASES).unwrap_or("?");
            format!("Create directory:\n\n{path}")
        }
        "move_path" => {
            let from = first_str_arg(args, &["from"]).unwrap_or("?");
            let to = first_str_arg(args, &["to"]).unwrap_or("?");
            format!("Move / rename:\n\n{from}\n→ {to}")
        }
        "delete_directory" => {
            let path = first_str_arg(args, DIR_ALIASES).unwrap_or("?");
            format!("Delete directory (and its contents):\n\n{path}")
        }
        "read_file" => {
            let path = first_str_arg(args, PATH_ALIASES).unwrap_or("?");
            format!("Read file:\n\n{path}")
        }
        "list_files" => {
            let target = first_str_arg(args, &["root", "subdir", "path", "directory"]).unwrap_or(".");
            format!("List files in:\n\n{target}")
        }
        "search_files" => {
            let query = first_str_arg(args, &["query"]).unwrap_or("");
            let root = first_str_arg(args, &["root"]).unwrap_or("granted folders");
            format!("Search for \"{query}\" in:\n\n{root}")
        }
        _ => format!("Run {tool_name} with {args}"),
    }
}

fn describe_write_file(granted_folders: &[GrantedFolder], args: &Value) -> String {
    let path = first_str_arg(args, PATH_ALIASES).unwrap_or("?");
    let root_override = first_str_arg(args, &["root"]);
    let content = first_str_arg(args, &["content"]).unwrap_or("");
    let exists = resolve_granted_root(granted_folders, path, root_override, true)
        .and_then(|(r, rel)| resolve_granted_path(&r, &rel))
        .map(|p| p.exists())
        .unwrap_or(false);
    let action = if exists { "Overwrite" } else { "Create" };
    let preview: String = content.lines().take(12).collect::<Vec<_>>().join("\n");
    format!("{action} file: {path}\n\n{preview}")
}

/// Execute a built-in workspace tool call with `granted_folders`.
pub fn execute_tool_with_grants(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &Value,
) -> Result<String> {
    let root_override = first_str_arg(args, &["root"]);
    match tool_name {
        "list_files" => exec_list_files(granted_folders, root_override, args),
        "read_file" => {
            let path = first_str_arg(args, PATH_ALIASES).context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, false)?;
            read_granted_file(&root, &rel_path)
        }
        "search_files" => exec_search_files(granted_folders, root_override, args),
        "write_file" => {
            let path = first_str_arg(args, PATH_ALIASES).context("Missing 'path' argument")?;
            let content =
                first_str_arg(args, &["content"]).context("Missing 'content' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            write_granted_file(&root, &rel_path, content)
        }
        "delete_file" => {
            let path = first_str_arg(args, PATH_ALIASES).context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            delete_granted_file(&root, &rel_path)
        }
        "create_directory" => {
            let path = first_str_arg(args, DIR_ALIASES).context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            create_granted_directory(&root, &rel_path)
        }
        "move_path" => {
            let from = first_str_arg(args, &["from"]).context("Missing 'from' argument")?;
            let to = first_str_arg(args, &["to"]).context("Missing 'to' argument")?;
            let (root, rel_from) =
                resolve_granted_root(granted_folders, from, root_override, true)?;
            let (_, rel_to) =
                resolve_granted_root(granted_folders, to, Some(&root.to_string_lossy()), true)?;
            move_granted_path(&root, &rel_from, &rel_to)
        }
        "delete_directory" => {
            let path = first_str_arg(args, DIR_ALIASES).context("Missing 'path' argument")?;
            let (root, rel_path) =
                resolve_granted_root(granted_folders, path, root_override, true)?;
            delete_granted_directory(&root, &rel_path)
        }
        other => bail!("Unknown tool: {other}"),
    }
}

fn exec_list_files(
    granted_folders: &[GrantedFolder],
    root_override: Option<&str>,
    args: &Value,
) -> Result<String> {
    let subdir = first_str_arg(args, LIST_ALIASES).unwrap_or(".");
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
    let (root, rel_sub) = resolve_granted_root(granted_folders, subdir, root_override, false)?;
    let files = list_granted_files(&root, &rel_sub, MAX_LIST_ENTRIES)?;
    Ok(if files.is_empty() {
        "(empty folder)".to_string()
    } else {
        files.join("\n")
    })
}

fn exec_search_files(
    granted_folders: &[GrantedFolder],
    root_override: Option<&str>,
    args: &Value,
) -> Result<String> {
    let query = first_str_arg(args, &["query"]).context("Missing 'query' argument")?;
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
            return Ok(if all_hits.is_empty() {
                "No matches found.".to_string()
            } else {
                all_hits.join("\n")
            });
        }
    }
    let (root, _) = resolve_granted_root(granted_folders, ".", root_override, false)?;
    let hits = search_granted_files(&root, query)?;
    Ok(if hits.is_empty() {
        "No matches found.".to_string()
    } else {
        hits.join("\n")
    })
}
