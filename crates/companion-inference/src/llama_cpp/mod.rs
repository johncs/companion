//! Embedded `llama.cpp` GGUF inference backend with Vulkan + CPU fallback.

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

use companion_core::runtime;
use companion_core::provider::{ChatEvent, ChatTurn, InferenceParams, ModelProvider};

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

    pub(super) fn get_or_load_model(
        &self,
        backend: &LlamaBackend,
        model_path: &str,
        n_gpu_layers: u32,
        tx: &mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<Arc<LlamaModel>> {
        {
            let mut guard = self
                .loaded
                .lock()
                .map_err(|_| anyhow!("Loaded model state lock poisoned"))?;
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

        let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext("Loading model…")));
        let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
        let model = LlamaModel::load_from_file(backend, model_path, &model_params)
            .with_context(|| format!("Failed to load GGUF model {model_path}"))?;
        let arc = Arc::new(model);

        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| anyhow!("Loaded model state lock poisoned"))?;
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
                    let _ = tx.send(ChatEvent::Status(companion_core::i18n::gettext(
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
