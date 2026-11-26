use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Transaction, params};
use uuid::Uuid;

struct DupMessageRow {
    old_id: String,
    role: String,
    content: String,
    reasoning: Option<String>,
    thinking: Option<String>,
    model_name: Option<String>,
    device: Option<String>,
    tokens: i64,
    gen_ms: Option<i64>,
    created_at: i64,
}

struct DupAttachmentRow {
    old_msg_id: String,
    file_name: String,
    mime_type: String,
    file_path: String,
    extracted_text: Option<String>,
    byte_size: i64,
}

struct DupToolCallRow {
    old_msg_id: String,
    tool_name: String,
    arguments_json: String,
    status: String,
    output: Option<String>,
    decision: String,
    created_at: i64,
}

pub(super) fn duplicate_messages(
    tx: &Transaction<'_>,
    source_id: &str,
    copy_id: &str,
) -> Result<HashMap<String, String>> {
    let mut stmt = tx.prepare(
        "SELECT id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at
         FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows: Vec<DupMessageRow> = stmt
        .query_map(params![source_id], |row| {
            Ok(DupMessageRow {
                old_id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                reasoning: row.get(3)?,
                thinking: row.get(4)?,
                model_name: row.get(5)?,
                device: row.get(6)?,
                tokens: row.get(7)?,
                gen_ms: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut id_map = HashMap::with_capacity(rows.len());
    for row in rows {
        let new_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id,
                copy_id,
                row.role,
                row.content,
                row.reasoning,
                row.thinking,
                row.model_name,
                row.device,
                row.tokens,
                row.gen_ms,
                row.created_at,
            ],
        )?;
        id_map.insert(row.old_id, new_id);
    }
    Ok(id_map)
}

pub(super) fn duplicate_attachments(
    tx: &Transaction<'_>,
    source_id: &str,
    msg_id_map: &mut HashMap<String, String>,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT a.message_id, a.file_name, a.mime_type, a.file_path, a.extracted_text, a.byte_size
         FROM attachments a
         JOIN messages m ON m.id = a.message_id
         WHERE m.conversation_id = ?1",
    )?;
    let rows: Vec<DupAttachmentRow> = stmt
        .query_map(params![source_id], |row| {
            Ok(DupAttachmentRow {
                old_msg_id: row.get(0)?,
                file_name: row.get(1)?,
                mime_type: row.get(2)?,
                file_path: row.get(3)?,
                extracted_text: row.get(4)?,
                byte_size: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for row in rows {
        if let Some(new_msg_id) = msg_id_map.get(&row.old_msg_id) {
            tx.execute(
                "INSERT INTO attachments (id, message_id, file_name, mime_type, file_path, extracted_text, byte_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    new_msg_id,
                    row.file_name,
                    row.mime_type,
                    row.file_path,
                    row.extracted_text,
                    row.byte_size,
                ],
            )?;
        }
    }
    Ok(())
}

pub(super) fn duplicate_tool_calls(
    tx: &Transaction<'_>,
    source_id: &str,
    msg_id_map: &HashMap<String, String>,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT t.message_id, t.tool_name, t.arguments_json, t.status, t.output, t.permission_decision, t.created_at
         FROM tool_calls t
         JOIN messages m ON m.id = t.message_id
         WHERE m.conversation_id = ?1
         ORDER BY t.created_at ASC",
    )?;
    let rows: Vec<DupToolCallRow> = stmt
        .query_map(params![source_id], |row| {
            Ok(DupToolCallRow {
                old_msg_id: row.get(0)?,
                tool_name: row.get(1)?,
                arguments_json: row.get(2)?,
                status: row.get(3)?,
                output: row.get(4)?,
                decision: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for row in rows {
        if let Some(new_msg_id) = msg_id_map.get(&row.old_msg_id) {
            tx.execute(
                "INSERT INTO tool_calls (id, message_id, tool_name, arguments_json, status, output, permission_decision, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    new_msg_id,
                    row.tool_name,
                    row.arguments_json,
                    row.status,
                    row.output,
                    row.decision,
                    row.created_at,
                ],
            )?;
        }
    }
    Ok(())
}

pub(super) fn duplicate_folders(
    tx: &Transaction<'_>,
    source_id: &str,
    copy_id: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT folder_path, permission_mode, created_at
         FROM conversation_folders
         WHERE conversation_id = ?1
         ORDER BY created_at ASC, rowid ASC",
    )?;
    let folders: Vec<(String, String, i64)> = stmt
        .query_map(params![source_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (folder_path, permission_mode, created_at) in folders {
        tx.execute(
            "INSERT INTO conversation_folders (id, conversation_id, folder_path, permission_mode, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().to_string(),
                copy_id,
                folder_path,
                permission_mode,
                created_at,
            ],
        )?;
    }
    Ok(())
}
