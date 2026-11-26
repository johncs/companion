//! Pure, UI-agnostic multi-step ReAct agent runner.

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;
use tokio::sync::mpsc;

use crate::approval::{ApprovalHandler, ApprovalResponse};
use crate::coalescer::{StreamStepUpdate, combine_answer, spawn_stream_coalescer};
use crate::parser::{parse_tool_calls, strip_tool_calls};
use crate::permissions::{PermissionDecision, PermissionEngine};
use crate::registry::ToolRegistry;
use crate::tool::{ApprovalRequest, ToolContext, ToolRisk};
use crate::tools::{
    GrantedFolder, first_str_arg, resolve_target_folder_for_write, resolve_ungranted_raw,
};
use crate::web_search::{
    execute_fetch_url, execute_web_search, inspect_fetch_url, inspect_search_query,
    validate_fetch_url,
};
use companion_core::markdown::MarkdownBlock;
use companion_core::types::{
    ExecutionDevice, FolderPermission, MessageRole, SearchEngine, ToolCallStatus,
};
use companion_core::{ThinkingLevel, runtime};
use companion_core::i18n::tr_f;
use companion_core::provider::{ChatRole, ChatTurn, InferenceParams, ModelProvider};
use companion_storage::{Database, NewMessage, ToolCallRecord};

pub const MAX_AGENT_STEPS: usize = 5;

/// Events streamed from [`AgentRunner`] to any UI or test subscriber.
#[derive(Debug)]
pub enum AgentEvent {
    Status(String),
    StreamSnapshot {
        combined_answer: String,
        blocks: Option<Vec<MarkdownBlock>>,
        reasoning: Option<String>,
    },
    ToolCallRecorded(ToolCallRecord),
    FolderGrantsUpdated,
    ContextUsage {
        total_tokens: u32,
    },
    Notice(String),
    Error(String),
    Finished {
        #[allow(dead_code)]
        saved_message_id: Option<String>,
        total_tokens: i64,
        elapsed_ms: i64,
        empty: bool,
    },
    TitleUpdated(String),
}

/// All inputs required for a single multi-step agent run.
pub struct AgentRunConfig {
    pub conv_id: String,
    pub model_id: String,
    pub model_path: String,
    pub model_display_name: String,
    pub base_params: InferenceParams,
    pub thinking_level: ThinkingLevel,
    pub execution_device: ExecutionDevice,
    pub web_search_enabled: bool,
    pub granted_folders: Vec<GrantedFolder>,
    pub turns: Vec<ChatTurn>,
    pub cancel_flag: Arc<AtomicBool>,
}

/// Executes multi-step inference, tool approval, tool dispatch, and DB persistence off the UI thread.
#[derive(Clone)]
pub struct AgentRunner {
    pub provider: Arc<dyn ModelProvider>,
    pub registry: ToolRegistry,
    pub permissions: PermissionEngine,
    pub approver: Arc<dyn ApprovalHandler>,
    pub db: Database,
}

