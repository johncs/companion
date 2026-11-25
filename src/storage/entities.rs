//! Domain entities persisted in Companion's local SQLite database.

/// A chat session.
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model_id: Option<String>,
    pub persona_id: Option<String>,
    pub system_prompt: Option<String>,
    pub thinking_level: String,
    pub web_search_enabled: bool,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A single turn in a [`Conversation`] (user, assistant, or tool output).
#[derive(Debug, Clone)]
pub struct ConversationFolder {
    pub id: String,
    pub conversation_id: String,
    pub folder_path: String,
    pub permission_mode: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub thinking_level: Option<String>,
    pub model_name: Option<String>,
    pub execution_device: Option<String>,
    pub token_count: i64,
    pub generation_ms: Option<i64>,
    pub created_at: i64,
}

/// A reusable system prompt and inference parameter preset.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
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
    pub status: String,
    pub output: Option<String>,
    pub permission_decision: String,
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
    pub default_params_json: String,
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
    /// Return the configured default thinking level from `default_params_json`,
    /// falling back to [`crate::core::ThinkingLevel::Default`].
    pub fn default_thinking_level(&self) -> crate::core::ThinkingLevel {
        serde_json::from_str::<serde_json::Value>(&self.default_params_json)
            .ok()
            .and_then(|v| {
                v.get("thinking_level")
                    .and_then(|s| s.as_str())
                    .map(crate::core::ThinkingLevel::from_str_lossy)
            })
            .unwrap_or(crate::core::ThinkingLevel::Default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ThinkingLevel;

    #[test]
    fn parses_default_thinking_level_from_params_json() {
        let mut model = ModelRecord {
            id: "m1".to_string(),
            display_name: "Test".to_string(),
            provider_type: "llama_cpp".to_string(),
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: true,
            default_params_json: "{}".to_string(),
            avg_tokens_per_sec: None,
        };
        assert_eq!(model.default_thinking_level(), ThinkingLevel::Default);

        model.default_params_json = r#"{"thinking_level":"high","n_ctx":8192}"#.to_string();
        assert_eq!(model.default_thinking_level(), ThinkingLevel::High);

        model.default_params_json = "invalid-json".to_string();
        assert_eq!(model.default_thinking_level(), ThinkingLevel::Default);
    }
}
