//! Embedded `llama.cpp` GGUF inference backend with Vulkan + CPU fallback.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use tokio::sync::mpsc;

use crate::core::ThinkingLevel;
use crate::core::runtime;
use crate::inference::provider::{ChatEvent, ChatTurn, InferenceParams, ModelProvider};
use crate::inference::think_parser::{ParsedChunk, ThinkStreamParser};

static LLAMA_BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();

fn get_backend() -> Result<Arc<LlamaBackend>> {
    if let Some(b) = LLAMA_BACKEND.get() {
        return Ok(b.clone());
    }
    let backend = LlamaBackend::init().map_err(|e| anyhow!("Failed to init llama.cpp: {e}"))?;
    let arc = Arc::new(backend);
    let _ = LLAMA_BACKEND.set(arc.clone());
    Ok(arc)
}

struct LoadedModel {
    path: String,
    n_gpu_layers: u32,
    model: Arc<LlamaModel>,
}

/// Embedded `llama.cpp` provider that caches the active loaded GGUF in memory.
/// Serializes inference turns with an async lock so multiple chats can share one loaded model safely.
#[derive(Clone, Default)]
pub struct LlamaCppProvider {
    loaded: Arc<Mutex<Option<LoadedModel>>>,
    inference_lock: Arc<tokio::sync::Mutex<()>>,
}

impl LlamaCppProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Unload any currently cached model weights from RAM/VRAM.
    pub fn unload(&self) {
        if let Ok(mut guard) = self.loaded.lock() {
            *guard = None;
        }
    }

    fn get_or_load_model(
        &self,
        backend: &LlamaBackend,
        model_path: &str,
        n_gpu_layers: u32,
        tx: &mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<Arc<LlamaModel>> {
        {
            let mut guard = self.loaded.lock().expect("loaded model mutex poisoned");
            if let Some(loaded) = guard.as_ref()
                && loaded.path == model_path
                && loaded.n_gpu_layers == n_gpu_layers
            {
                return Ok(loaded.model.clone());
            }
            // Drop the previous model before loading a new one or changing GPU offload mode.
            *guard = None;
        }

        if !Path::new(model_path).exists() {
            return Err(anyhow!("Model file not found: {model_path}"));
        }

        let _ = tx.send(ChatEvent::Status("Loading model…".to_string()));
        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .with_context(|| format!("Failed to load GGUF model {model_path}"))?;
        let arc = Arc::new(model);

        let mut guard = self.loaded.lock().expect("loaded model mutex poisoned");
        *guard = Some(LoadedModel {
            path: model_path.to_string(),
            n_gpu_layers,
            model: arc.clone(),
        });

        Ok(arc)
    }
}

impl ModelProvider for LlamaCppProvider {
    fn stream_chat(
        &self,
        model_path: String,
        messages: Vec<ChatTurn>,
        params: InferenceParams,
        cancel_flag: Arc<AtomicBool>,
        tx: mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<()> {
        let provider = self.clone();

        runtime().spawn(async move {
            let _turn_guard = match provider.inference_lock.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    let _ = tx.send(ChatEvent::Status(crate::i18n::gettext(
                        "Waiting for AI model…",
                    )));
                    provider.inference_lock.lock().await
                }
            };

            if cancel_flag.load(Ordering::Relaxed) {
                let _ = tx.send(ChatEvent::Done);
                return;
            }

            let provider_blocking = provider.clone();
            let tx_blocking = tx.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(err) = run_inference_blocking(
                    &provider_blocking,
                    &model_path,
                    &messages,
                    &params,
                    &cancel_flag,
                    &tx_blocking,
                ) {
                    let _ = tx_blocking.send(ChatEvent::Error(err.to_string()));
                }
                let _ = tx_blocking.send(ChatEvent::Done);
            })
            .await;
        });

        Ok(())
    }
}

