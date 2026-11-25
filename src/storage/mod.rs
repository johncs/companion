//! Local SQLite persistence layer (WAL mode + FTS5 full-text search).

pub mod agent;
pub mod attachments;
pub mod conversations;
pub mod entities;
pub mod messages;
pub mod models;
pub mod schema;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

pub use entities::{
    AttachmentRecord, Conversation, ConversationFolder, ModelRecord, PendingDownload,
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

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open SQLite database {:?}", db_path))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            ",
        )?;

        schema::run_migrations(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Open an isolated in-memory database for unit tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        schema::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Return the filesystem path of the active database.
    pub fn path(&self) -> &Path {
        &self.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_messages_and_fts_search() {
        let db = Database::open_in_memory().expect("open in-memory db");

        let arch_chat = db
            .create_conversation("Architecture Discussion")
            .expect("create chat");
        let quick_chat = db
            .create_conversation("Quick Question")
            .expect("create chat");

        assert_eq!(db.list_conversations(50).unwrap().len(), 2);

        db.insert_message(
            &quick_chat.id,
            "user",
            "How does virtual scrolling work in GTK4 ListView?",
            None,
            None,
        )
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
        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Long Chat").unwrap();

        for i in 0..120 {
            db.insert_message(&conv.id, "user", &format!("Message {i}"), None, None)
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
        let db = Database::open_in_memory().expect("open in-memory db");
        let orig = db.create_conversation("Design").unwrap();
        db.insert_message(&orig.id, "user", "Hello", None, None)
            .unwrap();
        db.insert_message(&orig.id, "assistant", "Hi there!", None, None)
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
        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Truncate Test").unwrap();
        let m1 = db.insert_message(&conv.id, "user", "First", None, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let m2 = db
            .insert_message(&conv.id, "assistant", "First reply", None, None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let m3 = db.insert_message(&conv.id, "user", "Second", None, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.insert_message(&conv.id, "assistant", "Second reply", None, None)
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
        let db = Database::open_in_memory().expect("open in-memory db");
        let conv = db.create_conversation("Metrics").unwrap();
        assert_eq!(conv.thinking_level, "default");

        db.set_conversation_thinking_level(&conv.id, "high").unwrap();
        let loaded = db.get_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.thinking_level, "high");

        let msg = db
            .insert_message_with_metrics(
                &conv.id,
                "assistant",
                "Done",
                Some("reasoning"),
                Some("high"),
                Some("Gemma 4 12B"),
                Some("cpu"),
                128,
                Some(1450),
            )
            .unwrap();
        assert_eq!(msg.model_name.as_deref(), Some("Gemma 4 12B"));
        assert_eq!(msg.execution_device.as_deref(), Some("cpu"));
        assert_eq!(msg.token_count, 128);
        assert_eq!(msg.generation_ms, Some(1450));

        let tc = ToolCallRecord {
            id: "tc-1".to_string(),
            message_id: msg.id.clone(),
            tool_name: "read_file".to_string(),
            arguments_json: "{\"path\":\"README.md\"}".to_string(),
            status: "ok".to_string(),
            output: Some("hello".to_string()),
            permission_decision: "allow_once".to_string(),
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
        assert_eq!(folders[0].permission_mode, "read");
        db.set_conversation_folder_permission(&f1.id, "read_write").unwrap();
        assert_eq!(
            db.list_conversation_folders(&conv.id).unwrap()[0].permission_mode,
            "read_write"
        );
        db.remove_conversation_folder(&f1.id).unwrap();
        assert!(db.list_conversation_folders(&conv.id).unwrap().is_empty());

        let m1 = db.insert_message(&conv.id, "user", "first", None, None).unwrap();
        let m2 = db.insert_message(&conv.id, "user", "second", None, None).unwrap();
        let _m3 = db.insert_message(&conv.id, "assistant", "third", None, None).unwrap();
        db.delete_message_and_after(&conv.id, &m2.id, m2.created_at).unwrap();
        let remaining = db.list_messages_page(&conv.id, None, 20).unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[1].id, m1.id);

        db.set_conversation_system_prompt(&conv.id, Some("Custom notes"))
            .unwrap();
        db.set_conversation_thinking_level(&conv.id, "high")
            .unwrap();
        let f_ro = db.add_conversation_folder(&conv.id, "/tmp/ro_dir").unwrap();
        let f_rw = db.add_conversation_folder(&conv.id, "/tmp/rw_dir").unwrap();
        db.set_conversation_folder_permission(&f_rw.id, "read_write").unwrap();
        let dup = db.duplicate_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(dup.system_prompt.as_deref(), Some("Custom notes"));
        assert_eq!(dup.thinking_level, "high");
        let dup_folders = db.list_conversation_folders(&dup.id).unwrap();
        assert_eq!(dup_folders.len(), 2);
        assert_eq!(dup_folders[0].folder_path, f_ro.folder_path);
        assert_eq!(dup_folders[0].permission_mode, "read");
        assert_eq!(dup_folders[1].folder_path, f_rw.folder_path);
        assert_eq!(dup_folders[1].permission_mode, "read_write");
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
        let rendered = crate::storage::conversations::render_system_prompt(
            "You are {assistant_name}, helping {user_name}.",
            "Emma",
            Some("Joe"),
        );
        assert_eq!(rendered, "You are Emma, helping Joe.");

        let legacy = crate::storage::conversations::render_system_prompt(
            "You are Companion, a helpful assistant.",
            "Emma",
            None,
        );
        assert_eq!(legacy, "You are Emma, a helpful assistant.");

        let composed = crate::storage::conversations::compose_personalized_system_prompt(
            "Reply in Greek.",
            "Emma",
            Some("Joe"),
            crate::core::AssistantPersonality::Friendly,
            None,
            None,
        );
        assert!(composed.starts_with(crate::storage::conversations::CORE_SYSTEM_RULES));
        assert!(composed.contains("Your name is Emma."));
        assert!(composed.contains("The user's name is Joe."));
        assert!(composed.ends_with("Reply in Greek."));

        let custom = crate::storage::conversations::compose_personalized_system_prompt(
            "",
            "Emma",
            Some("Joe"),
            crate::core::AssistantPersonality::Custom,
            Some("You are {assistant_name}, a concise Rust mentor for {user_name}."),
            None,
        );
        assert!(custom.contains("You are Emma, a concise Rust mentor for Joe."));
    }
}
