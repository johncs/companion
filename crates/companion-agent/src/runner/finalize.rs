//! Run finalization: SQLite persistence, error reporting, and title generation.

use std::sync::atomic::Ordering;
use std::time::Instant;

use tokio::sync::mpsc;

use companion_core::types::MessageRole;
use companion_storage::{NewMessage, ToolCallRecord};

use super::{AgentEvent, AgentRunConfig, AgentRunner};

pub(super) struct FinalizeRunArgs {
    pub(super) started: Instant,
    pub(super) full_answer: String,
    pub(super) full_reasoning: String,
    pub(super) recorded_tools: Vec<ToolCallRecord>,
    pub(super) total_completion_tokens: i64,
    pub(super) total_decode_ms: i64,
}

impl AgentRunner {
    pub(super) async fn finalize_run(
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
                created_at: None,
                final_answer: String::new(),
                final_reasoning: None,
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
        let reasoning_save = (!full_reasoning.is_empty()).then_some(full_reasoning.clone());
        let thinking = cfg.thinking_level;
        let model_name_save = cfg.model_display_name.clone();
        let device = cfg.execution_device;

        let save_res: Result<(String, i64), String> = tokio::task::spawn_blocking(move || {
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
                .map_err(|e| format!("Failed to save reply: {e}"))?;
            for mut tc in recorded_tools {
                tc.message_id = saved.id.clone();
                db_save
                    .insert_tool_call(&tc)
                    .map_err(|e| format!("Failed to save tool call: {e}"))?;
            }
            Ok((saved.id, saved.created_at))
        })
        .await
        .map_err(|e| format!("Failed to persist reply: {e}"))
        .and_then(|inner| inner);

        let (saved_message_id, created_at) = match save_res {
            Ok((id, ts)) => (Some(id), Some(ts)),
            Err(err) => {
                tracing::error!("{err}");
                let _ = tx.send(AgentEvent::Error(err));
                (None, None)
            }
        };

        let final_reasoning = (!full_reasoning.is_empty()).then_some(full_reasoning);
        let _ = tx.send(AgentEvent::Finished {
            saved_message_id,
            created_at,
            final_answer: full_answer.clone(),
            final_reasoning,
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