fn run_inference_blocking(
    provider: &LlamaCppProvider,
    model_path: &str,
    messages: &[ChatTurn],
    params: &InferenceParams,
    cancel_flag: &AtomicBool,
    tx: &mpsc::UnboundedSender<ChatEvent>,
) -> Result<()> {
    let backend = get_backend()?;
    let model = provider.get_or_load_model(&backend, model_path, params.n_gpu_layers, tx)?;
    let _ = tx.send(ChatEvent::Status("Generating…".to_string()));

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

    // Keep ggml CPU/barrier usage light so Cairo UI rendering and the compositor stay responsive.
    let max_threads = if params.n_gpu_layers > 0 { 2 } else { 4 };
    let ui_friendly_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .saturating_sub(2)
        .clamp(1, max_threads);
    let n_batch = if params.n_gpu_layers > 0 { 128usize } else { 512usize };
    let mut batch = LlamaBatch::new(n_batch, 1);

    let (mut ctx, mut pos, prompt_token_count, ctx_size) = if let Some(mmproj_path) = active_mmproj {
        let mtmd_params_gpu = MtmdContextParams {
            use_gpu: params.n_gpu_layers > 0,
            n_threads: ui_friendly_threads,
            ..MtmdContextParams::default()
        };
        let mut mtmd_ctx = MtmdContext::init_from_file(mmproj_path, &model, &mtmd_params_gpu)
            .or_else(|_| {
                let mtmd_params_cpu = MtmdContextParams {
                    use_gpu: false,
                    n_threads: ui_friendly_threads,
                    ..MtmdContextParams::default()
                };
                MtmdContext::init_from_file(mmproj_path, &model, &mtmd_params_cpu)
            })
            .map_err(|e| {
                anyhow!(
                    "Failed to load vision projector ({mmproj_path}): {e}. Ensure it matches this model in Settings → Models."
                )
            })?;

        let mut bitmaps = Vec::with_capacity(image_paths.len());
        for img_path in &image_paths {
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

        let estimated_tokens = (chunks.total_tokens() as u32) + 512;
        let ctx_size = params.n_ctx.max(estimated_tokens);
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx_size))
            .with_n_threads(ui_friendly_threads)
            .with_n_threads_batch(ui_friendly_threads);
        let mut ctx = model
            .new_context(&backend, ctx_params)
            .context("Failed to create llama.cpp context")?;

        let new_pos = chunks
            .eval_chunks(&mut mtmd_ctx, &mut ctx, 0, 0, n_batch as i32, true)
            .map_err(|e| anyhow!("Vision prompt evaluation failed: {e}"))?;
        (ctx, new_pos, new_pos.max(0) as u32, ctx_size)
    } else {
        let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
        let ctx_size = params.n_ctx.max((tokens.len() as u32) + 256);
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx_size))
            .with_n_threads(ui_friendly_threads)
            .with_n_threads_batch(ui_friendly_threads);
        let mut ctx = model
            .new_context(&backend, ctx_params)
            .context("Failed to create llama.cpp context")?;

        let mut pos = 0i32;
        for chunk in tokens.chunks(n_batch) {
            if cancel_flag.load(Ordering::Relaxed) {
                return Ok(());
            }
            batch.clear();
            for (idx, &tok) in chunk.iter().enumerate() {
                let is_last_overall = (pos as usize + idx + 1) == tokens.len();
                batch.add(tok, pos + idx as i32, &[0], is_last_overall)?;
            }
            ctx.decode(&mut batch).context("Prompt decode failed")?;
            pos += chunk.len() as i32;
            if params.n_gpu_layers > 0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            } else {
                std::thread::yield_now();
            }
        }
        (ctx, pos, tokens.len() as u32, ctx_size)
    };

    let mut sampler = if params.temperature <= 0.01 {
        LlamaSampler::greedy()
    } else {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(42);
        LlamaSampler::chain_simple([
            LlamaSampler::top_k(64),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::temp(params.temperature),
            LlamaSampler::dist(seed),
        ])
    };

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut think_parser = match initial_close_tag {
        Some(tag) => ThinkStreamParser::starting_in_think(tag),
        None => ThinkStreamParser::new(),
    };
    let mut raw_tail = String::new();
    let mut completion_tokens = 0u32;
    let gen_start = std::time::Instant::now();
    const STOP_SEQUENCES: [&str; 5] = [
        "<|im_end|>",
        "<end_of_turn>",
        "<turn|>",
        "<|eot_id|>",
        "<|end|>",
    ];

    enum StopReason {
        Natural,
        Cancelled,
        ContextLimit,
        MaxTokens,
    }
    let mut stop_reason = StopReason::MaxTokens;

    for _ in 0..params.max_tokens {
        if cancel_flag.load(Ordering::Relaxed) {
            stop_reason = StopReason::Cancelled;
            break;
        }

        let token = sampler.sample(&ctx, -1);
        sampler.accept(token);

        if vocab.is_eog(token) {
            stop_reason = StopReason::Natural;
            break;
        }
        completion_tokens += 1;

        let output_bytes = vocab.token_to_piece(token, true, None);
        let mut piece = String::with_capacity(output_bytes.len());
        let _ = decoder.decode_to_string(&output_bytes, &mut piece, false);

        if !piece.is_empty() {
            raw_tail.push_str(&piece);
            if let Some(stop_idx) = STOP_SEQUENCES
                .iter()
                .filter_map(|s| raw_tail.find(s))
                .min()
            {
                let before_stop = &raw_tail[..stop_idx];
                if !before_stop.is_empty() {
                    emit_parsed(&think_parser.push(before_stop), tx);
                }
                raw_tail.clear();
                stop_reason = StopReason::Natural;
                break;
            }

            // Hold back only enough trailing chars to detect a split stop sequence.
            let max_hold = STOP_SEQUENCES.iter().map(|s| s.len()).max().unwrap_or(0);
            if raw_tail.len() > max_hold {
                let mut flush_end = raw_tail.len() - max_hold;
                while flush_end > 0 && !raw_tail.is_char_boundary(flush_end) {
                    flush_end -= 1;
                }
                if flush_end > 0 {
                    let safe: String = raw_tail.drain(..flush_end).collect();
                    emit_parsed(&think_parser.push(&safe), tx);
                }
            }
        }

        // Stop cleanly if the KV cache has reached `ctx_size` instead of failing in `ctx.decode`.
        if pos >= ctx_size as i32 {
            stop_reason = StopReason::ContextLimit;
            break;
        }

        batch.clear();
        batch.add(token, pos, &[0], true)?;
        pos += 1;
        ctx.decode(&mut batch).context("Token decode failed")?;

        // Leave a brief GPU/CPU scheduling window between token decodes so GSK rendering
        // (window resizing, scrolling, animations) is not starved by back-to-back dispatches.
        if params.n_gpu_layers > 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        } else {
            std::thread::yield_now();
        }
    }

    if !raw_tail.is_empty() {
        let trimmed = raw_tail.trim_end();
        if !trimmed.is_empty() {
            emit_parsed(&think_parser.push(trimmed), tx);
        }
    }
    emit_parsed(&think_parser.finish(), tx);
    match stop_reason {
        StopReason::ContextLimit => {
            let _ = tx.send(ChatEvent::Notice(crate::i18n::gettext(
                "Response stopped: reached the context window limit",
            )));
        }
        StopReason::MaxTokens => {
            let _ = tx.send(ChatEvent::Notice(crate::i18n::gettext(
                "Response stopped: reached the maximum reply length",
            )));
        }
        StopReason::Natural | StopReason::Cancelled => {}
    }
    let decode_ms = Some(gen_start.elapsed().as_millis() as u64);
    let _ = tx.send(ChatEvent::Usage {
        prompt_tokens: prompt_token_count,
        completion_tokens,
        decode_ms,
    });
    Ok(())
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

