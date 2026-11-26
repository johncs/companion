//! Local SQLite persistence layer (WAL mode + FTS5 full-text search).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod agent;
pub mod attachments;
pub mod conversations;
pub mod downloads;
pub mod entities;
pub mod folders;
pub mod messages;
pub mod models;
pub mod schema;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

pub use entities::{
    AttachmentRecord, Conversation, ConversationFolder, ModelRecord, NewMessage, PendingDownload,
    ToolCallRecord,
};

/// Thread-safe handle to Companion's local SQLite database.
#[derive(Clone)]
pub struct Database {
    pub(crate) conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl Database {
    /// Open (or create) the SQLite database at `db_path`, configure WAL mode, and run migrations.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create database directory {:?}", parent))?;
        }

        let mut conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open SQLite database {:?}", db_path))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            ",
        )?;

        schema::run_migrations(&mut conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Open an isolated in-memory database (primarily for tests).
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        schema::run_migrations(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Return the filesystem path of the active database.
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    /// Execute a closure while holding the database mutex.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database mutex poisoned"))?;
        f(&guard)
    }

    /// Execute a closure inside an atomic SQLite transaction while holding the database mutex.
    pub fn with_tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut guard = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("Database mutex poisoned"))?;
        let tx = guard.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_messages_and_fts_search() {
        use companion_core::types::MessageRole;

        let db = Database::open_in_memory().expect("open in-memory db");

        let arch_chat = db
            .create_conversation("Architecture Discussion")
            .expect("create chat");
        let quick_chat = db
            .create_conversation("Quick Question")
            .expect("create chat");

        assert_eq!(db.list_conversations(50).unwrap().len(), 2);

        db.insert_message(NewMessage {
            conversation_id: &quick_chat.id,
            role: MessageRole::User,
            content: "How does virtual scrolling work in GTK4 ListView?",
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .expect("insert message");

        let page = db.list_messages_page(&quick_chat.id, None, 20).unwrap();
        assert_eq!(page.len(), 1);

        let hits = db.search_conversations("virtual", 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, quick_chat.id);

        let title_hits = db.search_conversations("Architecture", 20).unwrap();
        assert_eq!(title_hits.len(), 1);
        assert_eq!(title_hits[0].id, arch_chat.id);

        db.delete_conversation(&arch_chat.id).unwrap();
        assert_eq!(db.list_conversations(50).unwrap().len(), 1);
        assert!(db.get_conversation(&arch_chat.id).unwrap().is_none());
    }

    #[test]
    fn paginated_messages_handle_large_history() {
        use companion_core::types::MessageRole;

        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Long Chat").unwrap();

        for i in 0..120 {
            let content = format!("Message {i}");
            db.insert_message(NewMessage {
                conversation_id: &conv.id,
                role: MessageRole::User,
                content: &content,
                reasoning_content: None,
                thinking_level: None,
                model_name: None,
                execution_device: None,
                token_count: 0,
                generation_ms: None,
            })
            .unwrap();
        }

        let latest = db.list_messages_page(&conv.id, None, 50).unwrap();
        assert_eq!(latest.len(), 50);
        assert_eq!(latest.last().unwrap().content, "Message 119");

        let cursor = latest.first().unwrap().created_at;
        let older = db.list_messages_page(&conv.id, Some(cursor), 50).unwrap();
        assert!(!older.is_empty());
        assert!(older.last().unwrap().created_at < cursor);
    }

    #[test]
    fn duplicate_conversation_preserves_messages() {
        use companion_core::types::MessageRole;

        let db = Database::open_in_memory().expect("open in-memory db");
        let orig = db.create_conversation("Design").unwrap();
        db.insert_message(NewMessage {
            conversation_id: &orig.id,
            role: MessageRole::User,
            content: "Hello",
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .unwrap();
        db.insert_message(NewMessage {
            conversation_id: &orig.id,
            role: MessageRole::Assistant,
            content: "Hi there!",
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .unwrap();

        let dup = db
            .duplicate_conversation(&orig.id)
            .unwrap()
            .expect("duplicated conversation");
        assert_eq!(dup.title, "Design (Copy)");
        assert_eq!(db.list_messages_page(&dup.id, None, 10).unwrap().len(), 2);
    }

    #[test]
    fn edit_message_truncates_subsequent_messages() {
        use companion_core::types::MessageRole;

        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Truncate Test").unwrap();
        let simple = |role: MessageRole, content: &'static str| NewMessage {
            conversation_id: &conv.id,
            role,
            content,
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        };
        let m1 = db.insert_message(simple(MessageRole::User, "First")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let m2 = db
            .insert_message(simple(MessageRole::Assistant, "First reply"))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let m3 = db.insert_message(simple(MessageRole::User, "Second")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.insert_message(simple(MessageRole::Assistant, "Second reply"))
            .unwrap();

        db.edit_message_and_truncate_after(&conv.id, &m3.id, m3.created_at, "Second (edited)")
            .unwrap();

        let remaining = db.list_messages_page(&conv.id, None, 10).unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining[0].id, m1.id);
        assert_eq!(remaining[1].id, m2.id);
        assert_eq!(remaining[2].id, m3.id);
        assert_eq!(remaining[2].content, "Second (edited)");
    }

    #[test]
    fn persists_thinking_level_metrics_and_tool_calls() {
        use companion_core::ThinkingLevel;
        use companion_core::types::{
            ExecutionDevice, FolderPermission, MessageRole, PermissionDecision, ToolCallStatus,
        };

        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Metrics").unwrap();
        assert_eq!(conv.thinking_level, ThinkingLevel::Default);

        db.set_conversation_thinking_level(&conv.id, ThinkingLevel::High)
            .unwrap();
        let loaded = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.thinking_level, ThinkingLevel::High);

        let msg = db
            .insert_message(NewMessage {
                conversation_id: &conv.id,
                role: MessageRole::Assistant,
                content: "Done",
                reasoning_content: Some("reasoning"),
                thinking_level: Some(ThinkingLevel::High),
                model_name: Some("Gemma 4 12B"),
                execution_device: Some(ExecutionDevice::Cpu),
                token_count: 128,
                generation_ms: Some(1450),
            })
            .unwrap();
        assert_eq!(msg.model_name.as_deref(), Some("Gemma 4 12B"));
        assert_eq!(msg.execution_device, Some(ExecutionDevice::Cpu));
        assert_eq!(msg.token_count, 128);
        assert_eq!(msg.generation_ms, Some(1450));

        let tc = ToolCallRecord {
            id: "tc-1".to_string(),
            message_id: msg.id.clone(),
            tool_name: "read_file".to_string(),
            arguments_json: "{\"path\":\"README.md\"}".to_string(),
            status: ToolCallStatus::Ok,
            output: Some("hello".to_string()),
            permission_decision: PermissionDecision::AllowOnce,
            created_at: 1000,
        };
        db.insert_tool_call(&tc).unwrap();
        let mut map = db.list_tool_calls_for_messages(&[&msg.id]).unwrap();
        let calls = map.remove(&msg.id).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "read_file");

        let f1 = db.add_conversation_folder(&conv.id, "/tmp/docs").unwrap();
        let f2 = db.add_conversation_folder(&conv.id, "/tmp/docs").unwrap();
        assert_eq!(f1.id, f2.id);
        let folders = db.list_conversation_folders(&conv.id).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].folder_path, "/tmp/docs");
        assert_eq!(folders[0].permission_mode, FolderPermission::Read);
        db.set_conversation_folder_permission(&f1.id, FolderPermission::ReadWrite)
            .unwrap();
        assert_eq!(
            db.list_conversation_folders(&conv.id).unwrap()[0].permission_mode,
            FolderPermission::ReadWrite
        );
        db.remove_conversation_folder(&f1.id).unwrap();
        assert!(db.list_conversation_folders(&conv.id).unwrap().is_empty());

        let simple = |role: MessageRole, content: &'static str| NewMessage {
            conversation_id: &conv.id,
            role,
            content,
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        };
        let m1 = db.insert_message(simple(MessageRole::User, "first")).unwrap();
        let m2 = db.insert_message(simple(MessageRole::User, "second")).unwrap();
        let _m3 = db.insert_message(simple(MessageRole::Assistant, "third")).unwrap();
        db.delete_message_and_after(&conv.id, &m2.id, m2.created_at).unwrap();
        let remaining = db.list_messages_page(&conv.id, None, 20).unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[1].id, m1.id);

        db.set_conversation_system_prompt(&conv.id, Some("Custom notes"))
            .unwrap();
        db.set_conversation_thinking_level(&conv.id, ThinkingLevel::High)
            .unwrap();
        let f_ro = db.add_conversation_folder(&conv.id, "/tmp/ro_dir").unwrap();
        let f_rw = db.add_conversation_folder(&conv.id, "/tmp/rw_dir").unwrap();
        db.set_conversation_folder_permission(&f_rw.id, FolderPermission::ReadWrite)
            .unwrap();

        let att_msg = db
            .insert_message(NewMessage {
                conversation_id: &conv.id,
                role: companion_core::types::MessageRole::Assistant,
                content: "Checked file",
                reasoning_content: Some("Thinking..."),
                thinking_level: Some(ThinkingLevel::High),
                model_name: Some("Gemma 4"),
                execution_device: Some(companion_core::types::ExecutionDevice::Gpu),
                token_count: 42,
                generation_ms: Some(1250),
            })
            .unwrap();
        db.insert_attachments(
            &att_msg.id,
            &[AttachmentRecord {
                id: "att-dup-1".to_string(),
                message_id: att_msg.id.clone(),
                file_name: "notes.txt".to_string(),
                mime_type: "text/plain".to_string(),
                file_path: "/tmp/notes.txt".to_string(),
                extracted_text: Some("hello".to_string()),
                byte_size: 5,
            }],
        )
        .unwrap();
        db.insert_tool_call(&ToolCallRecord {
            id: "tc-dup-1".to_string(),
            message_id: att_msg.id.clone(),
            tool_name: "read_file".to_string(),
            arguments_json: r#"{"path":"notes.txt"}"#.to_string(),
            status: companion_core::types::ToolCallStatus::Ok,
            output: Some("hello".to_string()),
            permission_decision: companion_core::types::PermissionDecision::AllowOnce,
            created_at: 1000,
        })
        .unwrap();

        let dup = db.duplicate_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(dup.system_prompt.as_deref(), Some("Custom notes"));
        assert_eq!(dup.thinking_level, ThinkingLevel::High);

        let dup_msgs = db.list_messages_page(&dup.id, None, 20).unwrap();
        assert_eq!(dup_msgs.len(), 3);
        let dup_last = dup_msgs.last().unwrap();
        assert_ne!(dup_last.id, att_msg.id);
        assert_eq!(dup_last.model_name.as_deref(), Some("Gemma 4"));
        assert_eq!(
            dup_last.execution_device,
            Some(companion_core::types::ExecutionDevice::Gpu)
        );
        assert_eq!(dup_last.generation_ms, Some(1250));

        let dup_atts = db
            .list_attachments_for_messages(&[dup_last.id.as_str()])
            .unwrap();
        assert_eq!(dup_atts.get(&dup_last.id).map(Vec::len), Some(1));
        let dup_tcs = db
            .list_tool_calls_for_messages(&[dup_last.id.as_str()])
            .unwrap();
        assert_eq!(dup_tcs.get(&dup_last.id).map(Vec::len), Some(1));

        let dup_folders = db.list_conversation_folders(&dup.id).unwrap();
        assert_eq!(dup_folders.len(), 2);
        assert_eq!(dup_folders[0].folder_path, f_ro.folder_path);
        assert_eq!(dup_folders[0].permission_mode, FolderPermission::Read);
        assert_eq!(dup_folders[1].folder_path, f_rw.folder_path);
        assert_eq!(dup_folders[1].permission_mode, FolderPermission::ReadWrite);
    }

    #[test]
    fn active_model_setting_persists_and_clears() {
        let db = Database::open_in_memory().expect("open in-memory db");
        assert_eq!(db.get_active_model_id().unwrap(), None);

        db.set_active_model_id(Some("model-123")).unwrap();
        assert_eq!(db.get_active_model_id().unwrap().as_deref(), Some("model-123"));

        db.set_active_model_id(Some("model-456")).unwrap();
        assert_eq!(db.get_active_model_id().unwrap().as_deref(), Some("model-456"));

        db.set_active_model_id(None).unwrap();
        assert_eq!(db.get_active_model_id().unwrap(), None);
    }

    #[test]
    fn personalization_settings_persist_and_default() {
        let db = Database::open_in_memory().expect("open in-memory db");
        assert_eq!(db.get_assistant_name().unwrap(), "Companion");
        assert_eq!(db.get_user_name().unwrap(), None);

        db.set_assistant_name("Emma").unwrap();
        db.set_user_name("Joe").unwrap();
        assert_eq!(db.get_assistant_name().unwrap(), "Emma");
        assert_eq!(db.get_user_name().unwrap().as_deref(), Some("Joe"));

        db.set_assistant_name("").unwrap();
        db.set_user_name("   ").unwrap();
        assert_eq!(db.get_assistant_name().unwrap(), "Companion");
        assert_eq!(db.get_user_name().unwrap(), None);
    }

    #[test]
    fn render_system_prompt_substitutes_placeholders_and_legacy_prefix() {
        let rendered = companion_core::prompt::render_system_prompt(
            "You are {assistant_name}, helping {user_name}.",
            "Emma",
            Some("Joe"),
        );
        assert_eq!(rendered, "You are Emma, helping Joe.");

        let legacy = companion_core::prompt::render_system_prompt(
            "You are Companion, a helpful assistant.",
            "Emma",
            None,
        );
        assert_eq!(legacy, "You are Emma, a helpful assistant.");

        let composed = companion_core::prompt::compose_personalized_system_prompt(
            "Reply in Greek.",
            "Emma",
            Some("Joe"),
            companion_core::AssistantPersonality::Friendly,
            None,
            None,
        );
        assert!(composed.starts_with(companion_core::prompt::CORE_SYSTEM_RULES));
        assert!(composed.contains("Your name is Emma."));
        assert!(composed.contains("The user's name is Joe."));
        assert!(composed.ends_with("Reply in Greek."));

        let custom = companion_core::prompt::compose_personalized_system_prompt(
            "",
            "Emma",
            Some("Joe"),
            companion_core::AssistantPersonality::Custom,
            Some("You are {assistant_name}, a concise Rust mentor for {user_name}."),
            None,
        );
        assert!(custom.contains("You are Emma, a concise Rust mentor for Joe."));
    }

    #[test]
    fn with_tx_rolls_back_on_error() {
        let db = Database::open_in_memory().expect("open in-memory db");
        let err = db
            .with_tx(|tx| -> Result<()> {
                tx.execute(
                    "INSERT INTO app_settings (key, value) VALUES ('tx_test', 'val')",
                    [],
                )?;
                anyhow::bail!("abort tx")
            })
            .unwrap_err();
        assert!(err.to_string().contains("abort tx"));
        assert_eq!(db.get_setting("tx_test").unwrap(), None);
    }
}
