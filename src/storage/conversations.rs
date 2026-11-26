//! Conversation persistence and FTS5 full-text search.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::core::ThinkingLevel;
use crate::core::types::FolderPermission;

use super::Database;
use super::entities::Conversation;

pub const CORE_SYSTEM_RULES: &str = "Core rules: Be accurate, concise, and honest. If you are unsure or lack information, say so directly instead of guessing. Never fabricate file contents, command outputs, or tool results. Reply in the user's language.";
pub const DEFAULT_SYSTEM_PROMPT: &str = "";

/// Return the default per-chat system prompt template.
pub fn default_system_prompt() -> &'static str {
    DEFAULT_SYSTEM_PROMPT
}

/// Expand `{assistant_name}` and `{user_name}` placeholders (and legacy `"You are Companion,"` defaults).
pub fn render_system_prompt(
    template: &str,
    assistant_name: &str,
    user_name: Option<&str>,
) -> String {
    let mut out = template
        .replace("{assistant_name}", assistant_name)
        .replace("{user_name}", user_name.unwrap_or("the user"));
    if assistant_name != "Companion" && out.starts_with("You are Companion,") {
        out = out.replacen("You are Companion,", &format!("You are {assistant_name},"), 1);
    }
    out
}

/// Compose the layered system prompt: Global Core + Personalization first, followed by per-chat prompt.
/// Compose the full system prompt with core rules, personalized character/name, optional user context,
/// and per-chat instructions.
pub fn compose_personalized_system_prompt(
    chat_prompt: &str,
    assistant_name: &str,
    user_name: Option<&str>,
    personality: crate::core::AssistantPersonality,
    custom_personality: Option<&str>,
    user_about: Option<&str>,
) -> String {
    let effective_assistant = match assistant_name.trim() {
        "" => "Companion",
        s => s,
    };
    let clean_user = user_name.map(str::trim).filter(|s| !s.is_empty());
    let mut layers = vec![
        CORE_SYSTEM_RULES.to_string(),
        format!("Your name is {effective_assistant}."),
    ];
    if let Some(uname) = clean_user {
        layers.push(format!("The user's name is {uname}."));
    }
    layers.push(personality.resolve_guidance(effective_assistant, clean_user, custom_personality));
    if let Some(about) = user_about.map(str::trim).filter(|s| !s.is_empty()) {
        let who = clean_user.unwrap_or("the user");
        layers.push(format!("About {who}: {about}"));
    }

    let rendered_chat = render_system_prompt(chat_prompt, effective_assistant, user_name);
    let trimmed_chat = rendered_chat.trim();
    if !trimmed_chat.is_empty() {
        layers.push(trimmed_chat.to_string());
    }

    layers.join("\n\n")
}

