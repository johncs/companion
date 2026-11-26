//! Conversation session orchestration: prompt preparation and paginated message loading.

use std::path::{Path, PathBuf};

use companion_core::i18n::{gettext, tr_f};
use companion_core::provider::ChatTurn;
use companion_storage::settings::PersonalizationSettings;
use companion_storage::{Database, ModelRecord};

use crate::prompt::{MessageWithAttachments, PromptContext, build_prompt_turns};
use crate::registry::ToolRegistry;
use crate::tools::GrantedFolder;

pub use crate::session_page::{LoadedConversationPage, LoadedMessageItem, load_conversation_page};

/// Input required to build a prompt and validate active model/folder state off the UI thread.
pub struct PromptPrepInput {
    pub db: Database,
    pub conv_id: String,
    pub active_model_id: Option<String>,
    pub chat_system_prompt: String,
    pub web_search_enabled: bool,
    pub fallback_assistant_name: String,
    pub fallback_user_name: Option<String>,
    pub history: Vec<MessageWithAttachments>,
    pub load_history_from_db: bool,
    pub pending_user_turn: Option<MessageWithAttachments>,
    pub registry: ToolRegistry,
}

/// Validated model, folder grants, prompt turns, and non-fatal notices ready for [`AgentRunner`].
#[derive(Debug)]
pub struct PreparedPrompt {
    pub model_record: ModelRecord,
    pub granted_folders: Vec<GrantedFolder>,
    pub turns: Vec<ChatTurn>,
    pub notices: Vec<String>,
}

/// Validate the active model, resolve folder grants, load DB history if needed, and build prompt turns.
pub fn prepare_prompt(
    input: &PromptPrepInput,
    detect_thinking: impl Fn(&Path) -> bool,
) -> Result<PreparedPrompt, String> {
    let model_record =
        resolve_active_model(&input.db, input.active_model_id.as_deref(), detect_thinking)?;
    let (granted_folders, notices) = resolve_folder_grants(&input.db, &input.conv_id);
    let personalization = load_personalization(
        &input.db,
        &input.fallback_assistant_name,
        input.fallback_user_name.as_deref(),
    );
    let web_search_active =
        input.web_search_enabled && input.db.get_web_search_global_enabled().unwrap_or(false);
    let db_history = load_db_history_if_requested(input);
    let history = db_history.as_deref().unwrap_or(&input.history);
    let turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: &input.chat_system_prompt,
        personalization: &personalization,
        granted_folders: &granted_folders,
        web_search_active,
        history,
        registry: &input.registry,
    });
    Ok(PreparedPrompt {
        model_record,
        granted_folders,
        turns,
        notices,
    })
}

fn resolve_folder_grants(db: &Database, conv_id: &str) -> (Vec<GrantedFolder>, Vec<String>) {
    let mut notices = Vec::new();
    let mut granted_folders = Vec::new();
    for f in db.list_conversation_folders(conv_id).unwrap_or_default() {
        let p = PathBuf::from(&f.folder_path);
        if p.exists() {
            granted_folders.push(GrantedFolder {
                path: p,
                allow_write: f.permission_mode.allows_write(),
            });
        } else {
            notices.push(tr_f(
                "Granted folder unavailable: {path}",
                &[("path", &f.folder_path)],
            ));
        }
    }
    (granted_folders, notices)
}

fn load_personalization(
    db: &Database,
    fallback_assistant: &str,
    fallback_user: Option<&str>,
) -> PersonalizationSettings {
    db.get_personalization_settings()
        .unwrap_or_else(|_| PersonalizationSettings {
            assistant_name: fallback_assistant.to_string(),
            user_name: fallback_user.map(str::to_string),
            personality: Default::default(),
            custom_personality: None,
            user_about: None,
        })
}

fn load_db_history_if_requested(input: &PromptPrepInput) -> Option<Vec<MessageWithAttachments>> {
    if !input.load_history_from_db {
        return None;
    }
    let msgs = input.db.list_messages_page(&input.conv_id, None, 48).ok()?;
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    let mut att_map = input
        .db
        .list_attachments_for_messages(&ids)
        .unwrap_or_default();
    let mut loaded: Vec<MessageWithAttachments> = msgs
        .into_iter()
        .map(|m| MessageWithAttachments {
            role: m.role,
            content: m.content,
            attachments: att_map
                .remove(&m.id)
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.to_staged())
                .collect(),
        })
        .collect();
    if let Some(pending) = &input.pending_user_turn {
        loaded.push(pending.clone());
    }
    Some(loaded)
}

