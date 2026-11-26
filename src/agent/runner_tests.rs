use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;
use crate::agent::tool::BoxFuture;
use crate::inference::provider::ChatEvent;

struct MockModelProvider {
    steps: Mutex<VecDeque<Vec<ChatEvent>>>,
}

impl MockModelProvider {
    fn new(steps: Vec<Vec<ChatEvent>>) -> Self {
        Self {
            steps: Mutex::new(VecDeque::from(steps)),
        }
    }
}

impl ModelProvider for MockModelProvider {
    fn stream_chat(
        &self,
        _model_path: String,
        _messages: Vec<ChatTurn>,
        _params: InferenceParams,
        cancel: Arc<AtomicBool>,
        tx: mpsc::UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<()> {
        let events = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![ChatEvent::Done]);
        for ev in events {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let _ = tx.send(ev);
        }
        Ok(())
    }
}

struct MockApprovalHandler {
    responses: Mutex<VecDeque<ApprovalResponse>>,
    seen_requests: Mutex<Vec<ApprovalRequest>>,
}

impl MockApprovalHandler {
    fn new(responses: Vec<ApprovalResponse>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            seen_requests: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<ApprovalRequest> {
        self.seen_requests.lock().unwrap().clone()
    }
}

impl ApprovalHandler for MockApprovalHandler {
    fn request_approval<'a>(&'a self, req: ApprovalRequest) -> BoxFuture<'a, ApprovalResponse> {
        self.seen_requests.lock().unwrap().push(req);
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ApprovalResponse::Tool(PermissionDecision::Deny));
        Box::pin(async move { resp })
    }
}

fn base_config(conv_id: &str, folders: Vec<GrantedFolder>, web: bool) -> AgentRunConfig {
    AgentRunConfig {
        conv_id: conv_id.to_string(),
        model_id: "m1".to_string(),
        model_path: String::new(),
        model_display_name: "MockModel".to_string(),
        base_params: InferenceParams::default(),
        thinking_level: ThinkingLevel::Default,
        execution_device: ExecutionDevice::Cpu,
        web_search_enabled: web,
        granted_folders: folders,
        turns: vec![ChatTurn {
            role: ChatRole::User,
            content: "Hello".to_string(),
            images: Vec::new(),
        }],
        cancel_flag: Arc::new(AtomicBool::new(false)),
    }
}

fn run_agent(
    provider: MockModelProvider,
    approver: Arc<MockApprovalHandler>,
    db: &Database,
    cfg: AgentRunConfig,
) -> Vec<AgentEvent> {
    let runner = AgentRunner {
        provider: Arc::new(provider),
        registry: ToolRegistry::with_builtins(),
        permissions: PermissionEngine::new(),
        approver,
        db: db.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    runtime().block_on(async {
        runner.run(cfg, tx).await;
    });
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

#[test]
fn single_turn_streaming_persists_message_and_usage() {
    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Single Turn").unwrap();
    let provider = MockModelProvider::new(vec![vec![
        ChatEvent::ReasoningToken("pondering".into()),
        ChatEvent::Token("Final reply".into()),
        ChatEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            decode_ms: Some(100),
        },
        ChatEvent::Done,
    ]]);
    let approver = Arc::new(MockApprovalHandler::new(vec![]));
    let events = run_agent(provider, approver, &db, base_config(&conv.id, vec![], false));

    let finished = events.iter().find_map(|e| match e {
        AgentEvent::Finished {
            saved_message_id,
            total_tokens,
            empty,
            ..
        } => Some((saved_message_id.clone(), *total_tokens, *empty)),
        _ => None,
    });
    assert!(matches!(finished, Some((Some(_), 5, false))));

    let msgs = db.list_messages_page(&conv.id, None, 10).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "Final reply");
    assert_eq!(msgs[0].reasoning_content.as_deref(), Some("pondering"));
}

