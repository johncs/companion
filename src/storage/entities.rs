//! Domain entities persisted in Companion's local SQLite database.

use crate::core::ThinkingLevel;
use crate::core::model_params::ModelParams;
use crate::core::types::{
    ExecutionDevice, FolderPermission, MessageRole, PermissionDecision, ToolCallStatus,
};

/// A chat session.
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model_id: Option<String>,
    pub persona_id: Option<String>,
    pub system_prompt: Option<String>,
    pub thinking_level: ThinkingLevel,
    pub web_search_enabled: bool,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A granted local folder or file in a [`Conversation`].
#[derive(Debug, Clone)]
pub struct ConversationFolder {
    pub id: String,
    pub conversation_id: String,
    pub folder_path: String,
    pub permission_mode: FolderPermission,
    pub created_at: i64,
}

/// A single persisted message in a [`Conversation`].
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: MessageRole,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub model_name: Option<String>,
    pub execution_device: Option<ExecutionDevice>,
    pub token_count: i64,
    pub generation_ms: Option<i64>,
    pub created_at: i64,
}

/// Parameters for inserting a new [`Message`] into SQLite.
#[derive(Debug, Clone)]
pub struct NewMessage<'a> {
    pub conversation_id: &'a str,
    pub role: MessageRole,
    pub content: &'a str,
    pub reasoning_content: Option<&'a str>,
    pub thinking_level: Option<ThinkingLevel>,
    pub model_name: Option<&'a str>,
    pub execution_device: Option<ExecutionDevice>,
    pub token_count: i64,
    pub generation_ms: Option<i64>,
}

/// A file or image attached to a chat [`Message`].
#[derive(Debug, Clone)]
pub struct AttachmentRecord {
    pub id: String,
    pub message_id: String,
    pub file_name: String,
    pub mime_type: String,
    pub file_path: String,
    pub extracted_text: Option<String>,
    pub byte_size: i64,
}

/// An agent tool invocation linked to an assistant [`Message`].
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub id: String,
    pub message_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub status: ToolCallStatus,
    pub output: Option<String>,
    pub permission_decision: PermissionDecision,
    pub created_at: i64,
}

/// A registered local or remote AI model.
#[derive(Debug, Clone)]
pub struct ModelRecord {
    pub id: String,
    pub display_name: String,
    pub provider_type: String,
    pub file_path: Option<String>,
    pub mmproj_path: Option<String>,
    pub hf_repo: Option<String>,
    pub quantization: Option<String>,
    pub supports_thinking: bool,
    pub default_params: ModelParams,
    pub avg_tokens_per_sec: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PendingDownload {
    pub repo_id: String,
    pub filename: String,
    pub mmproj_filename: Option<String>,
    pub display_name: String,
    pub total_bytes: Option<u64>,
}

impl ModelRecord {
    /// Return the configured default thinking level from `default_params`.
    pub fn default_thinking_level(&self) -> ThinkingLevel {
        self.default_params.thinking_level
    }
}
