//! Strongly-typed domain enums shared across `core`, `storage`, `agent`, `inference`, and `ui`.

use serde::{Deserialize, Serialize};

use crate::i18n::gettext;

/// Role of a message in a conversation or inference prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    #[default]
    User,
    Assistant,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Self::System,
            "assistant" | "model" => Self::Assistant,
            _ => Self::User,
        }
    }
}

/// Access mode granted to a local folder or file in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderPermission {
    #[default]
    Read,
    ReadWrite,
}

impl FolderPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "read_write" => Self::ReadWrite,
            _ => Self::Read,
        }
    }

    pub fn allows_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Execution outcome of an agent tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    #[default]
    Ok,
    Denied,
    Error,
}

impl ToolCallStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "denied" => Self::Denied,
            "error" => Self::Error,
            _ => Self::Ok,
        }
    }
}

/// User permission decision recorded for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    #[default]
    Deny,
    AllowOnce,
    AllowChat,
}

impl PermissionDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowOnce => "allow_once",
            Self::AllowChat => "allow_chat",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow_once" => Self::AllowOnce,
            "allow_chat" => Self::AllowChat,
            _ => Self::Deny,
        }
    }
}

/// Hardware execution target recorded on generated assistant messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDevice {
    #[default]
    Cpu,
    Gpu,
}

impl ExecutionDevice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "gpu" => Self::Gpu,
            _ => Self::Cpu,
        }
    }
}

/// Configured privacy-first web search backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchEngine {
    #[default]
    DuckDuckGo,
    SearXNG,
}

impl SearchEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::SearXNG => "searxng",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "searxng" => Self::SearXNG,
            _ => Self::DuckDuckGo,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::DuckDuckGo => gettext("DuckDuckGo"),
            Self::SearXNG => gettext("SearXNG"),
        }
    }
}

/// A file or image attached to a chat message.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_enums_round_trip_and_handle_unknowns() {
        for r in [MessageRole::System, MessageRole::User, MessageRole::Assistant] {
            assert_eq!(MessageRole::from_str_lossy(r.as_str()), r);
        }
        assert_eq!(MessageRole::from_str_lossy("model"), MessageRole::Assistant);

        for p in [FolderPermission::Read, FolderPermission::ReadWrite] {
            assert_eq!(FolderPermission::from_str_lossy(p.as_str()), p);
        }
        assert!(FolderPermission::ReadWrite.allows_write());
        assert!(!FolderPermission::Read.allows_write());

        for s in [ToolCallStatus::Ok, ToolCallStatus::Denied, ToolCallStatus::Error] {
            assert_eq!(ToolCallStatus::from_str_lossy(s.as_str()), s);
        }

        for d in [
            PermissionDecision::Deny,
            PermissionDecision::AllowOnce,
            PermissionDecision::AllowChat,
        ] {
            assert_eq!(PermissionDecision::from_str_lossy(d.as_str()), d);
        }

        for dev in [ExecutionDevice::Cpu, ExecutionDevice::Gpu] {
            assert_eq!(ExecutionDevice::from_str_lossy(dev.as_str()), dev);
        }

        for eng in [SearchEngine::DuckDuckGo, SearchEngine::SearXNG] {
            assert_eq!(SearchEngine::from_str_lossy(eng.as_str()), eng);
        }

        // Case-insensitivity, whitespace trimming, and unknown fallbacks.
        assert_eq!(MessageRole::from_str_lossy("  ASSISTANT "), MessageRole::Assistant);
        assert_eq!(MessageRole::from_str_lossy("unknown"), MessageRole::User);
        assert_eq!(
            FolderPermission::from_str_lossy(" READ_WRITE "),
            FolderPermission::ReadWrite
        );
        assert_eq!(
            FolderPermission::from_str_lossy("invalid"),
            FolderPermission::Read
        );
        assert_eq!(ToolCallStatus::from_str_lossy(" DENIED "), ToolCallStatus::Denied);
        assert_eq!(ToolCallStatus::from_str_lossy("weird"), ToolCallStatus::Ok);
        assert_eq!(
            PermissionDecision::from_str_lossy(" Allow_Chat "),
            PermissionDecision::AllowChat
        );
        assert_eq!(
            PermissionDecision::from_str_lossy("unknown"),
            PermissionDecision::Deny
        );
        assert_eq!(ExecutionDevice::from_str_lossy(" GPU "), ExecutionDevice::Gpu);
        assert_eq!(ExecutionDevice::from_str_lossy("tpu"), ExecutionDevice::Cpu);
        assert_eq!(SearchEngine::from_str_lossy(" SearXNG "), SearchEngine::SearXNG);
        assert_eq!(SearchEngine::from_str_lossy("other"), SearchEngine::DuckDuckGo);
    }
}
