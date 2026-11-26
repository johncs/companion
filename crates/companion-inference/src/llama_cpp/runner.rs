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

use companion_core::provider::{ChatEvent, ChatTurn, InferenceParams};

use super::decode::emit_stop_notice;
use super::prompt::{format_chat_prompt, trim_turns_to_budget};
use super::{LlamaCppProvider, get_backend};

const MAX_AUTO_CTX_TOKENS: u32 = 32_768;

pub(super) enum StopReason {
    Natural,
    Cancelled,
    ContextLimit,
    MaxTokens,
}

pub(super) struct PromptEvalResult<'a> {
    pub(super) ctx: LlamaContext<'a>,
    pub(super) pos: i32,
    pub(super) prompt_tokens: u32,
    pub(super) ctx_size: u32,
}

pub(super) struct InferenceRun<'a> {
    pub(super) backend: &'a LlamaBackend,
    pub(super) model: &'a LlamaModel,
    pub(super) params: &'a InferenceParams,
    pub(super) ui_friendly_threads: i32,
    pub(super) n_batch: usize,
    pub(super) cancel_flag: &'a AtomicBool,
    pub(super) tx: &'a mpsc::UnboundedSender<ChatEvent>,
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
    let (model, effective_gpu_layers) =
        provider.get_or_load_model(&backend, model_path, params.n_gpu_layers, tx)?;
    let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
        "Generating…",
    )));

    if cancel_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    let PreparedPrompt {
        image_paths,
        active_mmproj,
        prompt,
        initial_close_tag,
    } = prepare_inference_prompt(&model, messages, params)?;

    let (ui_friendly_threads, n_batch) = compute_thread_config(effective_gpu_layers);
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
    let image_refs: Vec<&str> = image_paths.iter().map(String::as_str).collect();
    let Some(mut eval) =
        run.eval_prompt(active_mmproj.as_deref(), &image_refs, prompt, &mut batch)?
    else {
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

struct PreparedPrompt {
    image_paths: Vec<String>,
    active_mmproj: Option<String>,
    prompt: String,
    initial_close_tag: Option<&'static str>,
}

fn prepare_inference_prompt(
    model: &LlamaModel,
    messages: &[ChatTurn],
    params: &InferenceParams,
) -> Result<PreparedPrompt> {
    let vocab = model.vocab();
    let trimmed_messages = trim_turns_to_budget(messages, params.n_ctx, |s| {
        vocab.tokenize(s.as_bytes(), false, true).len()
    });
    // Collect image paths strictly from trimmed_messages so image count matches <__media__> markers.
    let image_paths: Vec<String> = trimmed_messages
        .iter()
        .flat_map(|m| m.images.iter().cloned())
        .collect();
    let active_mmproj = params
        .mmproj_path
        .as_deref()
        .filter(|p| !image_paths.is_empty() && Path::new(p).exists())
        .map(str::to_string);

    let (prompt, initial_close_tag) = format_chat_prompt(
        model,
        &trimmed_messages,
        params.thinking_level,
        active_mmproj.is_some(),
    )?;
    Ok(PreparedPrompt {
        image_paths,
        active_mmproj,
        prompt,
        initial_close_tag,
    })
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
        } else {
            self.eval_text_prompt(&prompt, batch)
        }
    }

    fn eval_multimodal_prompt(
        &self,
        mmproj_path: &str,
        image_paths: &[&str],
        prompt: String,
    ) -> Result<Option<PromptEvalResult<'a>>> {
        if self.cancel_flag.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let mut mtmd_ctx = self.init_mtmd_context(mmproj_path)?;
        let mut bitmaps = Vec::with_capacity(image_paths.len());
        for img_path in image_paths {
            if self.cancel_flag.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let bmp = MtmdBitmap::from_file(&mtmd_ctx, img_path, false)
                .map_err(|e| anyhow!("Failed to decode image {img_path}: {e}"))?;
            bitmaps.push(bmp);
        }
        if self.cancel_flag.load(Ordering::Relaxed) {
            return Ok(None);
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

        let total = u32::try_from(chunks.total_tokens()).unwrap_or(u32::MAX);
        let needed = total.saturating_add(512);
        let ctx_cap = MAX_AUTO_CTX_TOKENS.max(self.params.n_ctx);
        let ctx_size = self.params.n_ctx.max(needed).min(ctx_cap);
        let mut ctx = self.new_context(ctx_size)?;
        let n_batch_i32 = i32::try_from(self.n_batch).unwrap_or(512);
        let new_pos = chunks
            .eval_chunks(&mut mtmd_ctx, &mut ctx, 0, 0, n_batch_i32, true)
            .map_err(|e| anyhow!("Vision prompt evaluation failed: {e}"))?;
        Ok(Some(PromptEvalResult {
            ctx,
            pos: new_pos,
            prompt_tokens: u32::try_from(new_pos.max(0)).unwrap_or(0),
            ctx_size,
        }))
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
        if tokens.is_empty() {
            return Err(anyhow!("Prompt tokenized to 0 tokens"));
        }
        let tok_len = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
        let needed = tok_len.saturating_add(256);
        let ctx_cap = MAX_AUTO_CTX_TOKENS.max(self.params.n_ctx);
        let ctx_size = self.params.n_ctx.max(needed).min(ctx_cap);
        let mut ctx = self.new_context(ctx_size)?;

        let mut pos = 0i32;
        for chunk in tokens.chunks(self.n_batch) {
            if self.cancel_flag.load(Ordering::Relaxed) {
                return Ok(None);
            }
            batch.clear();
            for (idx, &tok) in chunk.iter().enumerate() {
                let is_last_overall = (pos as usize).saturating_add(idx + 1) == tokens.len();
                let tok_pos = pos.saturating_add(i32::try_from(idx).unwrap_or(0));
                batch.add(tok, tok_pos, &[0], is_last_overall)?;
            }
            ctx.decode(batch).context("Prompt decode failed")?;
            pos = pos.saturating_add(i32::try_from(chunk.len()).unwrap_or(0));
            std::thread::yield_now();
        }
        Ok(Some(PromptEvalResult {
            ctx,
            pos,
            prompt_tokens: tok_len,
            ctx_size,
        }))
    }

    fn new_context(&self, ctx_size: u32) -> Result<LlamaContext<'a>> {
        let n_batch_u32 = u32::try_from(self.n_batch).unwrap_or(512);
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx_size))
            .with_n_batch(n_batch_u32)
            .with_n_threads(self.ui_friendly_threads)
            .with_n_threads_batch(self.ui_friendly_threads);
        self.model
            .new_context(self.backend, ctx_params)
            .context("Failed to create llama.cpp context")
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

pub(super) fn build_sampler(temperature: f32) -> LlamaSampler {
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