impl AgentRunner {
    /// Spawn the agent run on the shared Tokio runtime and return the event stream.
    pub fn spawn(self, cfg: AgentRunConfig) -> mpsc::UnboundedReceiver<AgentEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        runtime().spawn(async move {
            self.run(cfg, tx).await;
        });
        rx
    }

    /// Run the agent loop to completion, emitting [`AgentEvent`]s to `tx`.
    pub async fn run(self, mut cfg: AgentRunConfig, tx: mpsc::UnboundedSender<AgentEvent>) {
        let started = Instant::now();
        let mut full_answer = String::new();
        let mut full_reasoning = String::new();
        let mut recorded_tools: Vec<ToolCallRecord> = Vec::new();
        let mut total_completion_tokens: i64 = 0;
        let mut total_decode_ms: i64 = 0;

        for step in 0..MAX_AGENT_STEPS {
            if cfg.cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let step_out = self
                .run_single_step(
                    &cfg,
                    &full_answer,
                    &full_reasoning,
                    &mut total_completion_tokens,
                    &mut total_decode_ms,
                    &tx,
                )
                .await;
            full_reasoning = step_out.reasoning;
            let visible_step = strip_tool_calls(&step_out.step_answer);
            full_answer = combine_answer(&full_answer, &visible_step);

            let calls = parse_tool_calls(&step_out.step_answer);
            if calls.is_empty() || cfg.cancel_flag.load(Ordering::Relaxed) {
                break;
            }

            let mut tool_feedback = String::new();
            for call in calls {
                let (tc, output) = self
                    .dispatch_tool(&mut cfg, &recorded_tools, call, &tx)
                    .await;
                let tool_name = tc.tool_name.clone();
                let _ = tx.send(AgentEvent::ToolCallRecorded(tc.clone()));
                recorded_tools.push(tc);
                tool_feedback.push_str(&format!("<tool_result name=\"{tool_name}\">\n{output}\n</tool_result>\n"));
            }

            cfg.turns.push(ChatTurn {
                role: ChatRole::Assistant,
                content: step_out.step_answer,
                images: Vec::new(),
            });
            cfg.turns.push(ChatTurn {
                role: ChatRole::User,
                content: format!(
                    "{tool_feedback}\nUse the tool results above to continue. Output another <tool_call> if needed, or give your final answer."
                ),
                images: Vec::new(),
            });
            if step + 1 == MAX_AGENT_STEPS {
                let _ = tx.send(AgentEvent::Notice(
                    "Reached maximum tool call steps for this turn.".to_string(),
                ));
            }
        }

        self.finalize_run(
            &cfg,
            FinalizeRunArgs {
                started,
                full_answer,
                full_reasoning,
                recorded_tools,
                total_completion_tokens,
                total_decode_ms,
            },
            &tx,
        )
        .await;
    }

    async fn run_single_step(
        &self,
        cfg: &AgentRunConfig,
        full_answer_prefix: &str,
        initial_reasoning: &str,
        total_completion_tokens: &mut i64,
        total_decode_ms: &mut i64,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> StepOutput {
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let mut ui_rx = spawn_stream_coalescer(
            raw_rx,
            full_answer_prefix.to_string(),
            initial_reasoning.to_string(),
        );
        if let Err(err) = self.provider.stream_chat(
            cfg.model_path.clone(),
            cfg.turns.clone(),
            cfg.base_params.clone(),
            cfg.cancel_flag.clone(),
            raw_tx,
        ) {
            let _ = tx.send(AgentEvent::Error(tr_f(
                "Inference error: {err}",
                &[("err", &err.to_string())],
            )));
        }

        let mut step_answer = String::new();
        let mut reasoning = initial_reasoning.to_string();
        while let Some(update) = ui_rx.recv().await {
            match update {
                StreamStepUpdate::Status(s) => {
                    let _ = tx.send(AgentEvent::Status(s));
                }
                StreamStepUpdate::Snapshot {
                    combined,
                    blocks,
                    reasoning: r,
                } => {
                    let _ = tx.send(AgentEvent::StreamSnapshot {
                        combined_answer: combined,
                        blocks,
                        reasoning: r,
                    });
                }
                StreamStepUpdate::Usage {
                    prompt_tokens,
                    completion_tokens,
                    decode_ms,
                } => {
                    *total_completion_tokens += completion_tokens as i64;
                    if let Some(ms) = decode_ms {
                        *total_decode_ms += ms as i64;
                        if completion_tokens >= 2 && ms > 50 {
                            let tps = (completion_tokens as f64) / ((ms as f64) / 1000.0);
                            let db_speed = self.db.clone();
                            let mid = cfg.model_id.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                let _ = db_speed.record_model_speed(&mid, tps);
                            });
                        }
                    }
                    let _ = tx.send(AgentEvent::ContextUsage {
                        total_tokens: prompt_tokens + completion_tokens,
                    });
                }
                StreamStepUpdate::Notice(m) => {
                    let _ = tx.send(AgentEvent::Notice(m));
                }
                StreamStepUpdate::Error(err) => {
                    let _ = tx.send(AgentEvent::Error(tr_f(
                        "Inference error: {err}",
                        &[("err", &err)],
                    )));
                }
                StreamStepUpdate::Done {
                    step_answer: sa,
                    full_reasoning: fr,
                } => {
                    step_answer = sa;
                    reasoning = fr;
                    break;
                }
            }
        }
        StepOutput {
            step_answer,
            reasoning,
        }
    }

    async fn dispatch_tool(
        &self,
        cfg: &mut AgentRunConfig,
        recorded_tools: &[ToolCallRecord],
        call: crate::parser::ParsedToolCall,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        if call.name == "web_search" {
            let local_texts = collect_local_texts(recorded_tools, &cfg.turns);
            return self.run_web_search(cfg, &local_texts, call, tx).await;
        }
        if call.name == "fetch_url" {
            let local_texts = collect_local_texts(recorded_tools, &cfg.turns);
            return self.run_fetch_url(cfg, &local_texts, call, tx).await;
        }
        self.run_workspace_tool(cfg, call, tx).await
    }

    async fn run_workspace_tool(
        &self,
        cfg: &mut AgentRunConfig,
        call: crate::parser::ParsedToolCall,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let Some(tool) = self.registry.get(&call.name) else {
            let out = format!("Unknown tool: {}", call.name);
            return (
                build_tool_record(call, ToolCallStatus::Error, &out, PermissionDecision::Deny),
                out,
            );
        };

        let risk = tool.risk();
        let needs_write = matches!(risk, ToolRisk::WriteWorkspace | ToolRisk::Destructive);
        let ungranted_target = resolve_ungranted_raw(
            &cfg.granted_folders,
            tool.raw_target_path(&call.arguments),
            needs_write,
        );
        let preview_ctx = ToolContext {
            granted_folders: &cfg.granted_folders,
            web_search_enabled: false,
            search_engine: SearchEngine::default(),
            searxng_url: None,
        };

        let decision = if ungranted_target.is_none()
            && (self.permissions.is_auto_allowed(&cfg.conv_id, &call.name, risk)
                || tool.is_write_already_granted(&cfg.granted_folders, &call.arguments))
        {
            PermissionDecision::AllowOnce
        } else {
            let req = ApprovalRequest::ToolAction {
                tool_name: call.name.clone(),
                preview: tool.describe(&preview_ctx, &call.arguments),
                risk,
                ungranted_path: ungranted_target.clone(),
            };
            let d = match self.approver.request_approval(req).await {
                ApprovalResponse::Tool(d) => d,
                _ => PermissionDecision::Deny,
            };
            if ungranted_target.is_none() {
                self.permissions
                    .record_decision(&cfg.conv_id, &call.name, risk, d);
            }
            d
        };

        let write_target_folder = if needs_write && ungranted_target.is_none() {
            resolve_target_folder_for_write(&cfg.granted_folders, &call.name, &call.arguments)
        } else {
            None
        };

        let (status, output) = if decision == PermissionDecision::Deny {
            (ToolCallStatus::Denied, "User denied permission.".to_string())
        } else {
            let _ = tx.send(AgentEvent::Status(tr_f(
                "Running {tool}…",
                &[("tool", &call.name)],
            )));
            let mut grants = cfg.granted_folders.clone();
            if let Some(ref target) = ungranted_target {
                grants.push(GrantedFolder {
                    path: target.clone(),
                    allow_write: needs_write,
                });
            } else if needs_write && let Some(ref exact_folder) = write_target_folder {
                for g in &mut grants {
                    if &g.path == exact_folder {
                        g.allow_write = true;
                    }
                }
            }
            let exec_ctx = ToolContext {
                granted_folders: &grants,
                web_search_enabled: false,
                search_engine: SearchEngine::default(),
                searxng_url: None,
            };
            match tool.execute(&exec_ctx, &call.arguments).await {
                Ok(out) => (ToolCallStatus::Ok, out),
                Err(err) => (ToolCallStatus::Error, err.to_string()),
            }
        };

        if decision == PermissionDecision::AllowChat {
            self.persist_allow_chat_grant(
                cfg,
                risk == ToolRisk::WriteWorkspace,
                ungranted_target,
                write_target_folder,
                tx,
            )
            .await;
        }

        let rec = build_tool_record(call, status, &output, decision);
        (rec, output)
    }

    async fn persist_allow_chat_grant(
        &self,
        cfg: &mut AgentRunConfig,
        allow_write: bool,
        ungranted_target: Option<PathBuf>,
        write_target_folder: Option<PathBuf>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let target_to_persist = if let Some(new_path) = ungranted_target {
            if !cfg.granted_folders.iter().any(|g| g.path == new_path) {
                cfg.granted_folders.push(GrantedFolder {
                    path: new_path.clone(),
                    allow_write,
                });
            }
            Some(new_path)
        } else if allow_write && let Some(exact) = write_target_folder {
            for gf in &mut cfg.granted_folders {
                if gf.path == exact {
                    gf.allow_write = true;
                }
            }
            Some(exact)
        } else {
            None
        };

        if let Some(path) = target_to_persist {
            let db = self.db.clone();
            let conv_id = cfg.conv_id.clone();
            let path_str = path.to_string_lossy().to_string();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(entry) = db.add_conversation_folder(&conv_id, &path_str)
                    && allow_write
                {
                    let _ = db
                        .set_conversation_folder_permission(&entry.id, FolderPermission::ReadWrite);
                }
            })
            .await;
            let _ = tx.send(AgentEvent::FolderGrantsUpdated);
        }
    }

    async fn run_web_search(
        &self,
        cfg: &AgentRunConfig,
        local_texts: &[String],
        call: crate::parser::ParsedToolCall,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let proposed_query = first_str_arg(&call.arguments, &["query", "q"])
            .unwrap_or("")
            .trim()
            .to_string();
        let db_cfg = self.db.clone();
        let (global_enabled, engine, searxng_url) = tokio::task::spawn_blocking(move || {
            (
                db_cfg.get_web_search_global_enabled().unwrap_or(false),
                db_cfg.get_search_engine().unwrap_or_default(),
                db_cfg.get_searxng_url().ok().flatten(),
            )
        })
        .await
        .unwrap_or((false, SearchEngine::default(), None));

        let (status, output, decision) = if !global_enabled || !cfg.web_search_enabled {
            (
                ToolCallStatus::Denied,
                "Web search is disabled for this chat.".to_string(),
                PermissionDecision::Deny,
            )
        } else if proposed_query.is_empty() {
            (
                ToolCallStatus::Error,
                "Missing 'query' argument".to_string(),
                PermissionDecision::Deny,
            )
        } else {
            let warnings = inspect_search_query(&proposed_query, local_texts);
            let req = ApprovalRequest::WebSearch {
                engine_label: engine.label(),
                proposed_query,
                warnings,
            };
            match self.approver.request_approval(req).await {
                ApprovalResponse::WebSearch(Some(final_query)) => {
                    let _ = tx.send(AgentEvent::Status(tr_f(
                        "Searching {engine}…",
                        &[("engine", &engine.label())],
                    )));
                    let exec_args = json!({ "query": final_query });
                    let ctx = ToolContext {
                        granted_folders: &[],
                        web_search_enabled: true,
                        search_engine: engine,
                        searxng_url: searxng_url.as_deref(),
                    };
                    let res = match self.registry.get("web_search") {
                        Some(tool) => tool.execute(&ctx, &exec_args).await,
                        None => execute_web_search(engine, searxng_url.as_deref(), &final_query).await,
                    };
                    match res {
                        Ok(out) => (ToolCallStatus::Ok, out, PermissionDecision::AllowOnce),
                        Err(err) => (
                            ToolCallStatus::Error,
                            err.to_string(),
                            PermissionDecision::AllowOnce,
                        ),
                    }
                }
                _ => (
                    ToolCallStatus::Denied,
                    "User denied web search.".to_string(),
                    PermissionDecision::Deny,
                ),
            }
        };

        let rec = build_tool_record(call, status, &output, decision);
        (rec, output)
    }

    async fn run_fetch_url(
        &self,
        cfg: &AgentRunConfig,
        local_texts: &[String],
        call: crate::parser::ParsedToolCall,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let proposed_url = first_str_arg(&call.arguments, &["url"])
            .unwrap_or("")
            .trim()
            .to_string();
        let db_cfg = self.db.clone();
        let global_enabled = tokio::task::spawn_blocking(move || {
            db_cfg.get_web_search_global_enabled().unwrap_or(false)
        })
        .await
        .unwrap_or(false);

        let (status, output, decision) = if !global_enabled || !cfg.web_search_enabled {
            (
                ToolCallStatus::Denied,
                "Web search is disabled for this chat.".to_string(),
                PermissionDecision::Deny,
            )
        } else if proposed_url.is_empty() {
            (
                ToolCallStatus::Error,
                "Missing 'url' argument".to_string(),
                PermissionDecision::Deny,
            )
        } else if let Err(err) = validate_fetch_url(&proposed_url) {
            (
                ToolCallStatus::Error,
                err.to_string(),
                PermissionDecision::Deny,
            )
        } else {
            let warnings = inspect_fetch_url(&proposed_url, local_texts);
            let req = ApprovalRequest::FetchUrl {
                proposed_url,
                warnings,
            };
            match self.approver.request_approval(req).await {
                ApprovalResponse::FetchUrl(Some(final_url)) => {
                    if let Err(err) = validate_fetch_url(&final_url) {
                        (
                            ToolCallStatus::Error,
                            err.to_string(),
                            PermissionDecision::Deny,
                        )
                    } else {
                        let _ = tx.send(AgentEvent::Status(
                            companion_core::i18n::gettext("Fetching web page…"),
                        ));
                        let exec_args = json!({ "url": final_url });
                        let ctx = ToolContext {
                            granted_folders: &[],
                            web_search_enabled: true,
                            search_engine: SearchEngine::default(),
                            searxng_url: None,
                        };
                        let res = match self.registry.get("fetch_url") {
                            Some(tool) => tool.execute(&ctx, &exec_args).await,
                            None => execute_fetch_url(&final_url).await,
                        };
                        match res {
                            Ok(out) => (ToolCallStatus::Ok, out, PermissionDecision::AllowOnce),
                            Err(err) => (
                                ToolCallStatus::Error,
                                err.to_string(),
                                PermissionDecision::AllowOnce,
                            ),
                        }
                    }
                }
                _ => (
                    ToolCallStatus::Denied,
                    "User denied web page fetch.".to_string(),
                    PermissionDecision::Deny,
                ),
            }
        };

        let rec = build_tool_record(call, status, &output, decision);
        (rec, output)
    }

    async fn finalize_run(
        &self,
        cfg: &AgentRunConfig,
        args: FinalizeRunArgs,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let FinalizeRunArgs {
            started,
            full_answer,
            full_reasoning,
            recorded_tools,
            total_completion_tokens,
            total_decode_ms,
        } = args;
        let was_cancelled = cfg.cancel_flag.load(Ordering::Relaxed);
        let is_empty = full_answer.trim().is_empty()
            && full_reasoning.trim().is_empty()
            && recorded_tools.is_empty();
        if was_cancelled && is_empty {
            let _ = tx.send(AgentEvent::Finished {
                saved_message_id: None,
                total_tokens: 0,
                elapsed_ms: 0,
                empty: true,
            });
            return;
        }

        let elapsed_ms = if total_decode_ms > 0 {
            total_decode_ms
        } else {
            started.elapsed().as_millis() as i64
        };

        let db_save = self.db.clone();
        let conv_save = cfg.conv_id.clone();
        let answer_save = full_answer.clone();
        let reasoning_save = (!full_reasoning.is_empty()).then_some(full_reasoning);
        let thinking = cfg.thinking_level;
        let model_name_save = cfg.model_display_name.clone();
        let device = cfg.execution_device;

        let saved_id = tokio::task::spawn_blocking(move || {
            let saved = db_save
                .insert_message(NewMessage {
                    conversation_id: &conv_save,
                    role: MessageRole::Assistant,
                    content: &answer_save,
                    reasoning_content: reasoning_save.as_deref(),
                    thinking_level: Some(thinking),
                    model_name: Some(&model_name_save),
                    execution_device: Some(device),
                    token_count: total_completion_tokens,
                    generation_ms: Some(elapsed_ms),
                })
                .ok()?;
            for mut tc in recorded_tools {
                tc.message_id = saved.id.clone();
                let _ = db_save.insert_tool_call(&tc);
            }
            Some(saved.id)
        })
        .await
        .ok()
        .flatten();

        let _ = tx.send(AgentEvent::Finished {
            saved_message_id: saved_id,
            total_tokens: total_completion_tokens,
            elapsed_ms,
            empty: false,
        });

        if !was_cancelled
            && !full_answer.trim().is_empty()
            && let Some(new_title) = crate::title_gen::maybe_generate_title(
                &self.db,
                self.provider.as_ref(),
                &cfg.model_path,
                &cfg.base_params,
                &cfg.conv_id,
                &cfg.turns,
                &cfg.cancel_flag,
            )
            .await
        {
            let _ = tx.send(AgentEvent::TitleUpdated(new_title));
        }
    }
}

struct FinalizeRunArgs {
    started: Instant,
    full_answer: String,
    full_reasoning: String,
    recorded_tools: Vec<ToolCallRecord>,
    total_completion_tokens: i64,
    total_decode_ms: i64,
}

struct StepOutput {
    step_answer: String,
    reasoning: String,
}

fn collect_local_texts(recorded_tools: &[ToolCallRecord], turns: &[ChatTurn]) -> Vec<String> {
    let mut local_texts: Vec<String> = recorded_tools
        .iter()
        .filter(|t| t.tool_name == "read_file" && t.status == ToolCallStatus::Ok)
        .filter_map(|t| t.output.clone())
        .collect();
    for t in turns {
        if matches!(t.role, ChatRole::User) && t.content.contains("<attachment") {
            local_texts.push(t.content.clone());
        }
    }
    local_texts
}

fn build_tool_record(
    call: crate::parser::ParsedToolCall,
    status: ToolCallStatus,
    output: &str,
    decision: PermissionDecision,
) -> ToolCallRecord {
    ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.to_string()),
        permission_decision: decision,
        created_at: chrono::Utc::now().timestamp_millis(),
    }
}
