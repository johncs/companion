//! Unified async `ModelProvider` abstraction for local and future remote backends.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use thiserror::Error;
use tokio::sync::mpsc;

use crate::model_params::ModelParams;
pub use crate::types::MessageRole as ChatRole;

/// Typed errors returned by [`ModelProvider`] implementations.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("Model file not found: {0}")]
    ModelNotFound(String),
    #[error("Context length exceeded: {0}")]
    ContextOverflow(String),
    #[error("Generation cancelled")]
    Cancelled,
    #[error("Inference backend error: {0}")]
    Backend(String),
}

/// A single turn passed to a [`ModelProvider`] for generation.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
    pub images: Vec<String>,
}

/// Per-request generation parameters composed around [`ModelParams`].
#[derive(Debug, Clone, Default)]
pub struct InferenceParams {
    pub model: ModelParams,
    pub mmproj_path: Option<String>,
}

impl Deref for InferenceParams {
    type Target = ModelParams;

    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

impl DerefMut for InferenceParams {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.model
    }
}

/// Request payload passed to [`ModelProvider::stream_chat`].
pub struct ChatRequest {
    pub model_path: String,
    pub messages: Vec<ChatTurn>,
    pub params: InferenceParams,
    pub cancel_flag: Arc<AtomicBool>,
    pub tx: mpsc::UnboundedSender<ChatEvent>,
}

/// Streaming events emitted by a [`ModelProvider`].
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Non-blocking status update (e.g. `"Loading model…"`).
    Status(String),
    /// Visible assistant response chunk.
    Token(String),
    /// Reasoning / `<think>` chunk.
    ReasoningToken(String),
    /// Exact token usage and decode duration reported by the inference backend.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        decode_ms: Option<u64>,
    },
    /// Generation finished cleanly.
    Done,
    /// Non-fatal informational notice (e.g. stopped at context or reply-length limit).
    Notice(String),
    /// Generation failed.
    Error(String),
}

/// Backend-agnostic interface implemented by `LlamaCppProvider` (and future cloud providers).
pub trait ModelProvider: Send + Sync {
    /// Stream a chat completion into `req.tx`, stopping early if `req.cancel_flag` becomes true.
    fn stream_chat(&self, req: ChatRequest) -> Result<(), ProviderError>;
}
