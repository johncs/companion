//! Per-conversation granted folder persistence.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use companion_core::types::FolderPermission;

use super::Database;
use super::entities::ConversationFolder;

const FOLDER_COLUMNS: &str = "id, conversation_id, folder_path, permission_mode, created_at";

fn map_folder(row: &Row<'_>) -> rusqlite::Result<ConversationFolder> {
    let mode_raw: String = row.get(3)?;
    Ok(ConversationFolder {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        folder_path: row.get(2)?,
        permission_mode: FolderPermission::from_str_lossy(&mode_raw),
        created_at: row.get(4)?,
    })
}

impl Database {
    /// Grant folder access to a conversation (deduplicated by `folder_path`, defaults to `Read`).
    pub fn add_conversation_folder(
        &self,
        conversation_id: &str,
        folder_path: &str,
    ) -> Result<ConversationFolder> {
        self.with_tx(|tx| {
            let select_sql = format!(
                "SELECT {FOLDER_COLUMNS} FROM conversation_folders
                 WHERE conversation_id = ?1 AND folder_path = ?2"
            );
            if let Some(existing) = tx
                .prepare_cached(&select_sql)?
                .query_row(params![conversation_id, folder_path], map_folder)
                .optional()?
            {
                return Ok(existing);
            }

            let entry = ConversationFolder {
                id: Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                folder_path: folder_path.to_string(),
                permission_mode: FolderPermission::Read,
                created_at: Utc::now().timestamp_millis(),
            };
            tx.prepare_cached(
                "INSERT INTO conversation_folders (id, conversation_id, folder_path, permission_mode, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                entry.id,
                entry.conversation_id,
                entry.folder_path,
                entry.permission_mode.as_str(),
                entry.created_at,
            ])?;
            Ok(entry)
        })
    }

    /// List all granted folders for a conversation in insertion order.
    pub fn list_conversation_folders(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<ConversationFolder>> {
        self.with_conn(|conn| {
            let sql = format!(
                "SELECT {FOLDER_COLUMNS} FROM conversation_folders
                 WHERE conversation_id = ?1
                 ORDER BY created_at ASC"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(params![conversation_id], map_folder)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
    }

    /// Update a granted folder's permission mode (`FolderPermission::Read` or `ReadWrite`).
    pub fn set_conversation_folder_permission(
        &self,
        id: &str,
        permission_mode: FolderPermission,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversation_folders SET permission_mode = ?1 WHERE id = ?2",
                params![permission_mode.as_str(), id],
            )?;
            Ok(())
        })
    }

    /// Revoke a granted folder from a conversation.
    pub fn remove_conversation_folder(&self, id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM conversation_folders WHERE id = ?1",
                params![id],
            )?;
            Ok(())
        })
    }
}
