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
        let was_cancelled = cfg.cancel_flag.load(Ordering::Relaxed);
        let is_empty = args.full_answer.trim().is_empty()
            && args.full_reasoning.trim().is_empty()
            && args.recorded_tools.is_empty();
        if is_empty {
            emit_empty_finished(tx);
            return;
        }
        let elapsed_ms = if args.total_decode_ms > 0 {
            args.total_decode_ms
        } else {
            args.started.elapsed().as_millis() as i64
        };
        let (saved_message_id, created_at) = self
            .persist_reply(
                cfg,
                (&args.full_answer, &args.full_reasoning),
                args.recorded_tools,
                (args.total_completion_tokens, elapsed_ms),
                tx,
            )
            .await;
        let final_reasoning = (!args.full_reasoning.is_empty()).then_some(args.full_reasoning);
        emit_reply_finished(
            saved_message_id,
            created_at,
            args.full_answer.clone(),
            final_reasoning,
            args.total_completion_tokens,
            elapsed_ms,
            tx,
        );
        self.maybe_emit_title_update(cfg, &args.full_answer, was_cancelled, tx)
            .await;
    }

    async fn maybe_emit_title_update(
        &self,
        cfg: &AgentRunConfig,
        full_answer: &str,
        was_cancelled: bool,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
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

    async fn persist_reply(
        &self,
        cfg: &AgentRunConfig,
        (full_answer, full_reasoning): (&str, &str),
        recorded_tools: Vec<ToolCallRecord>,
        (total_completion_tokens, elapsed_ms): (i64, i64),
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (Option<String>, Option<i64>) {
        let db_save = self.db.clone();
        let conv_save = cfg.conv_id.clone();
        let answer_save = full_answer.to_string();
        let reasoning_save = (!full_reasoning.is_empty()).then_some(full_reasoning.to_string());
        let thinking = cfg.thinking_level;
        let model_name_save = cfg.model_display_name.clone();
        let device = cfg.execution_device;

        let save_res: Result<(String, i64), String> = tokio::task::spawn_blocking(move || {
            let saved = db_save
                .insert_message_with_tool_calls(
                    NewMessage {
                        conversation_id: &conv_save,
                        role: MessageRole::Assistant,
                        content: &answer_save,
                        reasoning_content: reasoning_save.as_deref(),
                        thinking_level: Some(thinking),
                        model_name: Some(&model_name_save),
                        execution_device: Some(device),
                        token_count: total_completion_tokens,
                        generation_ms: Some(elapsed_ms),
                    },
                    &recorded_tools,
                )
                .map_err(|e| format!("Failed to save reply: {e}"))?;
            Ok((saved.id, saved.created_at))
        })
        .await
        .map_err(|e| format!("Failed to persist reply: {e}"))
        .and_then(|inner| inner);

        match save_res {
            Ok((id, ts)) => (Some(id), Some(ts)),
            Err(err) => {
                tracing::error!("{err}");
                let _ = tx.send(AgentEvent::Error(err));
                (None, None)
            }
        }
    }
}

fn emit_empty_finished(tx: &mpsc::UnboundedSender<AgentEvent>) {
    emit_reply_finished(None, None, String::new(), None, 0, 0, tx);
}

fn emit_reply_finished(
    saved_message_id: Option<String>,
    created_at: Option<i64>,
    final_answer: String,
    final_reasoning: Option<String>,
    total_tokens: i64,
    elapsed_ms: i64,
    tx: &mpsc::UnboundedSender<AgentEvent>,
) {
    let empty = saved_message_id.is_none()
        && final_answer.is_empty()
        && final_reasoning.is_none()
        && total_tokens == 0;
    let _ = tx.send(AgentEvent::Finished {
        saved_message_id,
        created_at,
        final_answer,
        final_reasoning,
        total_tokens,
        elapsed_ms,
        empty,
    });
}
