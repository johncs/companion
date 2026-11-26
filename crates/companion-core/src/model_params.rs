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

    /// Validate domain invariants (finite non-negative temperature, non-zero context and max tokens).
    pub fn validate(&self) -> Result<(), String> {
        if !self.temperature.is_finite() || self.temperature < 0.0 {
            return Err("Temperature must be a finite non-negative number".to_string());
        }
        if self.n_ctx == 0 {
            return Err("Context length (n_ctx) must be greater than 0".to_string());
        }
        if self.max_tokens == 0 {
            return Err("Max tokens must be greater than 0".to_string());
        }
        Ok(())
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

        assert_eq!(
            ModelParams::from_json_lossy("not-json"),
            ModelParams::default()
        );
        assert_eq!(ModelParams::from_json_lossy("{}"), ModelParams::default());
        assert_eq!(ModelParams::from_json_lossy("   "), ModelParams::default());

        let with_unknown = ModelParams::from_json_lossy(r#"{"n_ctx": 2048, "future_flag": true}"#);
        assert_eq!(with_unknown.n_ctx, 2048);
        assert_eq!(with_unknown.temperature, ModelParams::default().temperature);
    }

    #[test]
    fn model_params_validate_enforces_invariants() {
        assert!(ModelParams::default().validate().is_ok());

        let nan_temp = ModelParams {
            temperature: f32::NAN,
            ..ModelParams::default()
        };
        assert!(nan_temp.validate().is_err());

        let neg_temp = ModelParams {
            temperature: -0.1,
            ..ModelParams::default()
        };
        assert!(neg_temp.validate().is_err());

        let zero_ctx = ModelParams {
            n_ctx: 0,
            ..ModelParams::default()
        };
        assert!(zero_ctx.validate().is_err());

        let zero_max = ModelParams {
            max_tokens: 0,
            ..ModelParams::default()
        };
        assert!(zero_max.validate().is_err());
    }
}
