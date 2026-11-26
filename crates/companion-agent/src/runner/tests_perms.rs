use std::sync::Arc;

use crate::approval::ApprovalResponse;
use crate::tool::ApprovalRequest;
use companion_core::provider::ChatEvent;
use companion_core::types::{FolderPermission, PermissionDecision, ToolCallStatus};
use companion_storage::Database;

use super::tests_loop::{MockApprovalHandler, MockModelProvider, base_config, run_agent};
use super::*;

#[test]
fn web_search_forwards_privacy_warnings_to_approval_handler() {
    let db = Database::open_in_memory().unwrap();
    db.set_web_search_global_enabled(true).unwrap();
    let conv = db.create_conversation("Web Search Privacy").unwrap();

    let mut cfg = base_config(&conv.id, vec![], true);
    cfg.turns[0].content = "<attachment name=\"secret.txt\">API_TOKEN_12345</attachment>".into();

    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(
                "<tool_call>{\"name\":\"web_search\",\"arguments\":{\"query\":\"/home/alice/secret.txt\"}}</tool_call>".into(),
            ),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Done".into()), ChatEvent::Done],
    ]);
    let approver = Arc::new(MockApprovalHandler::new(vec![ApprovalResponse::WebSearch(
        None,
    )]));
    let events = run_agent(provider, approver.clone(), &db, cfg);

    let seen = approver.seen();
    assert_eq!(seen.len(), 1);
    match &seen[0] {
        ApprovalRequest::WebSearch {
            proposed_query,
            warnings,
            ..
        } => {
            assert_eq!(proposed_query, "/home/alice/secret.txt");
            assert!(!warnings.is_empty(), "expected privacy leak warning");
        }
        other => panic!("expected WebSearch request, got {other:?}"),
    }
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallRecorded(tc) if tc.status == ToolCallStatus::Denied
    )));
}

#[test]
fn allow_chat_persists_write_only_for_targeted_folder() {
    let base = std::env::temp_dir().join(format!("companion_runner_ac_{}", uuid::Uuid::new_v4()));
    let folder_a = base.join("folder_a");
    let folder_b = base.join("folder_b");
    std::fs::create_dir_all(&folder_a).unwrap();
    std::fs::create_dir_all(&folder_b).unwrap();

    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("AllowChat Isolation").unwrap();
    db.add_conversation_folder(&conv.id, &folder_a.to_string_lossy())
        .unwrap();
    db.add_conversation_folder(&conv.id, &folder_b.to_string_lossy())
        .unwrap();

    let target = folder_a.join("note.txt");
    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"write_file\",\"arguments\":{{\"path\":\"{}\",\"content\":\"ok\"}}}}</tool_call>",
                target.display()
            )),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Written".into()), ChatEvent::Done],
    ]);
    let approver = Arc::new(MockApprovalHandler::new(vec![ApprovalResponse::Tool(
        PermissionDecision::AllowChat,
    )]));
    let events = run_agent(
        provider,
        approver,
        &db,
        base_config(
            &conv.id,
            vec![
                GrantedFolder {
                    path: folder_a.clone(),
                    allow_write: false,
                },
                GrantedFolder {
                    path: folder_b.clone(),
                    allow_write: false,
                },
            ],
            false,
        ),
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::FolderGrantsUpdated))
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "ok");

    let folders = db.list_conversation_folders(&conv.id).unwrap();
    let perm_a = folders
        .iter()
        .find(|f| f.folder_path == folder_a.to_string_lossy())
        .unwrap()
        .permission_mode;
    let perm_b = folders
        .iter()
        .find(|f| f.folder_path == folder_b.to_string_lossy())
        .unwrap()
        .permission_mode;
    assert_eq!(perm_a, FolderPermission::ReadWrite);
    assert_eq!(perm_b, FolderPermission::Read);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn allow_chat_write_on_one_folder_does_not_escalate_read_only_folder() {
    let base = std::env::temp_dir().join(format!("companion_esc_{}", uuid::Uuid::new_v4()));
    let folder_rw = base.join("rw");
    let folder_ro = base.join("ro");
    std::fs::create_dir_all(&folder_rw).unwrap();
    std::fs::create_dir_all(&folder_ro).unwrap();
    let target_rw = folder_rw.join("ok.txt");
    let target_ro = folder_ro.join("blocked.txt");

    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Esc").unwrap();
    db.add_conversation_folder(&conv.id, &folder_rw.to_string_lossy())
        .unwrap();
    db.add_conversation_folder(&conv.id, &folder_ro.to_string_lossy())
        .unwrap();

    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"write_file\",\"arguments\":{{\"root\":\"{}\",\"path\":\"ok.txt\",\"content\":\"ok\"}}}}</tool_call>",
                folder_rw.display()
            )),
            ChatEvent::Done,
        ],
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"write_file\",\"arguments\":{{\"root\":\"{}\",\"path\":\"blocked.txt\",\"content\":\"pwn\"}}}}</tool_call>",
                folder_ro.display()
            )),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Done".into()), ChatEvent::Done],
    ]);
    // First approval grants AllowChat on folder_rw; second MUST still prompt and we Deny it.
    let approver = Arc::new(MockApprovalHandler::new(vec![
        ApprovalResponse::Tool(PermissionDecision::AllowChat),
        ApprovalResponse::Tool(PermissionDecision::Deny),
    ]));
    let _events = run_agent(
        provider,
        approver,
        &db,
        base_config(
            &conv.id,
            vec![
                GrantedFolder {
                    path: folder_rw.clone(),
                    allow_write: false,
                },
                GrantedFolder {
                    path: folder_ro.clone(),
                    allow_write: false,
                },
            ],
            false,
        ),
    );

    assert_eq!(std::fs::read_to_string(&target_rw).unwrap(), "ok");
    assert!(!target_ro.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn db_save_failure_emits_agent_error() {
    let db = Database::open_in_memory().unwrap();
    let provider = MockModelProvider::new(vec![vec![
        ChatEvent::Token("Hello".into()),
        ChatEvent::Done,
    ]]);
    let approver = Arc::new(MockApprovalHandler::new(vec![]));
    let events = run_agent(
        provider,
        approver,
        &db,
        base_config("missing-conv-id", vec![], false),
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("Failed to save reply")))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Finished {
            saved_message_id: None,
            created_at: None,
            ..
        }
    )));
}
