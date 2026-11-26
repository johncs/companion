//! Strongly-typed domain enums shared across `core`, `storage`, `agent`, `inference`, and `ui`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::i18n::gettext;

macro_rules! impl_enum_str_traits {
    ($ty:ty) => {
        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

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
        s.parse().unwrap_or(Self::User)
    }
}

impl FromStr for MessageRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" | "model" => Ok(Self::Assistant),
            other => Err(format!("Unknown message role: {other}")),
        }
    }
}

impl_enum_str_traits!(MessageRole);

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
        s.parse().unwrap_or(Self::Read)
    }

    pub fn allows_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

impl FromStr for FolderPermission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Ok(Self::Read),
            "read_write" => Ok(Self::ReadWrite),
            other => Err(format!("Unknown folder permission: {other}")),
        }
    }
}

impl_enum_str_traits!(FolderPermission);

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
        s.parse().unwrap_or(Self::Error)
    }
}

impl FromStr for ToolCallStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ok" => Ok(Self::Ok),
            "denied" => Ok(Self::Denied),
            "error" => Ok(Self::Error),
            other => Err(format!("Unknown tool call status: {other}")),
        }
    }
}

impl_enum_str_traits!(ToolCallStatus);

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
        s.parse().unwrap_or(Self::Deny)
    }
}

impl FromStr for PermissionDecision {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deny" => Ok(Self::Deny),
            "allow_once" => Ok(Self::AllowOnce),
            "allow_chat" => Ok(Self::AllowChat),
            other => Err(format!("Unknown permission decision: {other}")),
        }
    }
}

impl_enum_str_traits!(PermissionDecision);

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
        s.parse().unwrap_or(Self::Cpu)
    }
}

impl FromStr for ExecutionDevice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            other => Err(format!("Unknown execution device: {other}")),
        }
    }
}

impl_enum_str_traits!(ExecutionDevice);

/// Configured privacy-first web search backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SearchEngine {
    #[default]
    #[serde(rename = "duckduckgo")]
    DuckDuckGo,
    #[serde(rename = "searxng")]
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
        s.parse().unwrap_or(Self::DuckDuckGo)
    }

    pub fn label(self) -> String {
        match self {
            Self::DuckDuckGo => gettext("DuckDuckGo"),
            Self::SearXNG => gettext("SearXNG"),
        }
    }
}

impl FromStr for SearchEngine {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" => Ok(Self::DuckDuckGo),
            "searxng" => Ok(Self::SearXNG),
            other => Err(format!("Unknown search engine: {other}")),
        }
    }
}

impl_enum_str_traits!(SearchEngine);

pub use crate::model_types::{
    AttachmentRecord, DEFAULT_ASSISTANT_NAME, ModelProviderType, StagedAttachment,
    compute_running_speed_avg,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_enums_round_trip() {
        for r in [
            MessageRole::System,
            MessageRole::User,
            MessageRole::Assistant,
        ] {
            assert_eq!(MessageRole::from_str_lossy(r.as_str()), r);
        }
        assert_eq!(MessageRole::from_str_lossy("model"), MessageRole::Assistant);

        for p in [FolderPermission::Read, FolderPermission::ReadWrite] {
            assert_eq!(FolderPermission::from_str_lossy(p.as_str()), p);
        }
        assert!(FolderPermission::ReadWrite.allows_write());
        assert!(!FolderPermission::Read.allows_write());

        for s in [
            ToolCallStatus::Ok,
            ToolCallStatus::Denied,
            ToolCallStatus::Error,
        ] {
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
    }

    #[test]
    fn domain_enums_handle_unknown_inputs() {
        assert_eq!(
            MessageRole::from_str_lossy("  ASSISTANT "),
            MessageRole::Assistant
        );
        assert_eq!(MessageRole::from_str_lossy("unknown"), MessageRole::User);
        assert_eq!(
            FolderPermission::from_str_lossy(" READ_WRITE "),
            FolderPermission::ReadWrite
        );
        assert_eq!(
            FolderPermission::from_str_lossy("invalid"),
            FolderPermission::Read
        );
        assert_eq!(
            ToolCallStatus::from_str_lossy(" DENIED "),
            ToolCallStatus::Denied
        );
        assert_eq!(
            ToolCallStatus::from_str_lossy("weird"),
            ToolCallStatus::Error
        );
        assert!("weird".parse::<ToolCallStatus>().is_err());
        assert_eq!(
            PermissionDecision::from_str_lossy(" Allow_Chat "),
            PermissionDecision::AllowChat
        );
        assert_eq!(
            PermissionDecision::from_str_lossy("unknown"),
            PermissionDecision::Deny
        );
        assert_eq!(
            ExecutionDevice::from_str_lossy(" GPU "),
            ExecutionDevice::Gpu
        );
        assert_eq!(ExecutionDevice::from_str_lossy("tpu"), ExecutionDevice::Cpu);
        assert_eq!(
            SearchEngine::from_str_lossy(" SearXNG "),
            SearchEngine::SearXNG
        );
        assert_eq!(
            SearchEngine::from_str_lossy("other"),
            SearchEngine::DuckDuckGo
        );
    }

    #[test]
    fn search_engine_and_enums_serde_roundtrip() {
        let ddg = serde_json::to_string(&SearchEngine::DuckDuckGo).unwrap();
        assert_eq!(ddg, "\"duckduckgo\"");
        assert_eq!(
            serde_json::from_str::<SearchEngine>(&ddg).unwrap(),
            SearchEngine::DuckDuckGo
        );

        let sx = serde_json::to_string(&SearchEngine::SearXNG).unwrap();
        assert_eq!(sx, "\"searxng\"");
        assert_eq!(
            serde_json::from_str::<SearchEngine>(&sx).unwrap(),
            SearchEngine::SearXNG
        );
    }
}
