use std::cell::RefCell;

use crate::i18n::tr_f;

pub(super) struct ChatControlsFormState {
    pub(super) init_thinking_idx: u32,
    pub(super) init_ctx_val: u32,
    pub(super) init_prompt: String,
    pub(super) init_max_ctx: u32,
    pub(super) default_thinking_idx: u32,
    pub(super) ctx_presets: RefCell<Vec<u32>>,
}

impl ChatControlsFormState {
    pub(super) fn new(
        init_thinking_idx: u32,
        init_ctx_val: u32,
        init_prompt: &str,
        init_max_ctx: u32,
        default_thinking_idx: u32,
        presets: Vec<u32>,
    ) -> Self {
        Self {
            init_thinking_idx,
            init_ctx_val,
            init_prompt: init_prompt.trim().to_string(),
            init_max_ctx,
            default_thinking_idx,
            ctx_presets: RefCell::new(presets),
        }
    }

    pub(super) fn selected_ctx(&self, idx: u32) -> u32 {
        self.ctx_presets
            .borrow()
            .get(idx as usize)
            .copied()
            .unwrap_or(4096)
    }

    pub(super) fn is_dirty(&self, thinking_idx: u32, ctx_idx: u32, prompt: &str) -> bool {
        thinking_idx != self.init_thinking_idx
            || self.selected_ctx(ctx_idx) != self.init_ctx_val
            || prompt.trim() != self.init_prompt
    }

    pub(super) fn is_default(
        &self,
        thinking_idx: u32,
        ctx_idx: u32,
        prompt: &str,
        default_prompt: &str,
    ) -> bool {
        thinking_idx == self.default_thinking_idx
            && self.selected_ctx(ctx_idx) == crate::core::model_params::ModelParams::default().n_ctx
            && prompt.trim() == default_prompt.trim()
    }

    pub(super) fn format_usage(used_tokens: u32, max_ctx: u32) -> (f64, String) {
        let fraction = (used_tokens as f64 / max_ctx.max(1) as f64).clamp(0.0, 1.0);
        let pct = (fraction * 100.0).round() as u32;
        let (u, m, p) = (
            used_tokens.to_string(),
            max_ctx.to_string(),
            pct.to_string(),
        );
        let label = tr_f(
            "{used} / {max} tokens ({pct}%)",
            &[("used", &u), ("max", &m), ("pct", &p)],
        );
        (fraction, label)
    }
}

#[cfg(test)]
mod tests {
    use super::ChatControlsFormState;

    #[test]
    fn tracks_dirty_state_and_formats_usage() {
        let state =
            ChatControlsFormState::new(1, 8192, "  Be concise  ", 32768, 0, vec![4096, 8192]);
        assert!(!state.is_dirty(1, 1, "Be concise"));
        assert!(state.is_dirty(2, 1, "Be concise"));
        assert!(state.is_dirty(1, 0, "Be concise"));
        assert!(state.is_dirty(1, 1, "Updated"));

        let (frac, label) = ChatControlsFormState::format_usage(2048, 8192);
        assert!((frac - 0.25).abs() < 0.001);
        assert!(label.contains("25%"));
    }
}
