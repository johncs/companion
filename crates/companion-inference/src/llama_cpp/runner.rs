//! Blocking `llama.cpp` prompt evaluation, sampling loop, and event streaming.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::mtmd::{MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText};
use llama_cpp_2::sampling::LlamaSampler;
use tokio::sync::mpsc;

use crate::stop_filter::{StopOutput, StopSequenceFilter};
use crate::think_parser::{ParsedChunk, ThinkStreamParser};
use companion_core::provider::{ChatEvent, ChatTurn, InferenceParams};

use super::prompt::{format_chat_prompt, trim_turns_to_budget};
use super::{LlamaCppProvider, get_backend};

enum StopReason {
    Natural,
    Cancelled,
    ContextLimit,
    MaxTokens,
}

struct PromptEvalResult<'a> {
    ctx: LlamaContext<'a>,
    pos: i32,
    prompt_tokens: u32,
    ctx_size: u32,
}

struct InferenceRun<'a> {
    backend: &'a LlamaBackend,
    model: &'a LlamaModel,
    params: &'a InferenceParams,
    ui_friendly_threads: i32,
    n_batch: usize,
    cancel_flag: &'a AtomicBool,
    tx: &'a mpsc::UnboundedSender<ChatEvent>,
}

pub(super) fn run_inference_blocking(
    provider: &LlamaCppProvider,
    model_path: &str,
    messages: &[ChatTurn],
    params: &InferenceParams,
    cancel_flag: &AtomicBool,
    tx: &mpsc::UnboundedSender<ChatEvent>,
) -> Result<()> {
    let backend = get_backend()?;
    let model = provider.get_or_load_model(&backend, model_path, params.n_gpu_layers, tx)?;
    let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
        "Generating…",
    )));

    if cancel_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    let image_paths: Vec<&str> = messages
        .iter()
        .flat_map(|m| m.images.iter().map(String::as_str))
        .collect();
    let active_mmproj = params
        .mmproj_path
        .as_deref()
        .filter(|p| !image_paths.is_empty() && Path::new(p).exists());

    let vocab = model.vocab();
    let trimmed_messages = trim_turns_to_budget(messages, params.n_ctx, |s| {
        vocab.tokenize(s.as_bytes(), false, true).len()
    });
    let (prompt, initial_close_tag) = format_chat_prompt(
        &model,
        &trimmed_messages,
        params.thinking_level,
        active_mmproj.is_some(),
    )?;

    let (ui_friendly_threads, n_batch) = compute_thread_config(params.n_gpu_layers);
    let mut batch = LlamaBatch::new(n_batch, 1);
    let run = InferenceRun {
        backend: &backend,
        model: &model,
        params,
        ui_friendly_threads,
        n_batch,
        cancel_flag,
        tx,
    };

    let Some(mut eval) = run.eval_prompt(active_mmproj, &image_paths, prompt, &mut batch)? else {
        return Ok(());
    };
    let (stop_reason, completion_tokens, decode_ms) =
        run.run_decode_loop(&mut eval, &mut batch, initial_close_tag)?;

    emit_stop_notice(stop_reason, tx);
    let _ = tx.send(ChatEvent::Usage {
        prompt_tokens: eval.prompt_tokens,
        completion_tokens,
        decode_ms: Some(decode_ms),
    });
    Ok(())
}

