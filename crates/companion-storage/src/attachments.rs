//! Attachment persistence queries.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Row, params};
use uuid::Uuid;

use companion_core::types::StagedAttachment;

use super::Database;
use super::entities::AttachmentRecord;

const SQLITE_VAR_CHUNK: usize = 500;

impl Database {
    /// Persist one or more staged attachments linked to `message_id` and return saved records.
    pub fn insert_attachments(
        &self,
        message_id: &str,
        attachments: &[StagedAttachment],
    ) -> Result<Vec<AttachmentRecord>> {
        if attachments.is_empty() {
            return Ok(Vec::new());
        }
        self.with_tx(|tx| {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO attachments (id, message_id, file_name, mime_type, file_path, extracted_text, byte_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut saved = Vec::with_capacity(attachments.len());
            for att in attachments {
                let id = Uuid::new_v4().to_string();
                stmt.execute(params![
                    id,
                    message_id,
                    att.file_name,
                    att.mime_type,
                    att.file_path,
                    att.extracted_text,
                    att.byte_size,
                ])?;
                saved.push(AttachmentRecord {
                    id,
                    message_id: message_id.to_string(),
                    file_name: att.file_name.clone(),
                    mime_type: att.mime_type.clone(),
                    file_path: att.file_path.clone(),
                    extracted_text: att.extracted_text.clone(),
                    byte_size: att.byte_size,
                });
            }
            Ok(saved)
        })
    }

    /// Batch-fetch attachments for many `message_ids` in chunked queries (avoids N+1 and variable limits).
    pub fn list_attachments_for_messages(
        &self,
        message_ids: &[&str],
    ) -> Result<HashMap<String, Vec<AttachmentRecord>>> {
        let mut map: HashMap<String, Vec<AttachmentRecord>> = HashMap::new();
        if message_ids.is_empty() {
            return Ok(map);
        }

        self.with_conn(|conn| {
            for chunk in message_ids.chunks(SQLITE_VAR_CHUNK) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT id, message_id, file_name, mime_type, file_path, extracted_text, byte_size
                     FROM attachments
                     WHERE message_id IN ({placeholders})
                     ORDER BY rowid ASC"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows =
                    stmt.query_map(rusqlite::params_from_iter(chunk.iter()), map_attachment)?;
                for row in rows {
                    let att = row?;
                    map.entry(att.message_id.clone()).or_default().push(att);
                }
            }
            Ok(map)
        })
    }
}

fn map_attachment(row: &Row<'_>) -> rusqlite::Result<AttachmentRecord> {
    Ok(AttachmentRecord {
        id: row.get(0)?,
        message_id: row.get(1)?,
        file_name: row.get(2)?,
        mime_type: row.get(3)?,
        file_path: row.get(4)?,
        extracted_text: row.get(5)?,
        byte_size: row.get(6)?,
    })
}
