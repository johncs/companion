//! Model provider, attachment, and speed-averaging types shared across crates.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Default companion display name used across storage, prompt, and UI fallbacks.
pub const DEFAULT_ASSISTANT_NAME: &str = "Companion";

/// Inference backend provider for a registered model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderType {
    #[default]
    LlamaCpp,
}

impl ModelProviderType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama_cpp",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "llama_cpp" => Self::LlamaCpp,
            _ => Self::LlamaCpp,
        }
    }
}

impl FromStr for ModelProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "llama_cpp" => Ok(Self::LlamaCpp),
            other => Err(format!("Unknown model provider: {other}")),
        }
    }
}

impl AsRef<str> for ModelProviderType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Update a capped running average (window up to 20 samples) with a new `tokens_per_sec` sample.
pub fn compute_running_speed_avg(
    prev_avg: Option<f64>,
    prev_count: i64,
    tokens_per_sec: f64,
) -> Option<(f64, i64)> {
    if !tokens_per_sec.is_finite() || tokens_per_sec <= 0.0 {
        return None;
    }
    let new_count = prev_count.saturating_add(1);
    let weight = new_count.min(20) as f64;
    let new_avg = match prev_avg {
        Some(avg) if prev_count > 0 => ((avg * (weight - 1.0)) + tokens_per_sec) / weight,
        _ => tokens_per_sec,
    };
    Some((new_avg, new_count))
}

/// Unsaved attachment staged in the composer before persistence.
#[derive(Debug, Clone)]
pub struct StagedAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub file_path: String,
    pub extracted_text: Option<String>,
    pub byte_size: i64,
}

/// A file or image attached to a persisted chat message.
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

impl AttachmentRecord {
    /// Convert a persisted attachment into a [`StagedAttachment`] (e.g. for prompt building).
    pub fn to_staged(&self) -> StagedAttachment {
        StagedAttachment {
            file_name: self.file_name.clone(),
            mime_type: self.mime_type.clone(),
            file_path: self.file_path.clone(),
            extracted_text: self.extracted_text.clone(),
            byte_size: self.byte_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_speed_avg_caps_window_and_ignores_invalid_samples() {
        assert_eq!(compute_running_speed_avg(None, 0, 0.0), None);
        assert_eq!(compute_running_speed_avg(None, 0, -5.0), None);
        assert_eq!(compute_running_speed_avg(None, 0, f64::NAN), None);

        assert_eq!(compute_running_speed_avg(None, 0, 30.0), Some((30.0, 1)));
        assert_eq!(
            compute_running_speed_avg(Some(30.0), 1, 50.0),
            Some((40.0, 2))
        );
        // Window caps at 20 samples even when prev_count >= 20.
        let (avg, count) = compute_running_speed_avg(Some(40.0), 25, 60.0).unwrap();
        assert_eq!(count, 26);
        assert!((avg - 41.0).abs() < 1e-9);
    }
}
