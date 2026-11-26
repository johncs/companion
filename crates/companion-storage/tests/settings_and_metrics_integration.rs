#![allow(clippy::unwrap_used, clippy::expect_used)]

use companion_core::ThinkingLevel;
use companion_core::types::{
    ExecutionDevice, FolderPermission, MessageRole, PermissionDecision, ToolCallStatus,
};
use companion_storage::{Database, NewConversation, NewMessage, ToolCallRecord};

#[test]
fn persists_thinking_level_metrics_and_tool_calls() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = db
        .create_conversation(NewConversation::with_title("Metrics"))
        .unwrap();
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
}

#[test]
fn active_model_setting_persists_and_clears() {
    let db = Database::open_in_memory().expect("open in-memory db");
    assert_eq!(db.get_active_model_id().unwrap(), None);

    db.set_active_model_id(Some("model-123")).unwrap();
    assert_eq!(
        db.get_active_model_id().unwrap().as_deref(),
        Some("model-123")
    );

    db.set_active_model_id(Some("model-456")).unwrap();
    assert_eq!(
        db.get_active_model_id().unwrap().as_deref(),
        Some("model-456")
    );

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
        &companion_core::prompt::PersonalizationProfile {
            assistant_name: "Emma",
            user_name: Some("Joe"),
            personality: companion_core::AssistantPersonality::Friendly,
            custom_personality: None,
            user_about: None,
        },
    );
    assert!(composed.starts_with(companion_core::prompt::CORE_SYSTEM_RULES));
    assert!(composed.contains("Your name is Emma."));
    assert!(composed.contains("The user's name is Joe."));
    assert!(composed.ends_with("Reply in Greek."));

    let custom = companion_core::prompt::compose_personalized_system_prompt(
        "",
        &companion_core::prompt::PersonalizationProfile {
            assistant_name: "Emma",
            user_name: Some("Joe"),
            personality: companion_core::AssistantPersonality::Custom,
            custom_personality: Some(
                "You are {assistant_name}, a concise Rust mentor for {user_name}.",
            ),
            user_about: None,
        },
    );
    assert!(custom.contains("You are Emma, a concise Rust mentor for Joe."));
}
