//! Attachment persistence queries.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Row, params};

use super::Database;
use super::entities::AttachmentRecord;

impl Database {
    /// Persist one or more attachments linked to `message_id`.
    pub fn insert_attachments(
        &self,
        message_id: &str,
        attachments: &[AttachmentRecord],
    ) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        self.with_tx(|tx| {
            for att in attachments {
                tx.execute(
                    "INSERT INTO attachments (id, message_id, file_name, mime_type, file_path, extracted_text, byte_size)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        att.id,
                        message_id,
                        att.file_name,
                        att.mime_type,
                        att.file_path,
                        att.extracted_text,
                        att.byte_size,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Batch-fetch attachments for many `message_ids` in a single SQL query (avoids N+1 queries).
    pub fn list_attachments_for_messages(
        &self,
        message_ids: &[&str],
    ) -> Result<HashMap<String, Vec<AttachmentRecord>>> {
        let mut map: HashMap<String, Vec<AttachmentRecord>> = HashMap::new();
        if message_ids.is_empty() {
            return Ok(map);
        }

        let placeholders = vec!["?"; message_ids.len()].join(",");
        let sql = format!(
            "SELECT id, message_id, file_name, mime_type, file_path, extracted_text, byte_size
             FROM attachments
             WHERE message_id IN ({placeholders})"
        );

        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(message_ids.iter()), map_attachment)?;
            for row in rows {
                let att = row?;
                map.entry(att.message_id.clone()).or_default().push(att);
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
