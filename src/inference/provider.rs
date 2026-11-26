//! Unified async `ModelProvider` abstraction for local and future remote backends.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::core::ThinkingLevel;
pub use crate::core::types::MessageRole as ChatRole;

/// A single turn passed to a [`ModelProvider`] for generation.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
    pub images: Vec<String>,
}

/// Per-request generation parameters.
#[derive(Debug, Clone)]
pub struct InferenceParams {
    pub n_ctx: u32,
    pub n_gpu_layers: u32,
    pub max_tokens: u32,
    pub temperature: f32,
    pub thinking_level: ThinkingLevel,
    pub mmproj_path: Option<String>,
}

impl Default for InferenceParams {
    fn default() -> Self {
        Self {
            n_ctx: 4096,
            n_gpu_layers: 99,
            max_tokens: 4096,
            temperature: 1.0,
            thinking_level: ThinkingLevel::Off,
            mmproj_path: None,
        }
    }
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
    /// Stream a chat completion into `tx`, stopping early if `cancel_flag` becomes true.
    fn stream_chat(
        &self,
        model_path: String,
        messages: Vec<ChatTurn>,
        params: InferenceParams,
        cancel_flag: Arc<AtomicBool>,
        tx: mpsc::UnboundedSender<ChatEvent>,
    ) -> Result<()>;
}