fn format_chat_prompt(
    model: &LlamaModel,
    messages: &[ChatTurn],
    thinking_level: ThinkingLevel,
    has_vision: bool,
) -> Result<(String, Option<&'static str>)> {
    let template = model.chat_template(None).ok();
    let template_src = template
        .as_ref()
        .and_then(|t| t.to_str().ok())
        .unwrap_or("");
    let is_gemma_think = template_src.contains("<|think|>")
        || template_src.contains("<|channel>thought")
        || template_src.contains("<|turn>");
    let is_qwen_think = !is_gemma_think
        && (template_src.contains("enable_thinking") || template_src.contains("/no_think"));

    let media_marker = mtmd_default_marker();
    let mut prepared: Vec<(String, String)> = messages
        .iter()
        .map(|m| {
            let mut content = m.content.clone();
            if has_vision && !m.images.is_empty() {
                let markers = vec![media_marker; m.images.len()].join("\n");
                content = if content.is_empty() {
                    markers
                } else {
                    format!("{markers}\n{content}")
                };
            } else if !m.images.is_empty() {
                let notes = m
                    .images
                    .iter()
                    .map(|p| format!("[Attached image: {p}]"))
                    .collect::<Vec<_>>()
                    .join("\n");
                content = if content.is_empty() {
                    notes
                } else {
                    format!("{notes}\n{content}")
                };
            }
            (m.role.as_str().to_string(), content)
        })
        .collect();

    let is_explicit_thinking = !matches!(thinking_level, ThinkingLevel::Default | ThinkingLevel::Off);

    if thinking_level == ThinkingLevel::Off {
        if is_qwen_think
            && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
        {
            content.push_str(" /no_think");
        }
    } else if is_explicit_thinking {
        let effort = match thinking_level {
            ThinkingLevel::Low => "low (brief)",
            ThinkingLevel::Medium => "medium (balanced)",
            ThinkingLevel::High => "high (thorough)",
            ThinkingLevel::Max => "maximum (deep, exhaustive)",
            ThinkingLevel::Default | ThinkingLevel::Off => "",
        };
        let directive = if is_gemma_think {
            format!(
                "<|think|>\nReasoning effort: {effort}. Think step by step before giving your final answer."
            )
        } else {
            format!(
                "Reasoning effort: {effort}. Show your step-by-step thinking inside <think>...</think> tags before your final answer."
            )
        };

        if let Some((_, sys)) = prepared.iter_mut().find(|(r, _)| r == "system") {
            if is_gemma_think && !sys.starts_with("<|think|>") {
                *sys = format!("{directive}\n\n{sys}");
            } else if !is_gemma_think {
                sys.push_str("\n\n");
                sys.push_str(&directive);
            }
        } else {
            prepared.insert(0, ("system".to_string(), directive));
        }

        if is_qwen_think
            && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
        {
            content.push_str(" /think");
        }
    }

    // Gemma 4 uses `<|turn>system / user / model` and `<|think|>` + `<|channel>thought`.
    if is_gemma_think && template_src.contains("<|turn>") {
        let mut out = String::new();
        for (role, content) in &prepared {
            let gemma_role = if role == "assistant" { "model" } else { role.as_str() };
            out.push_str(&format!("<|turn>{gemma_role}\n{content}<turn|>\n"));
        }
        out.push_str("<|turn>model\n");
        let initial_close = if is_explicit_thinking {
            out.push_str("<|channel>thought\n");
            Some("<channel|>")
        } else {
            None
        };
        tracing::info!(
            "format_chat_prompt (gemma4): thinking_level={:?}, initial_close={:?}, tail={:?}",
            thinking_level,
            initial_close,
            &out[out.len().saturating_sub(80)..]
        );
        return Ok((out, initial_close));
    }

    let mut chat_msgs = Vec::with_capacity(prepared.len());
    for (role, content) in &prepared {
        chat_msgs.push(
            LlamaChatMessage::new(role.clone(), content.clone())
                .map_err(|e| anyhow!("Invalid chat message: {e}"))?,
        );
    }

    if let Some(template) = template.as_ref()
        && let Ok(mut formatted) = model.apply_chat_template(template, &chat_msgs, true)
    {
        let initial_close = if is_explicit_thinking {
            if is_gemma_think && formatted.ends_with("<|turn>model\n") {
                formatted.push_str("<|channel>thought\n");
                Some("<channel|>")
            } else if is_qwen_think {
                if let Some(stripped) = formatted.strip_suffix("<think>\n\n</think>\n\n") {
                    formatted = stripped.to_string();
                }
                if formatted.ends_with("<|im_start|>assistant\n") {
                    formatted.push_str("<think>\n");
                    Some("</think>")
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        tracing::info!(
            "format_chat_prompt (template): thinking_level={:?}, initial_close={:?}, tail={:?}",
            thinking_level,
            initial_close,
            &formatted[formatted.len().saturating_sub(80)..]
        );
        return Ok((formatted, initial_close));
    }

    // ChatML fallback if the GGUF lacks a supported built-in template.
    let mut fallback = String::new();
    for (role, content) in &prepared {
        fallback.push_str(&format!("<|im_start|>{role}\n{content}<|im_end|>\n"));
    }
    fallback.push_str("<|im_start|>assistant\n");
    let initial_close = if is_explicit_thinking {
        fallback.push_str("<think>\n");
        Some("</think>")
    } else {
        None
    };
    tracing::info!(
        "format_chat_prompt (fallback): thinking_level={:?}, initial_close={:?}, tail={:?}",
        thinking_level,
        initial_close,
        &fallback[fallback.len().saturating_sub(80)..]
    );
    Ok((fallback, initial_close))
}

/// Pin system turns and the latest non-system turn, then keep as many recent turns as fit
/// inside `n_ctx` minus a reply reserve so long chats continue seamlessly without overflowing.
fn trim_turns_to_budget(
    turns: &[ChatTurn],
    n_ctx: u32,
    count_tokens: impl Fn(&str) -> usize,
) -> Vec<ChatTurn> {
    const TURN_FRAMING_OVERHEAD: usize = 8;
    let ctx_budget = n_ctx.max(512);
    let reply_reserve = (ctx_budget / 4).clamp(256, 2048);
    let max_prompt_tokens = (ctx_budget - reply_reserve) as usize;

    let mut system_turns = Vec::new();
    let mut dialog_turns = Vec::new();
    let mut used_tokens = 0usize;

    for turn in turns {
        if matches!(turn.role, crate::inference::provider::ChatRole::System) {
            used_tokens += count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
            system_turns.push(turn.clone());
        } else {
            dialog_turns.push(turn);
        }
    }

    let Some((&latest, earlier)) = dialog_turns.split_last() else {
        return system_turns;
    };

    used_tokens += count_tokens(&latest.content) + TURN_FRAMING_OVERHEAD;
    let mut kept_rev = vec![latest.clone()];

    for &turn in earlier.iter().rev() {
        let cost = count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
        if used_tokens + cost > max_prompt_tokens {
            break;
        }
        used_tokens += cost;
        kept_rev.push(turn.clone());
    }

    kept_rev.reverse();
    system_turns.extend(kept_rev);
    system_turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::provider::ChatRole;

    fn turn(role: ChatRole, content: &str) -> ChatTurn {
        ChatTurn {
            role,
            content: content.to_string(),
            images: Vec::new(),
        }
    }

    #[test]
    fn trims_oldest_turns_while_preserving_system_and_latest_user() {
        let turns = vec![
            turn(ChatRole::System, &"s".repeat(40)),
            turn(ChatRole::User, &"1".repeat(200)),
            turn(ChatRole::Assistant, &"2".repeat(200)),
            turn(ChatRole::User, &"3".repeat(100)),
            turn(ChatRole::Assistant, &"4".repeat(100)),
            turn(ChatRole::User, &"5".repeat(80)),
        ];
        // n_ctx=512 -> reply_reserve=256 -> max_prompt_tokens=256.
        // system (48) + latest "5" (88) = 136; adding "4" (108) = 244 <= 256; adding "3" (108) = 352 > 256.
        let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
        assert_eq!(kept.len(), 3);
        assert!(matches!(kept[0].role, ChatRole::System));
        assert!(kept[1].content.starts_with('4'));
        assert!(kept[2].content.starts_with('5'));
    }

    #[test]
    fn keeps_latest_turn_even_when_over_budget() {
        let turns = vec![
            turn(ChatRole::System, "sys"),
            turn(ChatRole::User, "old"),
            turn(ChatRole::User, &"x".repeat(2000)),
        ];
        let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].content, "sys");
        assert_eq!(kept[1].content.len(), 2000);
    }
}
