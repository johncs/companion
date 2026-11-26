use std::fs;

use super::GrantedFolder;
use super::find_covering_granted_folder;
use super::path_guard::{
    resolve_granted_root, resolve_ungranted_raw, resolve_write_folder_for_raw, sanitize_tool_path,
};

#[test]
fn write_target_resolution_never_escalates_unrelated_readonly_folder() {
    let base = std::env::temp_dir().join("companion_exact_write_target_test");
    let _ = fs::remove_dir_all(&base);
    let folder_a = base.join("folder_a");
    let folder_b = base.join("folder_b");
    fs::create_dir_all(&folder_a).unwrap();
    fs::create_dir_all(&folder_b).unwrap();

    let grants = vec![
        GrantedFolder {
            path: folder_a.clone(),
            allow_write: false,
        },
        GrantedFolder {
            path: folder_b.clone(),
            allow_write: false,
        },
    ];

    let target_in_a = folder_a.join("out.txt");
    let resolved = resolve_write_folder_for_raw(&grants, &target_in_a.to_string_lossy(), None);
    assert_eq!(resolved, Some(folder_a.clone()));
    assert_ne!(resolved, Some(folder_b.clone()));

    let dir_in_a = folder_a.join("subdir");
    assert_eq!(
        resolve_write_folder_for_raw(&grants, &dir_in_a.to_string_lossy(), None),
        Some(folder_a.clone())
    );

    let from_in_a = folder_a.join("old.txt");
    fs::write(&from_in_a, "x").unwrap();
    assert_eq!(
        resolve_write_folder_for_raw(&grants, &from_in_a.to_string_lossy(), None),
        Some(folder_a.clone())
    );

    let _ = fs::remove_dir_all(&base);
}

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

    assert!(resolve_granted_root(&grants, "file.txt", None, false).is_err());

    let (root, rel) =
        resolve_granted_root(&grants, "file.txt", Some(&sub_a.to_string_lossy()), false).unwrap();
    assert_eq!(root, fs::canonicalize(&dir_a).unwrap());
    assert_eq!(rel, "sub/file.txt");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn resolve_ungranted_raw_catches_nonexistent_file_in_ungranted_dir() {
    let base = std::env::temp_dir().join(format!("companion_ug_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    let new_file = base.join("new_sub/file.txt");
    let resolved = resolve_ungranted_raw(&[], Some(&new_file.to_string_lossy()), true);
    assert_eq!(resolved, Some(fs::canonicalize(&base).unwrap()));
    let _ = fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn resolve_granted_path_blocks_dangling_and_escaping_symlinks() {
    use super::path_guard::resolve_granted_path;

    let base = std::env::temp_dir().join(format!("companion_sym_{}", uuid::Uuid::new_v4()));
    let granted = base.join("granted");
    let outside = base.join("outside");
    fs::create_dir_all(&granted).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let dangling_link = granted.join("escape_link");
    std::os::unix::fs::symlink(outside.join("nonexistent.txt"), &dangling_link).unwrap();

    let canon = fs::canonicalize(&granted).unwrap();
    assert!(resolve_granted_path(&canon, "escape_link").is_err());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn resolve_granted_root_allows_nested_nonexistent_directories() {
    let base = std::env::temp_dir().join(format!("companion_nest_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    let canon = fs::canonicalize(&base).unwrap();
    let grants = vec![GrantedFolder {
        path: canon.clone(),
        allow_write: true,
    }];

    let (root, rel) = resolve_granted_root(&grants, "a/b/c/new_file.txt", None, true).unwrap();
    assert_eq!(root, canon);
    assert_eq!(rel, "a/b/c/new_file.txt");

    let _ = fs::remove_dir_all(&base);
}

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

    let entries = vec![companion_storage::ConversationFolder {
        id: "1".to_string(),
        conversation_id: "c1".to_string(),
        folder_path: granted_dir.to_string_lossy().to_string(),
        permission_mode: companion_core::types::FolderPermission::Read,
        created_at: 0,
    }];
    let f = |p: &std::path::Path| find_covering_granted_folder(&entries, p);
    assert_eq!(f(&inside_file).as_deref(), Some("Photos"));
    assert_eq!(f(&sub_dir).as_deref(), Some("Photos"));
    assert_eq!(f(&outside_file), None);

    let _ = fs::remove_dir_all(&root);
}