fn resolve_active_model(
    db: &Database,
    active_model_id: Option<&str>,
    detect_thinking: impl Fn(&Path) -> bool,
) -> Result<ModelRecord, String> {
    let mut model_record = active_model_id
        .and_then(|id| db.get_model(id).ok().flatten())
        .filter(|m| {
            m.file_path
                .as_deref()
                .is_some_and(|p| Path::new(p).exists())
        });
    if let Some(ref mut rec) = model_record
        && !rec.supports_thinking
        && let Some(path) = rec.file_path.as_deref()
        && detect_thinking(Path::new(path))
    {
        rec.supports_thinking = true;
        let _ = db.upsert_model(rec);
    }
    model_record.ok_or_else(|| {
        gettext("No model selected or model file is missing. Choose a model in Settings.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_core::types::{DEFAULT_ASSISTANT_NAME, MessageRole, ModelProviderType};
    use companion_storage::{NewConversation, NewMessage};

    fn insert_msg(db: &Database, conv_id: &str, role: MessageRole, content: &str) {
        db.insert_message(NewMessage {
            conversation_id: conv_id,
            role,
            content,
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .expect("insert db msg");
    }

    #[test]
    fn prepare_prompt_validates_model_and_missing_folders() {
        let db = Database::open_in_memory().expect("in-memory db");
        let conv = db
            .create_conversation(NewConversation::with_title("Test"))
            .expect("conv");
        let missing_dir = std::env::temp_dir().join("companion_missing_folder_test_xyz");
        let _ = std::fs::remove_dir_all(&missing_dir);
        db.add_conversation_folder(&conv.id, &missing_dir.to_string_lossy())
            .expect("grant folder");

        let err = prepare_prompt(
            &PromptPrepInput {
                db: db.clone(),
                conv_id: conv.id.clone(),
                active_model_id: Some("missing-id".to_string()),
                chat_system_prompt: "Be helpful.".to_string(),
                web_search_enabled: false,
                fallback_assistant_name: DEFAULT_ASSISTANT_NAME.to_string(),
                fallback_user_name: None,
                history: vec![MessageWithAttachments {
                    role: MessageRole::User,
                    content: "hi".to_string(),
                    attachments: Vec::new(),
                }],
                load_history_from_db: false,
                pending_user_turn: None,
                registry: ToolRegistry::with_builtins(std::sync::Arc::new(
                    crate::web_search::WebSearchService::new().expect("web search"),
                )),
            },
            |_| false,
        )
        .unwrap_err();
        assert!(err.contains("No model selected"));
    }

    #[test]
    fn prepare_prompt_loads_db_history_and_appends_pending_turn() {
        let db = Database::open_in_memory().expect("in-memory db");
        let conv = db
            .create_conversation(NewConversation::with_title("Test"))
            .expect("conv");
        let dummy_model_path = std::env::temp_dir().join(format!(
            "companion_dummy_model_{}.gguf",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&dummy_model_path, b"not-a-gguf").unwrap();

        let rec = ModelRecord {
            id: "m-test".into(),
            display_name: "Test Model".into(),
            provider_type: ModelProviderType::LlamaCpp,
            file_path: Some(dummy_model_path.to_string_lossy().to_string()),
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params: Default::default(),
            avg_tokens_per_sec: None,
        };
        db.upsert_model(&rec).expect("upsert model");
        insert_msg(&db, &conv.id, MessageRole::User, "first from db");

        let prepared = prepare_prompt(
            &PromptPrepInput {
                db: db.clone(),
                conv_id: conv.id.clone(),
                active_model_id: Some("m-test".to_string()),
                chat_system_prompt: "Be helpful.".to_string(),
                web_search_enabled: false,
                fallback_assistant_name: DEFAULT_ASSISTANT_NAME.to_string(),
                fallback_user_name: None,
                history: Vec::new(),
                load_history_from_db: true,
                pending_user_turn: Some(MessageWithAttachments {
                    role: MessageRole::User,
                    content: "second pending".to_string(),
                    attachments: Vec::new(),
                }),
                registry: ToolRegistry::with_builtins(std::sync::Arc::new(
                    crate::web_search::WebSearchService::new().expect("web search"),
                )),
            },
            |_| false,
        )
        .expect("prepared prompt");

        assert_eq!(prepared.model_record.id, "m-test");
        assert_eq!(prepared.turns.len(), 3);
        assert_eq!(prepared.turns[1].content, "first from db");
        assert_eq!(prepared.turns[2].content, "second pending");
        let _ = std::fs::remove_file(&dummy_model_path);
    }
}