impl Database {
    /// List all conversations ordered by pinned state and recent activity.
    pub fn list_conversations(&self, limit: usize) -> Result<Vec<Conversation>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
             FROM conversations
             ORDER BY pinned DESC, updated_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], map_conversation)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Fetch a single conversation by ID.
    pub fn get_conversation(&self, id: &str) -> Result<Option<Conversation>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row(
            "SELECT id, title, model_id, persona_id, pinned, created_at, updated_at, system_prompt, thinking_level, web_search_enabled
             FROM conversations WHERE id = ?1",
            params![id],
            map_conversation,
        )
        .optional()
        .map_err(Into::into)
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
            system_prompt: Some(sys_prompt.clone()),
            thinking_level: default_thinking,
            web_search_enabled: false,
            pinned: false,
            created_at: now,
            updated_at: now,
        };
        let conn = self.conn.lock().expect("database mutex poisoned");
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
    }

    /// Persist whether web search is enabled for a conversation.
    pub fn set_conversation_web_search_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE conversations SET web_search_enabled = ?1 WHERE id = ?2",
            params![enabled as i64, id],
        )?;
        Ok(())
    }

    /// Persist the selected thinking level for a conversation.
    pub fn set_conversation_thinking_level(
        &self,
        id: &str,
        thinking_level: ThinkingLevel,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE conversations SET thinking_level = ?1 WHERE id = ?2",
            params![thinking_level.as_str(), id],
        )?;
        Ok(())
    }

    /// Update the custom system prompt for a conversation.
    pub fn set_conversation_system_prompt(
        &self,
        id: &str,
        system_prompt: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE conversations SET system_prompt = ?1 WHERE id = ?2",
            params![system_prompt, id],
        )?;
        Ok(())
    }

    /// Rename a conversation and bump its `updated_at` timestamp.
    pub fn rename_conversation(&self, id: &str, new_title: &str) -> Result<()> {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![trimmed, now, id],
        )?;
        Ok(())
    }

    /// Duplicate a conversation, its messages, and its granted folders.
    pub fn duplicate_conversation(&self, source_id: &str) -> Result<Option<Conversation>> {
        let mut conn = self.conn.lock().expect("database mutex poisoned");
        let Some(source) = conn
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

        let tx = conn.transaction()?;
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

        let mut stmt = tx.prepare(
            "SELECT role, content, reasoning_content, thinking_level, token_count, created_at
             FROM messages
             WHERE conversation_id = ?1
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows: Vec<(String, String, Option<String>, Option<String>, i64, i64)> = stmt
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

        for (role, content, reasoning, thinking_level, token_count, created_at) in rows {
            tx.execute(
                "INSERT INTO messages (id, conversation_id, role, content, reasoning_content, thinking_level, token_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    copy.id,
                    role,
                    content,
                    reasoning,
                    thinking_level,
                    token_count,
                    created_at,
                ],
            )?;
        }

        let mut fstmt = tx.prepare(
            "SELECT folder_path, permission_mode, created_at
             FROM conversation_folders
             WHERE conversation_id = ?1
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let folders: Vec<(String, String, i64)> = fstmt
            .query_map(params![source_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(fstmt);

        for (folder_path, permission_mode, created_at) in folders {
            tx.execute(
                "INSERT INTO conversation_folders (id, conversation_id, folder_path, permission_mode, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    Uuid::new_v4().to_string(),
                    copy.id,
                    folder_path,
                    permission_mode,
                    created_at,
                ],
            )?;
        }

        tx.commit()?;
        Ok(Some(copy))
    }

    /// Delete a conversation and cascade-delete its messages.
    pub fn delete_conversation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Grant folder access to a conversation (deduplicated by `folder_path`, defaults to `"read"`).
    pub fn add_conversation_folder(
        &self,
        conversation_id: &str,
        folder_path: &str,
    ) -> Result<super::entities::ConversationFolder> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        if let Some(existing) = conn
            .query_row(
                "SELECT id, conversation_id, folder_path, permission_mode, created_at
                 FROM conversation_folders
                 WHERE conversation_id = ?1 AND folder_path = ?2",
                params![conversation_id, folder_path],
                |row| {
                    let mode_raw: String = row.get(3)?;
                    Ok(super::entities::ConversationFolder {
                        id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        folder_path: row.get(2)?,
                        permission_mode: FolderPermission::from_str_lossy(&mode_raw),
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()?
        {
            return Ok(existing);
        }

        let entry = super::entities::ConversationFolder {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            folder_path: folder_path.to_string(),
            permission_mode: FolderPermission::Read,
            created_at: Utc::now().timestamp_millis(),
        };
        conn.execute(
            "INSERT INTO conversation_folders (id, conversation_id, folder_path, permission_mode, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.id,
                entry.conversation_id,
                entry.folder_path,
                entry.permission_mode.as_str(),
                entry.created_at,
            ],
        )?;
        Ok(entry)
    }

    /// List all granted folders for a conversation in insertion order.
    pub fn list_conversation_folders(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<super::entities::ConversationFolder>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, folder_path, permission_mode, created_at
             FROM conversation_folders
             WHERE conversation_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            let mode_raw: String = row.get(3)?;
            Ok(super::entities::ConversationFolder {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                folder_path: row.get(2)?,
                permission_mode: FolderPermission::from_str_lossy(&mode_raw),
                created_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Update a granted folder's permission mode (`FolderPermission::Read` or `ReadWrite`).
    pub fn set_conversation_folder_permission(
        &self,
        id: &str,
        permission_mode: FolderPermission,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "UPDATE conversation_folders SET permission_mode = ?1 WHERE id = ?2",
            params![permission_mode.as_str(), id],
        )?;
        Ok(())
    }

    /// Revoke a granted folder from a conversation.
    pub fn remove_conversation_folder(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute("DELETE FROM conversation_folders WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Search conversations by title or message content via FTS5.
    pub fn search_conversations(&self, query: &str, limit: usize) -> Result<Vec<Conversation>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let like_pattern = format!("%{}%", trimmed.replace('%', "\\%").replace('_', "\\_"));
        let fts_query = build_fts_query(trimmed);

        let conn = self.conn.lock().expect("database mutex poisoned");
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
            let rows = stmt.query_map(params![like_pattern, fts, limit as i64], map_conversation)?;
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
        Some(tokens.join(" AND "))
    }
}
