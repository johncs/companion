//! Message persistence and paginated retrieval for virtualized chat scrolling.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Row, params};
use uuid::Uuid;

use super::Database;
use super::entities::Message;

impl Database {
    /// Insert a message into a conversation and bump its `updated_at` timestamp.
    pub fn insert_message(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
        reasoning_content: Option<&str>,
        thinking_level: Option<&str>,
    ) -> Result<Message> {
        self.insert_message_with_metrics(
            conversation_id,
            role,
            content,
            reasoning_content,
            thinking_level,
            None,
            None,
            0,
            None,
        )
    }

    /// Insert a message along with model name, completion token count, and generation duration (ms).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_message_with_metrics(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
        reasoning_content: Option<&str>,
        thinking_level: Option<&str>,
        model_name: Option<&str>,
        execution_device: Option<&str>,
        token_count: i64,
        generation_ms: Option<i64>,
    ) -> Result<Message> {
        let now = Utc::now().timestamp_millis();
        let msg = Message {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            reasoning_content: reasoning_content.map(str::to_string),
            thinking_level: thinking_level.map(str::to_string),
            model_name: model_name.map(str::to_string),
            execution_device: execution_device.map(str::to_string),
            token_count,
            generation_ms,
            created_at: now,
        };

        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                msg.id,
                msg.conversation_id,
                msg.role,
                msg.content,
                msg.reasoning_content,
                msg.thinking_level,
                msg.model_name,
                msg.execution_device,
                msg.token_count,
                msg.generation_ms,
                msg.created_at,
            ],
        )?;
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, conversation_id],
        )?;
        Ok(msg)
    }

    /// Fetch the most recent `limit` messages (optionally before `before_created_at`) in chronological order.
    #[allow(dead_code)]
    pub fn list_messages_page(
        &self,
        conversation_id: &str,
        before_created_at: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut items = if let Some(cursor) = before_created_at {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at
                 FROM messages
                 WHERE conversation_id = ?1 AND created_at < ?2
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![conversation_id, cursor, limit as i64], map_message)?;
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
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let tx = conn.transaction()?;
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
        tx.commit()?;
        Ok(())
    }

    /// Delete a message and all messages in the conversation after it.
    pub fn delete_message_and_after(
        &self,
        conversation_id: &str,
        message_id: &str,
        created_at: i64,
    ) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let tx = conn.transaction()?;
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
        tx.commit()?;
        Ok(())
    }
}

fn map_message(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        reasoning_content: row.get(4)?,
        thinking_level: row.get(5)?,
        model_name: row.get(6)?,
        execution_device: row.get(7)?,
        token_count: row.get(8)?,
        generation_ms: row.get(9)?,
        created_at: row.get(10)?,
    })
}
