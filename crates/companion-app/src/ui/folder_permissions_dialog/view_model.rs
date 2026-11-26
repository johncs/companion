//! Pure, GTK-free partitioning for Access Manager folder and file grants.

use std::path::Path;

use crate::storage::ConversationFolder;

/// Classified folder or file grant entry with on-disk existence metadata.
pub(super) struct GrantItem {
    pub(super) entry: ConversationFolder,
    pub(super) exists: bool,
    pub(super) is_file: bool,
}

/// Partition persisted grants into `(file_entries, dir_entries)`.
pub(super) fn partition_grants(
    entries: Vec<ConversationFolder>,
) -> (Vec<GrantItem>, Vec<GrantItem>) {
    entries
        .into_iter()
        .map(|entry| {
            let p = Path::new(&entry.folder_path);
            let exists = p.exists();
            let is_file = if exists {
                p.is_file()
            } else {
                p.extension().is_some()
            };
            GrantItem {
                entry,
                exists,
                is_file,
            }
        })
        .partition(|i| i.is_file)
}

#[cfg(test)]
mod tests {
    use super::partition_grants;
    use crate::core::types::FolderPermission;
    use crate::storage::ConversationFolder;

    fn folder(id: &str, path: &str) -> ConversationFolder {
        ConversationFolder {
            id: id.into(),
            conversation_id: "c1".into(),
            folder_path: path.into(),
            permission_mode: FolderPermission::Read,
            created_at: 0,
        }
    }

    #[test]
    fn partitions_existing_and_missing_files_vs_directories() {
        let base = std::env::temp_dir().join(format!("companion_fp_vm_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("notes.txt");
        std::fs::write(&file, "hi").unwrap();

        let (files, dirs) = partition_grants(vec![
            folder("1", &base.to_string_lossy()),
            folder("2", &file.to_string_lossy()),
            folder("3", "/nonexistent/missing.pdf"),
            folder("4", "/nonexistent/missing_dir"),
        ]);

        assert_eq!(files.len(), 2);
        assert!(files[0].exists);
        assert!(!files[1].exists);
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].exists);
        assert!(!dirs[1].exists);

        let _ = std::fs::remove_dir_all(&base);
    }
}
