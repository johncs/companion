//! Strongly-typed per-model inference configuration parameters.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ThinkingLevel;

/// Typed configuration parameters stored per model in SQLite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelParams {
    pub n_ctx: u32,
    pub n_gpu_layers: u32,
    pub max_tokens: u32,
    pub temperature: f32,
    pub thinking_level: ThinkingLevel,
}

impl Default for ModelParams {
    fn default() -> Self {
        Self {
            n_ctx: 4096,
            n_gpu_layers: 99,
            max_tokens: 4096,
            temperature: 1.0,
            thinking_level: ThinkingLevel::Default,
        }
    }
}

impl ModelParams {
    /// Parse JSON into [`ModelParams`], falling back to defaults for missing or invalid fields.
    pub fn from_json_lossy(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Self::default();
        }
        serde_json::from_str::<Self>(trimmed).unwrap_or_default()
    }

    /// Serialize [`ModelParams`] to JSON for SQLite storage.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_params_round_trip_and_lossy_defaults() {
        let params = ModelParams {
            n_ctx: 8192,
            n_gpu_layers: 32,
            max_tokens: 4096,
            temperature: 0.4,
            thinking_level: ThinkingLevel::High,
        };
        let json = params.to_json().unwrap();
        assert_eq!(ModelParams::from_json_lossy(&json), params);

        let partial = ModelParams::from_json_lossy(r#"{"n_ctx":16384,"thinking_level":"medium"}"#);
        assert_eq!(partial.n_ctx, 16384);
        assert_eq!(partial.n_gpu_layers, 99);
        assert_eq!(partial.thinking_level, ThinkingLevel::Medium);

        assert_eq!(ModelParams::from_json_lossy("not-json"), ModelParams::default());
        assert_eq!(ModelParams::from_json_lossy("{}"), ModelParams::default());
        assert_eq!(ModelParams::from_json_lossy("   "), ModelParams::default());

        let with_unknown =
            ModelParams::from_json_lossy(r#"{"n_ctx": 2048, "future_flag": true}"#);
        assert_eq!(with_unknown.n_ctx, 2048);
        assert_eq!(with_unknown.temperature, ModelParams::default().temperature);
    }
}
