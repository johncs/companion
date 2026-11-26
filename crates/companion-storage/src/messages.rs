//! Message persistence and paginated retrieval for virtualized chat scrolling.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, Transaction, params};
use uuid::Uuid;

use companion_core::ThinkingLevel;
use companion_core::types::{ExecutionDevice, MessageRole};

use super::Database;
use super::agent::insert_tool_call_conn;
use super::entities::{Message, NewMessage, ToolCallRecord};

const MESSAGE_COLUMNS: &str = "id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at";

fn insert_message_tx(tx: &Transaction<'_>, new: NewMessage<'_>, now: i64) -> Result<Message> {
    let msg = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: new.conversation_id.to_string(),
        role: new.role,
        content: new.content.to_string(),
        reasoning_content: new.reasoning_content.map(str::to_string),
        thinking_level: new.thinking_level,
        model_name: new.model_name.map(str::to_string),
        execution_device: new.execution_device,
        token_count: new.token_count,
        generation_ms: new.generation_ms,
        created_at: now,
    };
    tx.prepare_cached(
        "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        msg.id,
        msg.conversation_id,
        msg.role.as_str(),
        msg.content,
        msg.reasoning_content,
        msg.thinking_level.map(ThinkingLevel::as_str),
        msg.model_name,
        msg.execution_device.map(ExecutionDevice::as_str),
        msg.token_count,
        msg.generation_ms,
        msg.created_at,
    ])?;
    tx.prepare_cached("UPDATE conversations SET updated_at = ?1 WHERE id = ?2")?
        .execute(params![now, new.conversation_id])?;
    Ok(msg)
}

impl Database {
    /// Insert a message into a conversation and bump its `updated_at` timestamp.
    pub fn insert_message(&self, new: NewMessage<'_>) -> Result<Message> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| insert_message_tx(tx, new, now))
    }

    /// Atomically insert a message and its associated tool calls in a single transaction.
    pub fn insert_message_with_tool_calls(
        &self,
        new: NewMessage<'_>,
        tool_calls: &[ToolCallRecord],
    ) -> Result<Message> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| {
            let msg = insert_message_tx(tx, new, now)?;
            for tc in tool_calls {
                let mut linked = tc.clone();
                linked.message_id = msg.id.clone();
                insert_tool_call_conn(tx, &linked)?;
            }
            Ok(msg)
        })
    }

    /// Fetch the most recent `limit` messages (optionally before `before_message_id`) in chronological order.
    pub fn list_messages_page(
        &self,
        conversation_id: &str,
        before_message_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let mut items = match before_message_id {
                Some(cursor_id) => {
                    query_messages_before_cursor(conn, conversation_id, cursor_id, limit_i64)?
                }
                None => query_latest_messages(conn, conversation_id, limit_i64)?,
            };
            items.reverse();
            Ok(items)
        })
    }
}

fn query_messages_before_cursor(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    cursor_id: &str,
    limit_i64: i64,
) -> Result<Vec<Message>> {
    let cursor: Option<(i64, i64)> = conn
        .prepare_cached(
            "SELECT created_at, rowid FROM messages WHERE id = ?1 AND conversation_id = ?2",
        )?
        .query_row(params![cursor_id, conversation_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((cursor_ts, cursor_rowid)) = cursor else {
        return Ok(Vec::new());
    };
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS}
         FROM messages
         WHERE conversation_id = ?1
           AND (created_at < ?2 OR (created_at = ?2 AND rowid < ?3))
         ORDER BY created_at DESC, rowid DESC
         LIMIT ?4"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![conversation_id, cursor_ts, cursor_rowid, limit_i64],
        map_message,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_latest_messages(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    limit_i64: i64,
) -> Result<Vec<Message>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS}
         FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at DESC, rowid DESC
         LIMIT ?2"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![conversation_id, limit_i64], map_message)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl Database {
    /// Update a user message's content and delete all messages in the conversation after it.
    /// Returns without deleting anything if `message_id` is not found in `conversation_id`.
    pub fn edit_message_and_truncate_after(
        &self,
        conversation_id: &str,
        message_id: &str,
        new_content: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| {
            let Some((target_ts, target_rowid)): Option<(i64, i64)> = tx
                .prepare_cached(
                    "SELECT created_at, rowid FROM messages WHERE id = ?1 AND conversation_id = ?2",
                )?
                .query_row(params![message_id, conversation_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
            else {
                return Ok(());
            };
            tx.prepare_cached(
                "UPDATE messages SET content = ?1 WHERE id = ?2 AND conversation_id = ?3",
            )?
            .execute(params![new_content, message_id, conversation_id])?;
            tx.prepare_cached(
                "DELETE FROM messages
                 WHERE conversation_id = ?1
                   AND (created_at > ?2 OR (created_at = ?2 AND rowid > ?3))",
            )?
            .execute(params![conversation_id, target_ts, target_rowid])?;
            tx.prepare_cached("UPDATE conversations SET updated_at = ?1 WHERE id = ?2")?
                .execute(params![now, conversation_id])?;
            Ok(())
        })
    }

    /// Delete a message and all messages in the conversation after it.
    /// Returns without deleting anything if `message_id` is not found in `conversation_id`.
    pub fn delete_message_and_after(&self, conversation_id: &str, message_id: &str) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| {
            let Some((target_ts, target_rowid)): Option<(i64, i64)> = tx
                .prepare_cached(
                    "SELECT created_at, rowid FROM messages WHERE id = ?1 AND conversation_id = ?2",
                )?
                .query_row(params![message_id, conversation_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
            else {
                return Ok(());
            };
            tx.prepare_cached(
                "DELETE FROM messages
                 WHERE conversation_id = ?1
                   AND (created_at > ?2 OR (created_at = ?2 AND rowid >= ?3))",
            )?
            .execute(params![conversation_id, target_ts, target_rowid])?;
            tx.prepare_cached("UPDATE conversations SET updated_at = ?1 WHERE id = ?2")?
                .execute(params![now, conversation_id])?;
            Ok(())
        })
    }
}

fn map_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    let role_raw: String = row.get(2)?;
    let thinking_raw: Option<String> = row.get(5)?;
    let device_raw: Option<String> = row.get(7)?;
    Ok(Message {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        role: MessageRole::from_str_lossy(&role_raw),
        content: row.get(3)?,
        reasoning_content: row.get(4)?,
        thinking_level: thinking_raw.as_deref().map(ThinkingLevel::from_str_lossy),
        model_name: row.get(6)?,
        execution_device: device_raw.as_deref().map(ExecutionDevice::from_str_lossy),
        token_count: row.get(8)?,
        generation_ms: row.get(9)?,
        created_at: row.get(10)?,
    })
}