impl<'a> InferenceRun<'a> {
    fn eval_prompt(
        &self,
        active_mmproj: Option<&str>,
        image_paths: &[&str],
        prompt: String,
        batch: &mut LlamaBatch,
    ) -> Result<Option<PromptEvalResult<'a>>> {
        if let Some(mmproj_path) = active_mmproj {
            self.eval_multimodal_prompt(mmproj_path, image_paths, prompt)
                .map(Some)
        } else {
            self.eval_text_prompt(&prompt, batch)
        }
    }

    fn eval_multimodal_prompt(
        &self,
        mmproj_path: &str,
        image_paths: &[&str],
        prompt: String,
    ) -> Result<PromptEvalResult<'a>> {
        let mut mtmd_ctx = self.init_mtmd_context(mmproj_path)?;
        let mut bitmaps = Vec::with_capacity(image_paths.len());
        for img_path in image_paths {
            let bmp = MtmdBitmap::from_file(&mtmd_ctx, img_path, false)
                .map_err(|e| anyhow!("Failed to decode image {img_path}: {e}"))?;
            bitmaps.push(bmp);
        }
        let bitmap_refs: Vec<&MtmdBitmap> = bitmaps.iter().collect();
        let chunks = mtmd_ctx
            .tokenize(
                MtmdInputText {
                    text: prompt,
                    add_special: true,
                    parse_special: true,
                },
                &bitmap_refs,
            )
            .map_err(|e| anyhow!("Multimodal tokenization failed: {e}"))?;

        let ctx_size = self.params.n_ctx.max((chunks.total_tokens() as u32) + 512);
        let mut ctx = self.new_context(ctx_size)?;
        let new_pos = chunks
            .eval_chunks(&mut mtmd_ctx, &mut ctx, 0, 0, self.n_batch as i32, true)
            .map_err(|e| anyhow!("Vision prompt evaluation failed: {e}"))?;
        Ok(PromptEvalResult {
            ctx,
            pos: new_pos,
            prompt_tokens: new_pos.max(0) as u32,
            ctx_size,
        })
    }

    fn init_mtmd_context(&self, mmproj_path: &str) -> Result<MtmdContext> {
        let gpu_params = MtmdContextParams {
            use_gpu: self.params.n_gpu_layers > 0,
            n_threads: self.ui_friendly_threads,
            ..MtmdContextParams::default()
        };
        MtmdContext::init_from_file(mmproj_path, self.model, &gpu_params)
            .or_else(|_| {
                let cpu_params = MtmdContextParams {
                    use_gpu: false,
                    n_threads: self.ui_friendly_threads,
                    ..MtmdContextParams::default()
                };
                MtmdContext::init_from_file(mmproj_path, self.model, &cpu_params)
            })
            .map_err(|e| {
                anyhow!(
                    "Failed to load vision projector ({mmproj_path}): {e}. Ensure it matches this model in Settings → Models."
                )
            })
    }

    fn eval_text_prompt(
        &self,
        prompt: &str,
        batch: &mut LlamaBatch,
    ) -> Result<Option<PromptEvalResult<'a>>> {
        let tokens = self.model.vocab().tokenize(prompt.as_bytes(), true, true);
        let ctx_size = self.params.n_ctx.max((tokens.len() as u32) + 256);
        let mut ctx = self.new_context(ctx_size)?;

        let mut pos = 0i32;
        for chunk in tokens.chunks(self.n_batch) {
            if self.cancel_flag.load(Ordering::Relaxed) {
                return Ok(None);
            }
            batch.clear();
            for (idx, &tok) in chunk.iter().enumerate() {
                let is_last_overall = (pos as usize + idx + 1) == tokens.len();
                batch.add(tok, pos + idx as i32, &[0], is_last_overall)?;
            }
            ctx.decode(batch).context("Prompt decode failed")?;
            pos += chunk.len() as i32;
            yield_for_ui(self.params.n_gpu_layers);
        }
        Ok(Some(PromptEvalResult {
            ctx,
            pos,
            prompt_tokens: tokens.len() as u32,
            ctx_size,
        }))
    }

    fn new_context(&self, ctx_size: u32) -> Result<LlamaContext<'a>> {
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx_size))
            .with_n_threads(self.ui_friendly_threads)
            .with_n_threads_batch(self.ui_friendly_threads);
        self.model
            .new_context(self.backend, ctx_params)
            .context("Failed to create llama.cpp context")
    }

    fn run_decode_loop(
        &self,
        eval: &mut PromptEvalResult<'a>,
        batch: &mut LlamaBatch,
        initial_close_tag: Option<&'static str>,
    ) -> Result<(StopReason, u32, u64)> {
        let vocab = self.model.vocab();
        let mut sampler = build_sampler(self.params.temperature);
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut think_parser = match initial_close_tag {
            Some(tag) => ThinkStreamParser::starting_in_think(tag),
            None => ThinkStreamParser::new(),
        };
        let mut stop_filter = StopSequenceFilter::new();
        let mut completion_tokens = 0u32;
        let gen_start = std::time::Instant::now();
        let mut stop_reason = StopReason::MaxTokens;

        for _ in 0..self.params.max_tokens {
            if self.cancel_flag.load(Ordering::Relaxed) {
                stop_reason = StopReason::Cancelled;
                break;
            }
            let token = sampler.sample(&eval.ctx, -1);
            sampler.accept(token);
            if vocab.is_eog(token) {
                stop_reason = StopReason::Natural;
                break;
            }
            completion_tokens += 1;

            let output_bytes = vocab.token_to_piece(token, true, None);
            let mut piece = String::with_capacity(output_bytes.len());
            let _ = decoder.decode_to_string(&output_bytes, &mut piece, false);

            match stop_filter.push(&piece) {
                StopOutput::Emit(safe) if !safe.is_empty() => {
                    emit_parsed(&think_parser.push(&safe), self.tx);
                }
                StopOutput::Emit(_) => {}
                StopOutput::Stopped { emit } => {
                    if !emit.is_empty() {
                        emit_parsed(&think_parser.push(&emit), self.tx);
                    }
                    stop_reason = StopReason::Natural;
                    break;
                }
            }

            if eval.pos >= eval.ctx_size as i32 {
                stop_reason = StopReason::ContextLimit;
                break;
            }
            batch.clear();
            batch.add(token, eval.pos, &[0], true)?;
            eval.pos += 1;
            eval.ctx.decode(batch).context("Token decode failed")?;
            yield_for_ui(self.params.n_gpu_layers);
        }

        let remaining = stop_filter.finish();
        if !remaining.is_empty() {
            emit_parsed(&think_parser.push(&remaining), self.tx);
        }
        emit_parsed(&think_parser.finish(), self.tx);
        Ok((
            stop_reason,
            completion_tokens,
            gen_start.elapsed().as_millis() as u64,
        ))
    }
}

