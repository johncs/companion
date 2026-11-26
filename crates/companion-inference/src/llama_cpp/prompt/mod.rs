//! Chat template formatting, thinking directives, and context-budget turn trimming.

mod budget;
mod templates;
#[cfg(test)]
mod tests;

use anyhow::{Result, anyhow};
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::mtmd_default_marker;

use companion_core::ThinkingLevel;
use companion_core::provider::ChatTurn;

pub(super) use budget::trim_turns_to_budget;
use templates::{
    ChatTemplateFamily, detect_template_family, format_chatml_fallback, format_gemma4_prompt,
    post_process_template_output, prepare_turns,
};

pub(super) fn format_chat_prompt(
    model: &LlamaModel,
    messages: &[ChatTurn],
    thinking_level: ThinkingLevel,
    has_vision: bool,
) -> Result<(String, Option<&'static str>)> {
    let template = model.chat_template(None).ok();
    let template_src = template
        .as_ref()
        .and_then(|t| t.to_str().ok())
        .unwrap_or("");
    let family = detect_template_family(template_src);
    let prepared = prepare_turns(
        messages,
        thinking_level,
        has_vision,
        mtmd_default_marker(),
        family,
    );

    if family == ChatTemplateFamily::Gemma4 && template_src.contains("<|turn>") {
        let (out, initial_close) = format_gemma4_prompt(&prepared, thinking_level);
        return Ok((out, initial_close));
    }

    let mut chat_msgs = Vec::with_capacity(prepared.len());
    for (role, content) in &prepared {
        chat_msgs.push(
            LlamaChatMessage::new(role.clone(), content.clone())
                .map_err(|e| anyhow!("Invalid chat message: {e}"))?,
        );
    }

    if let Some(template) = template.as_ref()
        && let Ok(raw) = model.apply_chat_template(template, &chat_msgs, true)
    {
        return Ok(post_process_template_output(raw, family, thinking_level));
    }

    Ok(format_chatml_fallback(&prepared, thinking_level))
}
