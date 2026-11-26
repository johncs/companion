//! Configurable reasoning effort level for models that support thinking.

use serde::{Deserialize, Serialize};

use crate::i18n::gettext;

/// Configurable reasoning effort level for models that support thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    #[default]
    Default,
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 6] = [
        Self::Default,
        Self::Off,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Max,
    ];

    pub fn label(self) -> String {
        match self {
            Self::Default => gettext("Default"),
            Self::Off => gettext("Off"),
            Self::Low => gettext("Low"),
            Self::Medium => gettext("Medium"),
            Self::High => gettext("High"),
            Self::Max => gettext("Max"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            _ => Self::Default,
        }
    }

    pub fn from_index(idx: u32) -> Self {
        Self::ALL.get(idx as usize).copied().unwrap_or_default()
    }
}
