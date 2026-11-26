//! Configurable character/tone preset for the personalized assistant.

use serde::{Deserialize, Serialize};

use crate::i18n::gettext;
use crate::render_placeholders;

/// Configurable character/tone preset for the personalized assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
            Self::Friendly => gettext("Friendly & Warm"),
            Self::Balanced => gettext("Balanced"),
            Self::Professional => gettext("Direct & Professional"),
            Self::Playful => gettext("Playful & Witty"),
            Self::Custom => gettext("Custom"),
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
            let user_label = user_name.unwrap_or("the user");
            return render_placeholders(
                raw,
                &[
                    ("assistant_name", assistant_name),
                    ("user_name", user_label),
                ],
            );
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
        assert_eq!(
            AssistantPersonality::from_str_lossy("playful"),
            AssistantPersonality::Playful
        );
        assert_eq!(
            AssistantPersonality::from_str_lossy("unknown"),
            AssistantPersonality::Friendly
        );
    }
}
