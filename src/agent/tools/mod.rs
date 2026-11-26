//! Bounded, path-jailed tools operating strictly inside user-granted folders and files.

mod fs_ops;
mod path_guard;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub use fs_ops::{
    create_granted_directory, delete_granted_directory, delete_granted_file, list_granted_files,
    move_granted_path, read_granted_file, search_granted_files, write_granted_file,
};
pub use path_guard::{
    GrantedFolder, is_write_already_granted, requested_ungranted_path, resolve_granted_path,
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

#[cfg(test)]
mod tests {
    use std::fs;

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
