//! Single inference step execution and token-speed recording.

use tokio::sync::mpsc;

use crate::coalescer::{StreamStepUpdate, spawn_stream_coalescer};
use companion_core::i18n::tr_f;
use companion_format::markdown::CodeHighlightCache;

use super::{AgentEvent, AgentRunConfig, AgentRunner};

pub(super) struct StepInput<'a> {
    pub(super) full_answer_prefix: &'a str,
    pub(super) initial_reasoning: &'a str,
    pub(super) code_cache: CodeHighlightCache,
    pub(super) total_completion_tokens: &'a mut i64,
    pub(super) total_decode_ms: &'a mut i64,
}

pub(super) struct StepOutput {
    pub(super) step_answer: String,
    pub(super) reasoning: String,
    pub(super) code_cache: CodeHighlightCache,
}

impl AgentRunner {
    pub(super) async fn run_single_step(
        &self,
        cfg: &AgentRunConfig,
        input: StepInput<'_>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> StepOutput {
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let mut ui_rx = spawn_stream_coalescer(
            &tokio::runtime::Handle::current(),
            raw_rx,
            input.full_answer_prefix.to_string(),
            input.initial_reasoning.to_string(),
            input.code_cache,
        );
        if let Err(err) = self
            .provider
            .stream_chat(companion_core::provider::ChatRequest {
                model_path: cfg.model_path.clone(),
                messages: cfg.turns.clone(),
                params: cfg.base_params.clone(),
                cancel_flag: cfg.cancel_flag.clone(),
                tx: raw_tx,
            })
        {
            let _ = tx.send(AgentEvent::Error(tr_f(
                "Inference error: {err}",
                &[("err", &err.to_string())],
            )));
        }

        let mut out = StepOutput {
            step_answer: String::new(),
            reasoning: input.initial_reasoning.to_string(),
            code_cache: CodeHighlightCache::new(),
        };
        while let Some(update) = ui_rx.recv().await {
            if self.handle_step_update(
                cfg,
                update,
                &mut out,
                input.total_completion_tokens,
                input.total_decode_ms,
                tx,
            ) {
                break;
            }
        }
        out
    }

    fn handle_step_update(
        &self,
        cfg: &AgentRunConfig,
        update: StreamStepUpdate,
        out: &mut StepOutput,
        total_completion_tokens: &mut i64,
        total_decode_ms: &mut i64,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> bool {
        match update {
            StreamStepUpdate::Status(s) => drop(tx.send(AgentEvent::Status(s))),
            StreamStepUpdate::Snapshot {
                combined,
                blocks,
                reasoning,
            } => {
                emit_snapshot(tx, combined, blocks, reasoning);
            }
            StreamStepUpdate::Usage {
                prompt_tokens,
                completion_tokens,
                decode_ms,
            } => {
                let usage = UsageReport {
                    prompt_tokens,
                    completion_tokens,
                    decode_ms,
                };
                self.record_step_usage(cfg, usage, total_completion_tokens, total_decode_ms, tx);
            }
            StreamStepUpdate::Notice(m) => drop(tx.send(AgentEvent::Notice(m))),
            StreamStepUpdate::Error(err) => {
                drop(tx.send(AgentEvent::Error(tr_f(
                    "Inference error: {err}",
                    &[("err", &err)],
                ))));
            }
            StreamStepUpdate::Done {
                step_answer,
                full_reasoning,
                code_cache,
            } => {
                out.step_answer = step_answer;
                out.reasoning = full_reasoning;
                out.code_cache = code_cache;
                return true;
            }
        }
        false
    }

    fn record_step_usage(
        &self,
        cfg: &AgentRunConfig,
        usage: UsageReport,
        total_completion_tokens: &mut i64,
        total_decode_ms: &mut i64,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        *total_completion_tokens += usage.completion_tokens as i64;
        if let Some(ms) = usage.decode_ms {
            *total_decode_ms += ms as i64;
            if usage.completion_tokens >= 2 && ms > 50 {
                let tps = (usage.completion_tokens as f64) / ((ms as f64) / 1000.0);
                let db_speed = self.db.clone();
                let mid = cfg.model_id.clone();
                std::mem::drop(tokio::task::spawn_blocking(move || {
                    match db_speed.get_model_speed_stats(&mid) {
                        Ok(Some((prev_avg, prev_count))) => {
                            if let Some((new_avg, new_count)) =
                                companion_core::types::compute_running_speed_avg(
                                    prev_avg, prev_count, tps,
                                )
                                && let Err(err) =
                                    db_speed.update_model_speed(&mid, new_avg, new_count)
                            {
                                tracing::warn!("Failed to record model speed for {mid}: {err}");
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!("Failed to read model speed stats for {mid}: {err}");
                        }
                    }
                }));
            }
        }
        let _ = tx.send(AgentEvent::ContextUsage {
            total_tokens: usage.prompt_tokens + usage.completion_tokens,
        });
    }
}

fn emit_snapshot(
    tx: &mpsc::UnboundedSender<AgentEvent>,
    combined_answer: String,
    blocks: Option<Vec<companion_format::markdown::MarkdownBlock>>,
    reasoning: Option<String>,
) {
    let _ = tx.send(AgentEvent::StreamSnapshot {
        combined_answer,
        blocks,
        reasoning,
    });
}

struct UsageReport {
    prompt_tokens: u32,
    completion_tokens: u32,
    decode_ms: Option<u64>,
}
