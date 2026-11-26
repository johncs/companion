//! Embedded `llama.cpp` GGUF inference backend with Vulkan + CPU fallback.

mod decode;
mod prompt;
mod runner;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use tokio::sync::mpsc;

use companion_core::provider::{ChatEvent, ChatRequest, ModelProvider, ProviderError};

use runner::run_inference_blocking;

static LLAMA_BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();
static LLAMA_BACKEND_INIT: Mutex<()> = Mutex::new(());

pub(super) fn get_backend() -> Result<Arc<LlamaBackend>> {
    if let Some(b) = LLAMA_BACKEND.get() {
        return Ok(b.clone());
    }
    let _init_guard = LLAMA_BACKEND_INIT
        .lock()
        .map_err(|_| anyhow!("llama.cpp backend init lock poisoned"))?;
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
#[derive(Clone)]
pub struct LlamaCppProvider {
    handle: tokio::runtime::Handle,
    loaded: Arc<Mutex<Option<LoadedModel>>>,
    inference_lock: Arc<tokio::sync::Mutex<()>>,
}

impl LlamaCppProvider {
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self {
            handle,
            loaded: Arc::new(Mutex::new(None)),
            inference_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Unload any currently cached model weights from RAM/VRAM.
    pub fn unload(&self) {
        if let Ok(mut guard) = self.loaded.lock() {
            *guard = None;
        }
    }

    pub(super) fn get_or_load_model(
        &self,
        backend: &LlamaBackend,
        model_path: &str,
        n_gpu_layers: u32,
        tx: &mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<(Arc<LlamaModel>, u32)> {
        {
            let mut guard = self
                .loaded
                .lock()
                .map_err(|_| anyhow!("Loaded model state lock poisoned"))?;
            if let Some(loaded) = guard.as_ref()
                && loaded.path == model_path
                && (loaded.n_gpu_layers == n_gpu_layers
                    || (n_gpu_layers > 0 && loaded.n_gpu_layers == 0))
            {
                return Ok((loaded.model.clone(), loaded.n_gpu_layers));
            }
            // Drop the previous model before loading a new one or changing GPU offload mode.
            *guard = None;
        }

        if !Path::new(model_path).exists() {
            return Err(anyhow!("Model file not found: {model_path}"));
        }

        let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
            "Loading model…",
        )));
        let (model, effective_gpu_layers) =
            load_model_with_fallback(backend, model_path, n_gpu_layers, tx)?;
        let arc = Arc::new(model);

        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| anyhow!("Loaded model state lock poisoned"))?;
        *guard = Some(LoadedModel {
            path: model_path.to_string(),
            n_gpu_layers: effective_gpu_layers,
            model: arc.clone(),
        });

        Ok((arc, effective_gpu_layers))
    }
}

fn load_model_with_fallback(
    backend: &LlamaBackend,
    model_path: &str,
    n_gpu_layers: u32,
    tx: &mpsc::UnboundedSender<ChatEvent>,
) -> Result<(LlamaModel, u32)> {
    if n_gpu_layers > 0 {
        let gpu_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
        match LlamaModel::load_from_file(backend, model_path, &gpu_params) {
            Ok(m) => return Ok((m, n_gpu_layers)),
            Err(gpu_err) => {
                tracing::warn!(
                    "GPU model load failed ({gpu_err}); falling back to CPU for {model_path}"
                );
                let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
                    "GPU model load failed; falling back to CPU…",
                )));
            }
        }
    }
    let cpu_params = LlamaModelParams::default().with_n_gpu_layers(0);
    let model = LlamaModel::load_from_file(backend, model_path, &cpu_params)
        .with_context(|| format!("Failed to load GGUF model {model_path}"))?;
    Ok((model, 0))
}

async fn acquire_turn_guard<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    cancel_flag: &AtomicBool,
    tx: &mpsc::UnboundedSender<ChatEvent>,
) -> Option<tokio::sync::MutexGuard<'a, ()>> {
    let guard = match lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
                "Waiting for AI model…",
            )));
            let Some(guard) = acquire_inference_lock_cancellable(lock, cancel_flag).await else {
                let _ = tx.send(ChatEvent::Done);
                return None;
            };
            guard
        }
    };
    if cancel_flag.load(Ordering::Relaxed) {
        let _ = tx.send(ChatEvent::Done);
        return None;
    }
    Some(guard)
}

async fn acquire_inference_lock_cancellable<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    cancel_flag: &AtomicBool,
) -> Option<tokio::sync::MutexGuard<'a, ()>> {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let fut = lock.lock();
    tokio::pin!(fut);
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            return None;
        }
        tokio::select! {
            guard = &mut fut => return Some(guard),
            _ = tick.tick() => {}
        }
    }
}

impl ModelProvider for LlamaCppProvider {
    fn stream_chat(&self, req: ChatRequest) -> Result<(), ProviderError> {
        let ChatRequest {
            model_path,
            messages,
            params,
            cancel_flag,
            tx,
        } = req;
        let provider = self.clone();

        self.handle.spawn(async move {
            let Some(_turn_guard) =
                acquire_turn_guard(&provider.inference_lock, &cancel_flag, &tx).await
            else {
                return;
            };

            let provider_blocking = provider.clone();
            let tx_blocking = tx.clone();
            let join_res = tokio::task::spawn_blocking(move || {
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

            if let Err(join_err) = join_res {
                tracing::error!("Inference worker task panicked/cancelled: {join_err}");
                let _ = tx.send(ChatEvent::Error(format!(
                    "Inference worker failed: {join_err}"
                )));
                let _ = tx.send(ChatEvent::Done);
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_core::provider::InferenceParams;

    #[tokio::test]
    async fn missing_model_and_pre_cancelled_and_unload_behave_cleanly() {
        let provider = LlamaCppProvider::new(tokio::runtime::Handle::current());
        provider.unload();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(true));
        provider
            .stream_chat(ChatRequest {
                model_path: "/nonexistent/model.gguf".to_string(),
                messages: Vec::new(),
                params: InferenceParams::default(),
                cancel_flag: cancel,
                tx,
            })
            .unwrap();
        let first = rx.recv().await.unwrap();
        assert!(matches!(first, ChatEvent::Done));

        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let cancel2 = Arc::new(AtomicBool::new(false));
        provider
            .stream_chat(ChatRequest {
                model_path: "/nonexistent/model.gguf".to_string(),
                messages: Vec::new(),
                params: InferenceParams::default(),
                cancel_flag: cancel2,
                tx: tx2,
            })
            .unwrap();
        let mut saw_error = false;
        let mut saw_done = false;
        while let Some(ev) = rx2.recv().await {
            match ev {
                ChatEvent::Error(e) => {
                    assert!(e.contains("Model file not found"));
                    saw_error = true;
                }
                ChatEvent::Done => {
                    saw_done = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_error && saw_done);
    }
}
