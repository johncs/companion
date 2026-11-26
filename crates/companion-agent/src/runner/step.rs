//! Single inference step execution and token-speed recording.

use tokio::sync::mpsc;

use crate::coalescer::{StreamStepUpdate, spawn_stream_coalescer};
use companion_core::i18n::tr_f;

use super::{AgentEvent, AgentRunConfig, AgentRunner};

pub(super) struct StepOutput {
    pub(super) step_answer: String,
    pub(super) reasoning: String,
}

impl AgentRunner {
    pub(super) async fn run_single_step(
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
                    self.record_step_usage(
                        cfg,
                        prompt_tokens,
                        completion_tokens,
                        decode_ms,
                        total_completion_tokens,
                        total_decode_ms,
                        tx,
                    );
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

    #[allow(clippy::too_many_arguments)]
    fn record_step_usage(
        &self,
        cfg: &AgentRunConfig,
        prompt_tokens: u32,
        completion_tokens: u32,
        decode_ms: Option<u64>,
        total_completion_tokens: &mut i64,
        total_decode_ms: &mut i64,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        *total_completion_tokens += completion_tokens as i64;
        if let Some(ms) = decode_ms {
            *total_decode_ms += ms as i64;
            if completion_tokens >= 2 && ms > 50 {
                let tps = (completion_tokens as f64) / ((ms as f64) / 1000.0);
                let db_speed = self.db.clone();
                let mid = cfg.model_id.clone();
                std::mem::drop(tokio::task::spawn_blocking(move || {
                    let _ = db_speed.record_model_speed(&mid, tps);
                }));
            }
        }
        let _ = tx.send(AgentEvent::ContextUsage {
            total_tokens: prompt_tokens + completion_tokens,
        });
    }
}
