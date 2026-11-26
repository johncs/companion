#![allow(clippy::unwrap_used, clippy::expect_used)]

use companion_core::ThinkingLevel;
use companion_core::types::{FolderPermission, MessageRole, StagedAttachment};
use companion_storage::{Conversation, Database, NewConversation, NewMessage, ToolCallRecord};

fn new_conv(db: &Database, title: &str) -> Conversation {
    db.create_conversation(NewConversation::with_title(title))
        .unwrap()
}

#[test]
fn conversations_messages_and_fts_search() {
    let db = Database::open_in_memory().expect("open in-memory db");

    let arch_chat = new_conv(&db, "Architecture Discussion");
    let quick_chat = new_conv(&db, "Quick Question");

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
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "Long Chat");

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

    let cursor = latest.first().unwrap();
    let older = db
        .list_messages_page(&conv.id, Some(&cursor.id), 50)
        .unwrap();
    assert!(!older.is_empty());
    assert!(older.last().unwrap().created_at <= cursor.created_at);
}

#[test]
fn duplicate_conversation_preserves_messages() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let orig = new_conv(&db, "Design");
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
        .duplicate_conversation(&orig.id, "Design (Copy)")
        .unwrap()
        .expect("duplicated conversation");
    assert_eq!(dup.title, "Design (Copy)");
    assert_eq!(db.list_messages_page(&dup.id, None, 10).unwrap().len(), 2);
}

#[test]
fn edit_message_truncates_subsequent_messages() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "Truncate Test");
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
    let m1 = db
        .insert_message(simple(MessageRole::User, "First"))
        .unwrap();
    let m2 = db
        .insert_message(simple(MessageRole::Assistant, "First reply"))
        .unwrap();
    let m3 = db
        .insert_message(simple(MessageRole::User, "Second"))
        .unwrap();
    db.insert_message(simple(MessageRole::Assistant, "Second reply"))
        .unwrap();

    db.edit_message_and_truncate_after(&conv.id, &m3.id, "Second (edited)")
        .unwrap();

    let remaining = db.list_messages_page(&conv.id, None, 10).unwrap();
    assert_eq!(remaining.len(), 3);
    assert_eq!(remaining[0].id, m1.id);
    assert_eq!(remaining[1].id, m2.id);
    assert_eq!(remaining[2].id, m3.id);
    assert_eq!(remaining[2].content, "Second (edited)");
}

#[test]
fn deletes_message_and_subsequent_messages() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "Delete From");
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
    let m1 = db
        .insert_message(simple(MessageRole::User, "first"))
        .unwrap();
    let m2 = db
        .insert_message(simple(MessageRole::User, "second"))
        .unwrap();
    let _m3 = db
        .insert_message(simple(MessageRole::Assistant, "third"))
        .unwrap();
    db.delete_message_and_after(&conv.id, &m2.id).unwrap();
    let remaining = db.list_messages_page(&conv.id, None, 20).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, m1.id);

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
            role: MessageRole::Assistant,
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
        &[StagedAttachment {
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

    let dup = db
        .duplicate_conversation(&conv.id, "Copy")
        .unwrap()
        .unwrap();
    assert_eq!(dup.system_prompt.as_deref(), Some("Custom notes"));
    assert_eq!(dup.thinking_level, ThinkingLevel::High);

    let dup_msgs = db.list_messages_page(&dup.id, None, 20).unwrap();
    assert_eq!(dup_msgs.len(), 2);
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
fn pagination_keeps_all_same_millisecond_messages_across_pages() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "Same-ms Pagination");
    for i in 0..7 {
        db.insert_message(NewMessage {
            conversation_id: &conv.id,
            role: MessageRole::User,
            content: &format!("msg-{i}"),
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .unwrap();
    }

    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = db
            .list_messages_page(&conv.id, cursor.as_deref(), 2)
            .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = Some(page[0].id.clone());
        all.splice(0..0, page);
    }
    let contents: Vec<_> = all.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec![
            "msg-0", "msg-1", "msg-2", "msg-3", "msg-4", "msg-5", "msg-6"
        ]
    );
}

#[test]
fn missing_message_id_in_edit_or_delete_does_not_delete_other_messages() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "No-op Guard");
    db.insert_message(NewMessage {
        conversation_id: &conv.id,
        role: MessageRole::User,
        content: "keep me",
        reasoning_content: None,
        thinking_level: None,
        model_name: None,
        execution_device: None,
        token_count: 0,
        generation_ms: None,
    })
    .unwrap();

    db.edit_message_and_truncate_after(&conv.id, "nonexistent-id", "hack")
        .unwrap();
    db.delete_message_and_after(&conv.id, "nonexistent-id")
        .unwrap();

    let remaining = db.list_messages_page(&conv.id, None, 10).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].content, "keep me");
}

#[test]
fn fts_search_handles_hyphens_paths_like_escapes_and_atomic_tool_calls() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = new_conv(&db, "100% real_path\\test");
    db.insert_message_with_tool_calls(
        NewMessage {
            conversation_id: &conv.id,
            role: MessageRole::Assistant,
            content: "Check foo-bar in src/main.rs",
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 10,
            generation_ms: None,
        },
        &[ToolCallRecord {
            id: "tc-atomic-1".to_string(),
            message_id: String::new(),
            tool_name: "read_file".to_string(),
            arguments_json: "{}".to_string(),
            status: companion_core::types::ToolCallStatus::Ok,
            output: Some("ok".to_string()),
            permission_decision: companion_core::types::PermissionDecision::AllowOnce,
            created_at: 1,
        }],
    )
    .unwrap();

    assert_eq!(db.search_conversations("foo-bar", 10).unwrap().len(), 1);
    assert_eq!(db.search_conversations("src/main.rs", 10).unwrap().len(), 1);
    assert_eq!(
        db.search_conversations("100% real_path\\test", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(db.search_conversations("100_zzz", 10).unwrap().is_empty());
}

#[test]
fn create_conversation_persists_explicit_settings_and_duplicate_uses_title() {
    let db = Database::open_in_memory().expect("open in-memory db");
    let conv = db
        .create_conversation(NewConversation {
            title: "  Custom  ",
            model_id: None,
            persona_id: None,
            system_prompt: Some("Be brief.".into()),
            thinking_level: ThinkingLevel::High,
            web_search_enabled: true,
        })
        .unwrap();
    assert_eq!(conv.title, "Custom");
    assert_eq!(conv.system_prompt.as_deref(), Some("Be brief."));
    assert_eq!(conv.thinking_level, ThinkingLevel::High);
    assert!(conv.web_search_enabled);

    let dup = db
        .duplicate_conversation(&conv.id, "Custom (Clone)")
        .unwrap()
        .unwrap();
    assert_eq!(dup.title, "Custom (Clone)");
    assert_eq!(dup.thinking_level, ThinkingLevel::High);
    assert!(dup.web_search_enabled);
}