#[test]
fn multi_step_tool_call_executes_and_feeds_result_back() {
    let dir = std::env::temp_dir().join(format!("companion_runner_tc_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("hello.txt");
    std::fs::write(&f, "secret-value").unwrap();

    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Tool Loop").unwrap();
    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(format!(
                "<tool_call>{{\"name\":\"read_file\",\"arguments\":{{\"path\":\"{}\"}}}}</tool_call>",
                f.display()
            )),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("File says secret-value".into()), ChatEvent::Done],
    ]);
    let approver = Arc::new(MockApprovalHandler::new(vec![]));
    let events = run_agent(
        provider,
        approver,
        &db,
        base_config(
            &conv.id,
            vec![GrantedFolder {
                path: dir.clone(),
                allow_write: false,
            }],
            false,
        ),
    );

    let recorded = events.iter().find_map(|e| match e {
        AgentEvent::ToolCallRecorded(tc) => Some(tc.clone()),
        _ => None,
    });
    let tc = recorded.expect("tool call recorded");
    assert_eq!(tc.status, ToolCallStatus::Ok);
    assert_eq!(tc.output.as_deref(), Some("secret-value"));

    let msgs = db.list_messages_page(&conv.id, None, 10).unwrap();
    assert_eq!(msgs[0].content, "File says secret-value");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denied_tool_call_does_not_execute_and_records_denial() {
    let dir = std::env::temp_dir().join(format!("companion_runner_deny_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Deny Tool").unwrap();
    let provider = MockModelProvider::new(vec![
        vec![
            ChatEvent::Token(
                "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"pwn.txt\",\"content\":\"bad\"}}</tool_call>".into(),
            ),
            ChatEvent::Done,
        ],
        vec![ChatEvent::Token("Okay, cancelled.".into()), ChatEvent::Done],
    ]);
    let approver = Arc::new(MockApprovalHandler::new(vec![ApprovalResponse::Tool(
        PermissionDecision::Deny,
    )]));
    let events = run_agent(
        provider,
        approver,
        &db,
        base_config(
            &conv.id,
            vec![GrantedFolder {
                path: dir.clone(),
                allow_write: false,
            }],
            false,
        ),
    );

    assert!(!dir.join("pwn.txt").exists());
    let tc = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallRecorded(tc) => Some(tc.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(tc.status, ToolCallStatus::Denied);
    assert_eq!(tc.output.as_deref(), Some("User denied permission."));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelled_empty_run_skips_persistence() {
    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Cancelled").unwrap();
    let cfg = base_config(&conv.id, vec![], false);
    cfg.cancel_flag.store(true, Ordering::Relaxed);

    let provider = MockModelProvider::new(vec![vec![
        ChatEvent::Token("Should not run".into()),
        ChatEvent::Done,
    ]]);
    let approver = Arc::new(MockApprovalHandler::new(vec![]));
    let events = run_agent(provider, approver, &db, cfg);

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Finished {
            saved_message_id: None,
            empty: true,
            ..
        }
    )));
    assert!(db.list_messages_page(&conv.id, None, 10).unwrap().is_empty());
}

#[test]
fn max_agent_steps_caps_infinite_tool_loops() {
    let dir = std::env::temp_dir().join(format!("companion_runner_cap_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open_in_memory().unwrap();
    let conv = db.create_conversation("Step Cap").unwrap();

    let loop_call = "<tool_call>{\"name\":\"list_files\",\"arguments\":{}}</tool_call>";
    let steps = (0..MAX_AGENT_STEPS)
        .map(|_| vec![ChatEvent::Token(loop_call.into()), ChatEvent::Done])
        .collect();
    let provider = MockModelProvider::new(steps);
    let approver = Arc::new(MockApprovalHandler::new(vec![]));
    let events = run_agent(
        provider,
        approver,
        &db,
        base_config(
            &conv.id,
            vec![GrantedFolder {
                path: dir.clone(),
                allow_write: false,
            }],
            false,
        ),
    );

    let tc_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallRecorded(_)))
        .count();
    assert_eq!(tc_count, MAX_AGENT_STEPS);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Notice(msg) if msg.contains("maximum tool call steps")
    )));
    let _ = std::fs::remove_dir_all(&dir);
}

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

    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::FolderGrantsUpdated)));
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
