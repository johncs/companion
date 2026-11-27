//! Token sampling loop and stop-notice / chunk emission for `llama.cpp`.

use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::vocab::LlamaVocab;
use tokio::sync::mpsc;

use crate::think_parser::ParsedChunk;
use crate::token_pipeline::TokenStreamPipeline;
use companion_core::provider::ChatEvent;

use super::runner::{InferenceRun, PromptEvalResult, StopReason, build_sampler};

struct DecodeLoopState<'b> {
    sampler: LlamaSampler,
    pipeline: TokenStreamPipeline,
    vocab: LlamaVocab<'b>,
    ctx_limit_i32: i32,
    completion_tokens: u32,
}

impl<'a> InferenceRun<'a> {
    pub(super) fn run_decode_loop(
        &self,
        eval: &mut PromptEvalResult<'a>,
        batch: &mut LlamaBatch,
        initial_close_tag: Option<&'static str>,
    ) -> Result<(StopReason, u32, u64)> {
        let mut state = DecodeLoopState {
            sampler: build_sampler(self.params.temperature),
            pipeline: TokenStreamPipeline::new(initial_close_tag),
            vocab: self.model.vocab(),
            ctx_limit_i32: i32::try_from(eval.ctx_size).unwrap_or(i32::MAX),
            completion_tokens: 0,
        };
        let gen_start = std::time::Instant::now();
        let mut stop_reason = StopReason::MaxTokens;

        for _ in 0..self.params.max_tokens {
            if self.cancel_flag.load(Ordering::Relaxed) {
                stop_reason = StopReason::Cancelled;
                break;
            }
            if let Some(reason) = self.decode_next_token(eval, batch, &mut state)? {
                stop_reason = reason;
                break;
            }
        }

        emit_parsed(&state.pipeline.finish(), self.tx);
        Ok((
            stop_reason,
            state.completion_tokens,
            gen_start.elapsed().as_millis() as u64,
        ))
    }

    fn decode_next_token(
        &self,
        eval: &mut PromptEvalResult<'a>,
        batch: &mut LlamaBatch,
        state: &mut DecodeLoopState<'_>,
    ) -> Result<Option<StopReason>> {
        let token = state.sampler.sample(&eval.ctx, -1);
        state.sampler.accept(token);
        if state.vocab.is_eog(token) {
            return Ok(Some(StopReason::Natural));
        }
        state.completion_tokens = state.completion_tokens.saturating_add(1);

        let output_bytes = state.vocab.token_to_piece(token, true, None);
        let step = state.pipeline.push_bytes(&output_bytes);
        if !step.chunks.is_empty() {
            emit_parsed(&step.chunks, self.tx);
        }
        if step.stopped {
            return Ok(Some(StopReason::Natural));
        }
        if eval.pos >= state.ctx_limit_i32 {
            return Ok(Some(StopReason::ContextLimit));
        }
        batch.clear();
        batch.add(token, eval.pos, &[0], true)?;
        eval.pos = eval.pos.saturating_add(1);
        eval.ctx.decode(batch).context("Token decode failed")?;
        if !eval.evaluated_tokens.is_empty() {
            eval.evaluated_tokens.push(token);
        }
        if state.completion_tokens.is_multiple_of(16) {
            std::thread::yield_now();
        }
        Ok(None)
    }
}

pub(super) fn emit_stop_notice(reason: StopReason, tx: &mpsc::UnboundedSender<ChatEvent>) {
    match reason {
        StopReason::ContextLimit => {
            let _ = tx.send(ChatEvent::Notice(companion_core::i18n::gettext(
                "Response stopped: reached the context window limit",
            )));
        }
        StopReason::MaxTokens => {
            let _ = tx.send(ChatEvent::Notice(companion_core::i18n::gettext(
                "Response stopped: reached the maximum reply length",
            )));
        }
        StopReason::Natural | StopReason::Cancelled => {}
    }
}

fn emit_parsed(chunks: &[ParsedChunk], tx: &mpsc::UnboundedSender<ChatEvent>) {
    for chunk in chunks {
        match chunk {
            ParsedChunk::Answer(t) => {
                let _ = tx.send(ChatEvent::Token(t.clone()));
            }
            ParsedChunk::Reasoning(r) => {
                let _ = tx.send(ChatEvent::ReasoningToken(r.clone()));
            }
        }
    }
}
