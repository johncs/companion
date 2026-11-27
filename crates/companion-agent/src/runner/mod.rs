//! Multi-step agent orchestrator coordinating inference, tool dispatch, approvals, and persistence.

mod dispatch;
mod finalize;
mod step;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tokio::sync::mpsc;

use crate::approval::ApprovalHandler;
use crate::coalescer::combine_answer;
use crate::parser::{ParsedToolCall, parse_tool_calls, strip_tool_calls};
use crate::permissions::PermissionEngine;
use crate::registry::ToolRegistry;
use crate::tools::GrantedFolder;
use companion_core::ThinkingLevel;
use companion_core::provider::{ChatRole, ChatTurn, InferenceParams, ModelProvider};
use companion_core::types::ExecutionDevice;
use companion_format::markdown::MarkdownBlock;
use companion_storage::{Database, ToolCallRecord};

use finalize::FinalizeRunArgs;

pub const MAX_AGENT_STEPS: usize = 5;

/// Streaming UI events emitted by [`AgentRunner`].
#[derive(Debug, Clone)]
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
        saved_message_id: Option<String>,
        created_at: Option<i64>,
        final_answer: String,
        final_reasoning: Option<String>,
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
    /// Spawn the agent run on `handle` and return the event stream.
    pub fn spawn_on(
        self,
        handle: &tokio::runtime::Handle,
        cfg: AgentRunConfig,
    ) -> mpsc::UnboundedReceiver<AgentEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        handle.spawn(async move {
            self.run(cfg, tx).await;
        });
        rx
    }

    /// Spawn the agent run on the current Tokio runtime and return the event stream.
    pub fn spawn(self, cfg: AgentRunConfig) -> mpsc::UnboundedReceiver<AgentEvent> {
        self.spawn_on(&tokio::runtime::Handle::current(), cfg)
    }

    /// Run the agent loop to completion, emitting [`AgentEvent`]s to `tx`.
    pub async fn run(self, mut cfg: AgentRunConfig, tx: mpsc::UnboundedSender<AgentEvent>) {
        let started = Instant::now();
        let mut full_answer = String::new();
        let mut full_reasoning = String::new();
        let mut recorded_tools: Vec<ToolCallRecord> = Vec::new();
        let mut total_completion_tokens: i64 = 0;
        let mut total_decode_ms: i64 = 0;
        let mut code_cache = companion_format::markdown::CodeHighlightCache::new();

        for step in 0..MAX_AGENT_STEPS {
            if cfg.cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let input = step::StepInput {
                full_answer_prefix: &full_answer,
                initial_reasoning: &full_reasoning,
                code_cache,
                total_completion_tokens: &mut total_completion_tokens,
                total_decode_ms: &mut total_decode_ms,
            };
            let step_out = self.run_single_step(&cfg, input, &tx).await;
            code_cache = step_out.code_cache;
            full_reasoning = step_out.reasoning;
            let visible_step = strip_tool_calls(&step_out.step_answer);
            full_answer = combine_answer(&full_answer, &visible_step);

            let calls = parse_tool_calls(&step_out.step_answer);
            if calls.is_empty() || cfg.cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let tool_feedback = self
                .execute_tool_batch(&mut cfg, &mut recorded_tools, calls, &tx)
                .await;
            push_tool_feedback_turns(&mut cfg, step_out.step_answer, &tool_feedback);
            if step + 1 == MAX_AGENT_STEPS {
                let _ = tx.send(AgentEvent::Notice(companion_core::i18n::gettext(
                    "Reached maximum tool call steps for this turn.",
                )));
            }
        }
        let fin = FinalizeRunArgs {
            started,
            full_answer,
            full_reasoning,
            recorded_tools,
            total_completion_tokens,
            total_decode_ms,
        };
        self.finalize_run(&cfg, fin, &tx).await;
    }

    async fn execute_tool_batch(
        &self,
        cfg: &mut AgentRunConfig,
        recorded_tools: &mut Vec<ToolCallRecord>,
        calls: Vec<crate::parser::ParsedToolCall>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> String {
        let mut tool_feedback = String::new();
        for call in calls {
            if cfg.cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let (tc, output) = self.dispatch_tool(cfg, recorded_tools, call, tx).await;
            let tool_name = tc.tool_name.clone();
            let _ = tx.send(AgentEvent::ToolCallRecorded(tc.clone()));
            recorded_tools.push(tc);
            tool_feedback.push_str(&format!(
                "<tool_result name=\"{tool_name}\">\n{output}\n</tool_result>\n"
            ));
        }
        tool_feedback
    }
}

fn push_tool_feedback_turns(cfg: &mut AgentRunConfig, step_answer: String, tool_feedback: &str) {
    cfg.turns.push(ChatTurn {
        role: ChatRole::Assistant,
        content: step_answer,
        images: Vec::new(),
    });
    cfg.turns.push(ChatTurn {
        role: ChatRole::User,
        content: format!(
            "{tool_feedback}\nUse the tool results above to continue. Output another <tool_call> if needed, or give your final answer."
        ),
        images: Vec::new(),
    });
}

pub(super) fn build_tool_record(
    call: ParsedToolCall,
    status: companion_core::types::ToolCallStatus,
    output: &str,
    decision: companion_core::types::PermissionDecision,
) -> companion_storage::ToolCallRecord {
    companion_storage::ToolCallRecord {
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

#[cfg(test)]
mod tests_loop;
#[cfg(test)]
mod tests_perms;
