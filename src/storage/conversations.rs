//! Conversation persistence and FTS5 full-text search.

use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;

use super::Database;
use super::entities::Conversation;

impl Database {
    /// List all conversations ordered by pinned state and recent activity.
    pub fn list_conversations(&self, limit: usize) -> Result<Vec<Conversation>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
                 FROM conversations
                 ORDER BY pinned DESC, updated_at DESC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], map_conversation)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    /// Fetch a single conversation by ID.
    pub fn get_conversation(&self, id: &str) -> Result<Option<Conversation>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
                 FROM conversations WHERE id = ?1",
                params![id],
                map_conversation,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    /// Create a new conversation.
    pub fn create_conversation(&self, title: &str) -> Result<Conversation> {
        let now = Utc::now().timestamp_millis();
        let sys_prompt = default_system_prompt().to_string();
        let default_thinking = self
            .get_active_model_id()
            .ok()
            .flatten()
            .and_then(|id| self.get_model(&id).ok().flatten())
            .map(|m| m.default_thinking_level())
            .unwrap_or_default();
        let conv = Conversation {
            id: Uuid::new_v4().to_string(),
            title: title.trim().to_string(),
            model_id: None,
            persona_id: None,
            system_prompt: Some(sys_prompt),
            thinking_level: default_thinking,
            web_search_enabled: false,
            pinned: false,
            created_at: now,
            updated_at: now,
        };
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    conv.id,
                    conv.title,
                    conv.model_id,
                    conv.persona_id,
                    conv.pinned as i64,
                    conv.created_at,
                    conv.updated_at,
                    conv.system_prompt,
                    conv.thinking_level.as_str(),
                    conv.web_search_enabled as i64,
                ],
            )?;
            Ok(conv)
        })
    }

    /// Persist whether web search is enabled for a conversation.
    pub fn set_conversation_web_search_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET web_search_enabled = ?1 WHERE id = ?2",
                params![enabled as i64, id],
            )?;
            Ok(())
        })
    }

    /// Persist the selected thinking level for a conversation.
    pub fn set_conversation_thinking_level(
        &self,
        id: &str,
        thinking_level: ThinkingLevel,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET thinking_level = ?1 WHERE id = ?2",
                params![thinking_level.as_str(), id],
            )?;
            Ok(())
        })
    }

    /// Update the custom system prompt for a conversation.
    pub fn set_conversation_system_prompt(
        &self,
        id: &str,
        system_prompt: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET system_prompt = ?1 WHERE id = ?2",
                params![system_prompt, id],
            )?;
            Ok(())
        })
    }

    /// Rename a conversation and bump its `updated_at` timestamp.
    pub fn rename_conversation(&self, id: &str, new_title: &str) -> Result<()> {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![trimmed, now, id],
            )?;
            Ok(())
        })
    }

    /// Duplicate a conversation along with its messages (including metrics), attachments, tool calls, and folders.
    pub fn duplicate_conversation(&self, source_id: &str) -> Result<Option<Conversation>> {
        self.with_tx(|tx| {
            let Some(source) = tx
                .query_row(
                    "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
                     FROM conversations WHERE id = ?1",
                    params![source_id],
                    map_conversation,
                )
                .optional()?
            else {
                return Ok(None);
            };

            let now = Utc::now().timestamp_millis();
            let copy = Conversation {
                id: Uuid::new_v4().to_string(),
                title: format!("{} (Copy)", source.title),
                model_id: source.model_id.clone(),
                persona_id: source.persona_id.clone(),
                system_prompt: source.system_prompt.clone(),
                thinking_level: source.thinking_level,
                web_search_enabled: source.web_search_enabled,
                pinned: false,
                created_at: now,
                updated_at: now,
            };

            tx.execute(
                "INSERT INTO conversations (id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    copy.id,
                    copy.title,
                    copy.model_id,
                    copy.persona_id,
                    0i64,
                    copy.created_at,
                    copy.updated_at,
                    copy.system_prompt,
                    copy.thinking_level.as_str(),
                    copy.web_search_enabled as i64,
                ],
            )?;

            let mut msg_id_map = duplicate_messages(tx, source_id, &copy.id)?;
            duplicate_attachments(tx, source_id, &mut msg_id_map)?;
            duplicate_tool_calls(tx, source_id, &msg_id_map)?;
            duplicate_folders(tx, source_id, &copy.id)?;

            Ok(Some(copy))
        })
    }

    /// Delete a conversation and cascade-delete its messages.
    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// Search conversations by title or message content via FTS5.
    pub fn search_conversations(&self, query: &str, limit: usize) -> Result<Vec<Conversation>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let like_pattern = format!("%{}%", trimmed.replace('%', "\\%").replace('_', "\\_"));
        let fts_query = build_fts_query(trimmed);

        self.with_conn(|conn| {
            if let Some(fts) = fts_query {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT c.id, c.title, c.model_id, c.persona_id, c.pinned, c.created_at, c.updated_at, c.system_prompt, c.thinking_level, c.web_search_enabled
                     FROM conversations c
                     WHERE c.title LIKE ?1 ESCAPE '\\'
                        OR c.id IN (
                            SELECT conversation_id FROM messages_fts WHERE messages_fts MATCH ?2
                        )
                     ORDER BY c.pinned DESC, c.updated_at DESC
                     LIMIT ?3",
                )?;
                let rows =
                    stmt.query_map(params![like_pattern, fts, limit as i64], map_conversation)?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT c.id, c.title, c.model_id, c.persona_id, c.pinned, c.created_at, c.updated_at, c.system_prompt, c.thinking_level, c.web_search_enabled
                     FROM conversations c
                     WHERE c.title LIKE ?1 ESCAPE '\\'
                     ORDER BY c.pinned DESC, c.updated_at DESC
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![like_pattern, limit as i64], map_conversation)?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
            }
        })
    }
}

type MsgRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<i64>,
    i64,
);

fn duplicate_messages(
    tx: &rusqlite::Transaction<'_>,
    source_id: &str,
    copy_id: &str,
) -> Result<HashMap<String, String>> {
    let mut stmt = tx.prepare(
        "SELECT id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at
         FROM messages
         WHERE conversation_id = ?1
         ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows: Vec<MsgRow> = stmt
        .query_map(params![source_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut id_map = HashMap::with_capacity(rows.len());
    for (old_id, role, content, reasoning, thinking, model_name, device, tokens, gen_ms, created_at) in
        rows
    {
        let new_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, model_name, execution_device, token_count, generation_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id,
                copy_id,
                role,
                content,
                reasoning,
                thinking,
                model_name,
                device,
                tokens,
                gen_ms,
                created_at,
            ],
        )?;
        id_map.insert(old_id, new_id);
    }
    Ok(id_map)
}

fn duplicate_attachments(
    tx: &rusqlite::Transaction<'_>,
    source_id: &str,
    msg_id_map: &mut HashMap<String, String>,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT a.message_id, a.file_name, a.mime_type, a.file_path, a.extracted_text, a.byte_size
         FROM attachments a
         JOIN messages m ON m.id = a.message_id
         WHERE m.conversation_id = ?1",
    )?;
    let rows: Vec<(String, String, String, String, Option<String>, i64)> = stmt
        .query_map(params![source_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (old_msg_id, file_name, mime_type, file_path, extracted_text, byte_size) in rows {
        if let Some(new_msg_id) = msg_id_map.get(&old_msg_id) {
            tx.execute(
                "INSERT INTO attachments (id, message_id, file_name, mime_type, file_path, extracted_text, byte_size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    new_msg_id,
                    file_name,
                    mime_type,
                    file_path,
                    extracted_text,
                    byte_size,
                ],
            )?;
        }
    }
    Ok(())
}

fn duplicate_tool_calls(
    tx: &rusqlite::Transaction<'_>,
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
    let rows: Vec<(String, String, String, String, Option<String>, String, i64)> = stmt
        .query_map(params![source_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for (old_msg_id, tool_name, arguments_json, status, output, decision, created_at) in rows {
        if let Some(new_msg_id) = msg_id_map.get(&old_msg_id) {
            tx.execute(
                "INSERT INTO tool_calls (id, message_id, tool_name, arguments_json, status, output, permission_decision, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    new_msg_id,
                    tool_name,
                    arguments_json,
                    status,
                    output,
                    decision,
                    created_at,
                ],
            )?;
        }
    }
    Ok(())
}

fn duplicate_folders(
    tx: &rusqlite::Transaction<'_>,
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

fn map_conversation(row: &Row<'_>) -> rusqlite::Result<Conversation> {
    let pinned: i64 = row.get(4)?;
    let thinking_raw: String = row.get(8)?;
    let web_search: i64 = row.get(9)?;
    Ok(Conversation {
        id: row.get(0)?,
        title: row.get(1)?,
        model_id: row.get(2)?,
        persona_id: row.get(3)?,
        pinned: pinned != 0,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        system_prompt: row.get(7)?,
        thinking_level: ThinkingLevel::from_str_lossy(&thinking_raw),
        web_search_enabled: web_search != 0,
    })
}

fn build_fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}
