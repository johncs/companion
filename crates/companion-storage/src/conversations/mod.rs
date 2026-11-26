//! Conversation persistence and FTS5 full-text search.

mod duplicate;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use companion_core::ThinkingLevel;

use crate::Database;
use crate::entities::{Conversation, NewConversation};

const CONVERSATION_COLUMNS: &str = "id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled";

impl Database {
    /// List all conversations ordered by pinned state and recent activity.
    pub fn list_conversations(&self, limit: usize) -> Result<Vec<Conversation>> {
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(|conn| {
            let sql = format!(
                "SELECT {CONVERSATION_COLUMNS}
                 FROM conversations
                 ORDER BY pinned DESC, updated_at DESC
                 LIMIT ?1"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(params![limit_i64], map_conversation)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
    }

    /// Fetch a single conversation by ID.
    pub fn get_conversation(&self, id: &str) -> Result<Option<Conversation>> {
        self.with_conn(|conn| {
            let sql = format!("SELECT {CONVERSATION_COLUMNS} FROM conversations WHERE id = ?1");
            conn.prepare_cached(&sql)?
                .query_row(params![id], map_conversation)
                .optional()
                .map_err(Into::into)
        })
    }

    /// Create a new conversation from explicit caller-resolved settings.
    pub fn create_conversation(&self, new: NewConversation<'_>) -> Result<Conversation> {
        let now = Utc::now().timestamp_millis();
        let conv = Conversation {
            id: Uuid::new_v4().to_string(),
            title: new.title.trim().to_string(),
            model_id: new.model_id.map(str::to_string),
            persona_id: new.persona_id.map(str::to_string),
            system_prompt: new.system_prompt,
            thinking_level: new.thinking_level,
            web_search_enabled: new.web_search_enabled,
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
    pub fn duplicate_conversation(
        &self,
        source_id: &str,
        new_title: &str,
    ) -> Result<Option<Conversation>> {
        self.with_tx(|tx| {
            let Some(source) = load_source_conversation(tx, source_id)? else {
                return Ok(None);
            };
            let copy = build_conversation_copy(&source, new_title);
            insert_conversation_row(tx, &copy)?;

            let mut msg_id_map = duplicate::duplicate_messages(tx, source_id, &copy.id)?;
            duplicate::duplicate_attachments(tx, source_id, &mut msg_id_map)?;
            duplicate::duplicate_tool_calls(tx, source_id, &msg_id_map)?;
            duplicate::duplicate_folders(tx, source_id, &copy.id)?;

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
}

fn load_source_conversation(
    tx: &rusqlite::Transaction<'_>,
    source_id: &str,
) -> Result<Option<Conversation>> {
    Ok(tx
        .query_row(
            "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
             FROM conversations WHERE id = ?1",
            params![source_id],
            map_conversation,
        )
        .optional()?)
}

fn build_conversation_copy(source: &Conversation, new_title: &str) -> Conversation {
    let now = Utc::now().timestamp_millis();
    Conversation {
        id: Uuid::new_v4().to_string(),
        title: new_title.trim().to_string(),
        model_id: source.model_id.clone(),
        persona_id: source.persona_id.clone(),
        system_prompt: source.system_prompt.clone(),
        thinking_level: source.thinking_level,
        web_search_enabled: source.web_search_enabled,
        pinned: false,
        created_at: now,
        updated_at: now,
    }
}

fn insert_conversation_row(tx: &rusqlite::Transaction<'_>, copy: &Conversation) -> Result<()> {
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
    Ok(())
}

impl Database {
    /// Search conversations by title or message content via FTS5.
    pub fn search_conversations(&self, query: &str, limit: usize) -> Result<Vec<Conversation>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let like_pattern = format!(
            "%{}%",
            trimmed
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let fts_query = build_fts_query(trimmed);
        let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

        self.with_conn(|conn| {
            if let Some(fts) = fts_query {
                let sql = format!(
                    "SELECT {CONVERSATION_COLUMNS}
                     FROM conversations
                     WHERE title LIKE ?1 ESCAPE '\\'
                        OR id IN (
                            SELECT conversation_id FROM messages_fts WHERE messages_fts MATCH ?2
                        )
                     ORDER BY pinned DESC, updated_at DESC
                     LIMIT ?3"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let rows =
                    stmt.query_map(params![like_pattern, fts, limit_i64], map_conversation)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            } else {
                let sql = format!(
                    "SELECT {CONVERSATION_COLUMNS}
                     FROM conversations
                     WHERE title LIKE ?1 ESCAPE '\\'
                     ORDER BY pinned DESC, updated_at DESC
                     LIMIT ?2"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let rows = stmt.query_map(params![like_pattern, limit_i64], map_conversation)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            }
        })
    }
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
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}
