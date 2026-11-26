//! Thin UI bridge for off-thread prompt preparation in [`crate::agent::session`].

pub(in crate::ui::chat_view) use crate::agent::session::{PreparedPrompt, PromptPrepInput};

pub(in crate::ui::chat_view) fn prepare_prompt_blocking(
    input: &PromptPrepInput,
) -> Result<PreparedPrompt, String> {
    crate::agent::session::prepare_prompt(input, crate::models::detect_gguf_supports_thinking)
}
