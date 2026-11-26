//! Pure, unit-testable form state for the per-model configuration dialog.

use crate::core::ThinkingLevel;
use crate::core::model_params::ModelParams;
use crate::storage::ModelRecord;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelConfigFormState {
    pub ctx_presets: Vec<u32>,
    pub init_ctx_idx: u32,
    pub default_ctx_idx: u32,
    pub init_thinking_idx: u32,
    pub init_gpu_enabled: bool,
    pub init_temp_cents: i64,
}

impl ModelConfigFormState {
    pub fn from_model(model: &ModelRecord, max_ctx: u32) -> Self {
        let ctx_presets = crate::models::gguf::context_presets_up_to(max_ctx);
        let init_ctx = model.default_params.n_ctx;
        let init_ctx_idx = ctx_presets
            .iter()
            .rposition(|&v| v <= init_ctx.min(max_ctx))
            .unwrap_or(0) as u32;
        let default_ctx_idx = ctx_presets
            .iter()
            .rposition(|&v| v <= 4096_u32.min(max_ctx))
            .unwrap_or(0) as u32;
        let init_thinking = model.default_thinking_level();
        let init_thinking_idx = ThinkingLevel::ALL
            .iter()
            .position(|t| *t == init_thinking)
            .unwrap_or(0) as u32;
        let init_temp_cents = (f64::from(model.default_params.temperature) * 100.0).round() as i64;
        Self {
            ctx_presets,
            init_ctx_idx,
            default_ctx_idx,
            init_thinking_idx,
            init_gpu_enabled: model.default_params.n_gpu_layers > 0,
            init_temp_cents,
        }
    }

    pub fn is_dirty(&self, ctx_idx: u32, thinking_idx: u32, gpu: bool, temp_cents: i64) -> bool {
        ctx_idx != self.init_ctx_idx
            || thinking_idx != self.init_thinking_idx
            || gpu != self.init_gpu_enabled
            || temp_cents != self.init_temp_cents
    }

    pub fn is_default(&self, ctx_idx: u32, thinking_idx: u32, gpu: bool, temp_cents: i64) -> bool {
        let d = ModelParams::default();
        let d_temp = (f64::from(d.temperature) * 100.0).round() as i64;
        ctx_idx == self.default_ctx_idx
            && thinking_idx == 0
            && gpu == (d.n_gpu_layers > 0)
            && temp_cents == d_temp
    }

    pub fn to_model_params(
        &self,
        ctx_idx: u32,
        thinking_idx: u32,
        gpu: bool,
        temp_value: f64,
        existing_max_tokens: u32,
    ) -> ModelParams {
        ModelParams {
            n_ctx: self
                .ctx_presets
                .get(ctx_idx as usize)
                .copied()
                .unwrap_or(4096),
            n_gpu_layers: if gpu { 99 } else { 0 },
            max_tokens: existing_max_tokens,
            temperature: ((temp_value * 100.0).round() / 100.0) as f32,
            thinking_level: ThinkingLevel::from_index(thinking_idx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_state_tracks_dirty_default_and_save_params() {
        let model = ModelRecord {
            id: "m1".into(),
            display_name: "Test".into(),
            provider_type: crate::core::types::ModelProviderType::LlamaCpp,
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: true,
            default_params: ModelParams::default(),
            avg_tokens_per_sec: None,
        };
        let state = ModelConfigFormState::from_model(&model, 32768);
        assert!(!state.is_dirty(state.init_ctx_idx, 0, true, 100));
        assert!(state.is_default(state.default_ctx_idx, 0, true, 100));
        assert!(state.is_dirty(state.init_ctx_idx + 1, 0, true, 100));
        assert!(!state.is_default(state.default_ctx_idx, 1, false, 70));

        let saved = state.to_model_params(state.default_ctx_idx, 1, false, 0.7, 2048);
        assert_eq!(saved.n_ctx, 4096);
        assert_eq!(saved.n_gpu_layers, 0);
        assert_eq!(saved.max_tokens, 2048);
        assert!((saved.temperature - 0.7).abs() < 1e-4);
        assert_eq!(saved.thinking_level, ThinkingLevel::from_index(1));
    }
}
