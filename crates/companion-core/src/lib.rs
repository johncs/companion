//! Core domain logic: Markdown rendering, thinking levels, and shared async runtime.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod attachments;
pub mod i18n;
pub mod markdown;
pub mod model_params;
pub mod paths;
pub mod prompt;
pub mod provider;
pub mod types;

use std::sync::LazyLock;

static TOKIO_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
});

/// Shared multi-threaded Tokio runtime for background inference, downloads, and MCP I/O.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    &TOKIO_RT
}

/// Process `items` in parallel across CPU cores (up to 8 workers) when `items.len() >= min_parallel`.
pub fn parallel_chunks<T, R, F>(items: &[T], min_parallel: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&[T]) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() < min_parallel {
        return vec![f(items)];
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .min(items.len());
    let chunk_size = items.len().div_ceil(workers).max(1);

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in items.chunks(chunk_size) {
            handles.push(s.spawn(|| f(chunk)));
        }
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    })
}

/// Configurable reasoning effort level for models that support thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
            Self::Default => crate::i18n::gettext("Default"),
            Self::Off => crate::i18n::gettext("Off"),
            Self::Low => crate::i18n::gettext("Low"),
            Self::Medium => crate::i18n::gettext("Medium"),
            Self::High => crate::i18n::gettext("High"),
            Self::Max => crate::i18n::gettext("Max"),
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

/// Configurable character/tone preset for the personalized assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssistantPersonality {
    #[default]
    Friendly,
    Balanced,
    Professional,
    Playful,
    Custom,
}

impl AssistantPersonality {
    pub const ALL: [Self; 5] = [
        Self::Friendly,
        Self::Balanced,
        Self::Professional,
        Self::Playful,
        Self::Custom,
    ];

    pub fn label(self) -> String {
        match self {
            Self::Friendly => crate::i18n::gettext("Friendly & Warm"),
            Self::Balanced => crate::i18n::gettext("Balanced"),
            Self::Professional => crate::i18n::gettext("Direct & Professional"),
            Self::Playful => crate::i18n::gettext("Playful & Witty"),
            Self::Custom => crate::i18n::gettext("Custom"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Friendly => "friendly",
            Self::Balanced => "balanced",
            Self::Professional => "professional",
            Self::Playful => "playful",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "balanced" => Self::Balanced,
            "professional" => Self::Professional,
            "playful" => Self::Playful,
            "custom" => Self::Custom,
            _ => Self::Friendly,
        }
    }

    pub fn from_index(idx: u32) -> Self {
        Self::ALL.get(idx as usize).copied().unwrap_or_default()
    }

    /// Behavioral guidance injected into the system prompt so the model embodies the chosen character.
    pub fn prompt_guidance(self, assistant_name: &str, user_name: Option<&str>) -> String {
        match (self, user_name) {
            (Self::Friendly, Some(u)) => format!(
                "You are {assistant_name}, {u}'s warm, supportive friend and personal assistant. Speak naturally and conversationally, be encouraging and genuine, and address {u} by name naturally without overusing it."
            ),
            (Self::Friendly, None) => format!(
                "You are {assistant_name}, a warm, supportive friend and personal assistant. Speak naturally and conversationally, and be encouraging and genuine."
            ),
            (Self::Balanced, Some(u)) => format!(
                "You are {assistant_name}, a helpful, thoughtful personal assistant to {u}."
            ),
            (Self::Balanced, None) => {
                format!("You are {assistant_name}, a helpful, thoughtful personal assistant.")
            }
            (Self::Professional, Some(u)) => format!(
                "You are {assistant_name}, a concise, direct, and professional assistant to {u}. Address {u} respectfully when appropriate."
            ),
            (Self::Professional, None) => {
                format!("You are {assistant_name}, a concise, direct, and professional assistant.")
            }
            (Self::Playful, Some(u)) => format!(
                "You are {assistant_name}, {u}'s witty, upbeat friend and assistant. Keep the tone light, engaging, and personable, and address {u} by name naturally without overdoing it."
            ),
            (Self::Playful, None) => format!(
                "You are {assistant_name}, a witty, upbeat friend and assistant. Keep the tone light, engaging, and personable."
            ),
            (Self::Custom, _) => Self::Balanced.prompt_guidance(assistant_name, user_name),
        }
    }

    /// Resolve behavioral guidance, using `custom_text` (with placeholder substitution) when `Custom` is active.
    pub fn resolve_guidance(
        self,
        assistant_name: &str,
        user_name: Option<&str>,
        custom_text: Option<&str>,
    ) -> String {
        if self == Self::Custom
            && let Some(raw) = custom_text.map(str::trim).filter(|s| !s.is_empty())
        {
            return raw
                .replace("{assistant_name}", assistant_name)
                .replace("{user_name}", user_name.unwrap_or("the user"));
        }
        self.prompt_guidance(assistant_name, user_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personality_guidance_includes_names() {
        let msg = AssistantPersonality::Friendly.prompt_guidance("Emma", Some("John"));
        assert!(msg.contains("Emma"));
        assert!(msg.contains("John"));
        assert_eq!(AssistantPersonality::from_str_lossy("playful"), AssistantPersonality::Playful);
        assert_eq!(AssistantPersonality::from_str_lossy("unknown"), AssistantPersonality::Friendly);
    }
}