/// Keep ggml CPU/barrier usage light so Cairo UI rendering and the compositor stay responsive.
fn compute_thread_config(n_gpu_layers: u32) -> (i32, usize) {
    let max_threads = if n_gpu_layers > 0 { 2 } else { 4 };
    let ui_friendly_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .saturating_sub(2)
        .clamp(1, max_threads);
    let n_batch = if n_gpu_layers > 0 { 128 } else { 512 };
    (ui_friendly_threads, n_batch)
}

fn build_sampler(temperature: f32) -> LlamaSampler {
    if temperature <= 0.01 {
        LlamaSampler::greedy()
    } else {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(42);
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(64),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(seed),
        ])
    }
}

/// Leave a brief GPU/CPU scheduling window between token decodes so GSK rendering
/// (window resizing, scrolling, animations) is not starved by back-to-back dispatches.
fn yield_for_ui(n_gpu_layers: u32) {
    if n_gpu_layers > 0 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    } else {
        std::thread::yield_now();
    }
}

fn emit_stop_notice(stop_reason: StopReason, tx: &mpsc::UnboundedSender<ChatEvent>) {
    match stop_reason {
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

#[cfg(test)]
mod tests {
    use super::compute_thread_config;

    #[test]
    fn compute_thread_config_scales_with_gpu_offload() {
        let (gpu_threads, gpu_batch) = compute_thread_config(32);
        assert!((1..=2).contains(&gpu_threads));
        assert_eq!(gpu_batch, 128);

        let (cpu_threads, cpu_batch) = compute_thread_config(0);
        assert!((1..=4).contains(&cpu_threads));
        assert_eq!(cpu_batch, 512);
    }
}
