use std::fs;

use serde_json::json;

use super::fs_ops::{
    create_granted_directory, delete_granted_directory, delete_granted_file, list_granted_files,
    move_granted_path, read_granted_file, search_granted_files, write_granted_file,
};
use super::path_guard::resolve_granted_path;
use super::*;
use crate::approval::ApprovalResponse;
use crate::permissions::PermissionEngine;
use crate::registry::ToolRegistry;
use crate::tool::{ApprovalRequest, ToolAuthorization, ToolContext, ToolPrepareContext};
use companion_core::types::{PermissionDecision, SearchEngine};

fn is_write_already_granted(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &serde_json::Value,
) -> bool {
    let reg = ToolRegistry::with_builtins();
    let Some(tool) = reg.get(tool_name) else {
        return false;
    };
    let perms = PermissionEngine::new();
    let ctx = ToolPrepareContext {
        conv_id: "test-conv",
        granted_folders,
        permissions: &perms,
        web_search_enabled: false,
        search_engine: SearchEngine::default(),
        searxng_url: None,
        local_texts: &[],
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    matches!(
        rt.block_on(tool.prepare(&ctx, args)),
        ToolAuthorization::AutoAllowed
    )
}

fn describe_tool_action(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &serde_json::Value,
) -> String {
    let reg = ToolRegistry::with_builtins();
    let tool = reg.get(tool_name).unwrap();
    let perms = PermissionEngine::new();
    let ctx = ToolPrepareContext {
        conv_id: "test-conv",
        granted_folders,
        permissions: &perms,
        web_search_enabled: false,
        search_engine: SearchEngine::default(),
        searxng_url: None,
        local_texts: &[],
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    match rt.block_on(tool.prepare(&ctx, args)) {
        ToolAuthorization::RequiresApproval {
            request: ApprovalRequest::ToolAction { preview, .. },
            ..
        } => preview,
        other => format!("{other:?}"),
    }
}

fn execute_tool_with_grants(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &serde_json::Value,
) -> anyhow::Result<String> {
    let reg = ToolRegistry::with_builtins();
    let tool = reg
        .get(tool_name)
        .ok_or_else(|| anyhow::anyhow!("Unknown tool: {tool_name}"))?;
    let ctx = ToolContext {
        granted_folders,
        web_search_enabled: false,
        search_engine: SearchEngine::default(),
        searxng_url: None,
    };
    let approval = ApprovalResponse::Tool(PermissionDecision::AllowOnce);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(tool.execute_approved(&ctx, args, &approval))
}

fn requested_ungranted_path(
    granted_folders: &[GrantedFolder],
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<std::path::PathBuf> {
    let reg = ToolRegistry::with_builtins();
    let tool = reg.get(tool_name)?;
    let perms = PermissionEngine::new();
    let ctx = ToolPrepareContext {
        conv_id: "test-conv",
        granted_folders,
        permissions: &perms,
        web_search_enabled: false,
        search_engine: SearchEngine::default(),
        searxng_url: None,
        local_texts: &[],
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    match rt.block_on(tool.prepare(&ctx, args)) {
        ToolAuthorization::RequiresApproval { ungranted_path, .. } => ungranted_path,
        _ => None,
    }
}

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
        &json!({ "path": "a.txt", "content": "hi" }),
    ));
    assert!(!is_write_already_granted(
        &ro,
        "write_file",
        &json!({ "path": "a.txt", "content": "hi" }),
    ));
    assert!(!is_write_already_granted(
        &rw,
        "delete_file",
        &json!({ "path": "a.txt" }),
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
    let listed = execute_tool_with_grants(&grants, "list_files", &json!({})).unwrap();
    assert_eq!(listed, "readme.txt");

    let abs = granted.join("readme.txt").to_string_lossy().to_string();
    let read = execute_tool_with_grants(&grants, "read_file", &json!({ "path": abs })).unwrap();
    assert_eq!(read, "hello world");

    assert!(
        execute_tool_with_grants(
            &grants,
            "write_file",
            &json!({ "path": "evil.txt", "content": "nope" }),
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
        &json!({ "path": "allowed.txt", "content": "ok" }),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(granted.join("allowed.txt")).unwrap(),
        "ok"
    );
    assert!(
        execute_tool_with_grants(&grants, "read_file", &json!({ "path": "/etc/passwd" })).is_err()
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
    let all_listed = execute_tool_with_grants(&multi, "list_files", &json!({})).unwrap();
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
        &json!({ "path": allowed_file.to_string_lossy() }),
    )
    .unwrap();
    assert_eq!(file_read, "visible");
    assert!(
        execute_tool_with_grants(
            &file_grant,
            "read_file",
            &json!({ "path": sibling_file.to_string_lossy() }),
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
        &json!({ "path": format!(" \"{}\" ", file.to_string_lossy()) }),
    );
    assert_eq!(ungranted, Some(fs::canonicalize(&file).unwrap()));

    let content = execute_tool_with_grants(
        &[GrantedFolder {
            path: file.clone(),
            allow_write: false,
        }],
        "read_file",
        &json!({ "path": format!("{} ", file.to_string_lossy()) }),
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
        &json!({ "path": file.to_string_lossy() }),
    );
    assert_eq!(covered, None);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn argument_aliases_are_consistent_across_describe_ungranted_and_execute() {
    let dir = std::env::temp_dir().join("companion_alias_consistency_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub")).unwrap();
    let sample = dir.join("sub/sample.txt");
    fs::write(&sample, "alias-ok").unwrap();
    let sample_str = sample.to_string_lossy().to_string();

    for alias in ["path", "file_path", "filepath", "file"] {
        let args = json!({ alias: sample_str });
        let preview = describe_tool_action(&[], "read_file", &args);
        assert!(preview.contains(&sample_str), "describe missed {alias}");
        let ungranted = requested_ungranted_path(&[], "read_file", &args);
        assert_eq!(
            ungranted,
            Some(fs::canonicalize(&sample).unwrap()),
            "requested_ungranted_path missed {alias}"
        );
        let out = execute_tool_with_grants(
            &[GrantedFolder {
                path: dir.clone(),
                allow_write: false,
            }],
            "read_file",
            &args,
        )
        .unwrap();
        assert_eq!(out, "alias-ok");
    }

    for alias in ["path", "directory"] {
        let dir_args = json!({ alias: "new_dir" });
        assert!(describe_tool_action(&[], "create_directory", &dir_args).contains("new_dir"));
        assert!(describe_tool_action(&[], "delete_directory", &dir_args).contains("new_dir"));
    }

    let _ = fs::remove_dir_all(&dir);
}
