//! Message persistence and paginated retrieval for virtualized chat scrolling.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Row, params};
use uuid::Uuid;

use companion_core::ThinkingLevel;
use companion_core::types::{ExecutionDevice, MessageRole};

use super::Database;
use super::entities::{Message, NewMessage};

impl Database {
    /// Insert a message into a conversation and bump its `updated_at` timestamp.
    pub fn insert_message(&self, new: NewMessage<'_>) -> Result<Message> {
        let now = Utc::now().timestamp_millis();
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

        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
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
                ],
            )?;
            tx.execute(
                "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
                params![now, new.conversation_id],
            )?;
            Ok(msg)
        })
    }

    /// Fetch the most recent `limit` messages (optionally before `before_created_at`) in chronological order.
    #[allow(dead_code)]
    pub fn list_messages_page(
        &self,
        conversation_id: &str,
        before_created_at: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        self.with_conn(|conn| {
            let mut items = if let Some(cursor) = before_created_at {
                let mut stmt = conn.prepare(
                    "SELECT id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at
                     FROM messages
                     WHERE conversation_id = ?1 AND created_at < ?2
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?3",
                )?;
                let rows =
                    stmt.query_map(params![conversation_id, cursor, limit as i64], map_message)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at
                     FROM messages
                     WHERE conversation_id = ?1
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![conversation_id, limit as i64], map_message)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            items.reverse();
            Ok(items)
        })
    }

    /// Update a user message's content and delete all messages in the conversation after it.
    pub fn edit_message_and_truncate_after(
        &self,
        conversation_id: &str,
        message_id: &str,
        created_at: i64,
        new_content: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| {
            tx.execute(
                "UPDATE messages SET content = ?1 WHERE id = ?2 AND conversation_id = ?3",
                params![new_content, message_id, conversation_id],
            )?;
            tx.execute(
                "DELETE FROM messages
                 WHERE conversation_id = ?1
                   AND (
                       created_at > ?2
                       OR (created_at = ?2 AND rowid > COALESCE((SELECT rowid FROM messages WHERE id = ?3), 0))
                   )",
                params![conversation_id, created_at, message_id],
            )?;
            tx.execute(
                "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
                params![now, conversation_id],
            )?;
            Ok(())
        })
    }

    /// Delete a message and all messages in the conversation after it.
    pub fn delete_message_and_after(
        &self,
        conversation_id: &str,
        message_id: &str,
        created_at: i64,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.with_tx(|tx| {
            tx.execute(
                "DELETE FROM messages
                 WHERE conversation_id = ?1
                   AND (
                       created_at > ?2
                       OR (created_at = ?2 AND rowid >= COALESCE((SELECT rowid FROM messages WHERE id = ?3), 0))
                   )",
                params![conversation_id, created_at, message_id],
            )?;
            tx.execute(
                "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
                params![now, conversation_id],
            )?;
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
