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
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title(
            "Web Search Privacy",
        ))
        .unwrap();

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
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title(
            "AllowChat Isolation",
        ))
        .unwrap();
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
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title("Esc"))
        .unwrap();
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

#[test]
fn destructive_allow_chat_does_not_escalate_read_only_folder() {
    let base = std::env::temp_dir().join(format!("companion_dest_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    let victim = base.join("victim.txt");
    std::fs::write(&victim, "bye").unwrap();
    let blocked_write = base.join("escalated.txt");

    let db = Database::open_in_memory().unwrap();
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title(
            "Destructive Isolation",
        ))
        .unwrap();
    db.add_conversation_folder(&conv.id, &base.to_string_lossy())
        .unwrap();

    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"delete_file\",\"arguments\":{{\"path\":\"{}\"}}}}</tool_call>",
                victim.display()
            )),
            ChatEvent::Done,
        ],
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"write_file\",\"arguments\":{{\"path\":\"{}\",\"content\":\"pwn\"}}}}</tool_call>",
                blocked_write.display()
            )),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Done".into()), ChatEvent::Done],
    ]);
    // Even if AllowChat is returned for delete_file, the folder must remain ReadOnly,
    // so the subsequent write_file MUST still prompt and be denied.
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
            vec![GrantedFolder {
                path: base.clone(),
                allow_write: false,
            }],
            false,
        ),
    );

    assert!(!victim.exists());
    assert!(!blocked_write.exists());
    let folders = db.list_conversation_folders(&conv.id).unwrap();
    assert_eq!(folders[0].permission_mode, FolderPermission::Read);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn mid_batch_tool_cancellation_stops_remaining_calls() {
    let db = Database::open_in_memory().unwrap();
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title(
            "Cancel Batch",
        ))
        .unwrap();
    let batch = "<tool_call>{\"name\":\"list_files\",\"arguments\":{\"subdir\":\".\"}}</tool_call>\n\
                 <tool_call>{\"name\":\"list_files\",\"arguments\":{\"subdir\":\".\"}}</tool_call>";
    let provider =
        MockModelProvider::new(vec![vec![ChatEvent::Token(batch.into()), ChatEvent::Done]]);
    let cfg = base_config(&conv.id, vec![], false);
    cfg.cancel_flag
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let events = run_agent(
        provider,
        Arc::new(MockApprovalHandler::new(vec![])),
        &db,
        cfg,
    );
    let tc_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallRecorded(_)))
        .count();
    assert_eq!(tc_count, 0);
}

#[tokio::test]
async fn approved_web_search_executes_mock_transport_and_feeds_next_turn() {
    struct MockSearchTransport;
    impl crate::web_search::HttpTransport for MockSearchTransport {
        fn get<'a>(
            &'a self,
            _url: &'a str,
            _query: &'a [(&'a str, &'a str)],
        ) -> crate::tool::BoxFuture<'a, anyhow::Result<crate::web_search::HttpResponse>> {
            let html = br#"<div class="result"><a class="result__a" href="https://example.com">Rust News</a><a class="result__snippet">Latest release</a></div>"#;
            Box::pin(async move {
                Ok(crate::web_search::HttpResponse {
                    status: 200,
                    location: None,
                    body: html.to_vec(),
                })
            })
        }

        fn fetch_pinned_hop<'a>(
            &'a self,
            _url: &'a reqwest::Url,
            _pinned_addr: std::net::SocketAddr,
            _max_bytes: usize,
        ) -> crate::tool::BoxFuture<'a, anyhow::Result<crate::web_search::HttpResponse>> {
            Box::pin(async { anyhow::bail!("unused") })
        }
    }

    let db = Database::open_in_memory().unwrap();
    db.set_web_search_global_enabled(true).unwrap();
    let conv = db
        .create_conversation(companion_storage::NewConversation::with_title("Mock Web"))
        .unwrap();

    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(
                r#"<tool_call>{"name":"web_search","arguments":{"query":"rust release"}}</tool_call>"#
                    .into(),
            ),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Rust released!".into()), ChatEvent::Done],
    ]);
    let web = Arc::new(crate::web_search::WebSearchService::with_transport(
        Arc::new(MockSearchTransport),
    ));
    let approver = Arc::new(MockApprovalHandler::new(vec![ApprovalResponse::WebSearch(
        Some("rust release".into()),
    )]));
    let runner = AgentRunner {
        provider: Arc::new(provider),
        registry: ToolRegistry::with_builtins(web),
        permissions: PermissionEngine::new(),
        approver,
        db: db.clone(),
    };
    let mut rx = runner.spawn_on(
        &tokio::runtime::Handle::current(),
        base_config(&conv.id, vec![], true),
    );
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let done = matches!(ev, AgentEvent::Finished { .. });
        events.push(ev);
        if done {
            break;
        }
    }

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallRecorded(tc)
            if tc.tool_name == "web_search"
                && tc.status == ToolCallStatus::Ok
                && tc.output.as_deref().is_some_and(|o| o.contains("Rust News"))
    )));
}
