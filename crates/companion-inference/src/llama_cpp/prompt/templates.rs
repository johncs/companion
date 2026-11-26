//! Chat template family detection, thinking directives, and Gemma4/ChatML formatting.

use companion_core::ThinkingLevel;
use companion_core::provider::ChatTurn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatTemplateFamily {
    Gemma4,
    QwenThinking,
    Generic,
}

pub(super) fn detect_template_family(template_src: &str) -> ChatTemplateFamily {
    if template_src.contains("<|think|>")
        || template_src.contains("<|channel>thought")
        || template_src.contains("<|turn>")
    {
        ChatTemplateFamily::Gemma4
    } else if template_src.contains("enable_thinking") || template_src.contains("/no_think") {
        ChatTemplateFamily::QwenThinking
    } else {
        ChatTemplateFamily::Generic
    }
}

pub(super) fn prepare_turns(
    messages: &[ChatTurn],
    thinking_level: ThinkingLevel,
    has_vision: bool,
    media_marker: &str,
    family: ChatTemplateFamily,
) -> Vec<(String, String)> {
    let mut prepared: Vec<(String, String)> = messages
        .iter()
        .map(|m| {
            let content = attach_image_markers(&m.content, &m.images, has_vision, media_marker);
            (m.role.as_str().to_string(), content)
        })
        .collect();

    apply_thinking_directives(&mut prepared, thinking_level, family);
    prepared
}

fn attach_image_markers(
    base: &str,
    images: &[String],
    has_vision: bool,
    media_marker: &str,
) -> String {
    if images.is_empty() {
        return base.to_string();
    }
    let prefix = if has_vision {
        vec![media_marker; images.len()].join("\n")
    } else {
        images
            .iter()
            .map(|p| format!("[Attached image: {p}]"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if base.is_empty() {
        prefix
    } else {
        format!("{prefix}\n{base}")
    }
}

fn apply_thinking_directives(
    prepared: &mut Vec<(String, String)>,
    thinking_level: ThinkingLevel,
    family: ChatTemplateFamily,
) {
    if thinking_level == ThinkingLevel::Off {
        if family == ChatTemplateFamily::QwenThinking
            && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
        {
            content.push_str(" /no_think");
        }
        return;
    }

    let Some(effort) = thinking_effort_label(thinking_level) else {
        return;
    };

    let directive = if family == ChatTemplateFamily::Gemma4 {
        format!(
            "<|think|>\nReasoning effort: {effort}. Think step by step before giving your final answer."
        )
    } else {
        format!(
            "Reasoning effort: {effort}. Show your step-by-step thinking inside <think>...</think> tags before your final answer."
        )
    };

    if let Some((_, sys)) = prepared.iter_mut().find(|(r, _)| r == "system") {
        if family == ChatTemplateFamily::Gemma4 && !sys.starts_with("<|think|>") {
            *sys = format!("{directive}\n\n{sys}");
        } else if family != ChatTemplateFamily::Gemma4 {
            sys.push_str("\n\n");
            sys.push_str(&directive);
        }
    } else {
        prepared.insert(0, ("system".to_string(), directive));
    }

    if family == ChatTemplateFamily::QwenThinking
        && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
    {
        content.push_str(" /think");
    }
}

fn thinking_effort_label(level: ThinkingLevel) -> Option<&'static str> {
    match level {
        ThinkingLevel::Low => Some("low (brief)"),
        ThinkingLevel::Medium => Some("medium (balanced)"),
        ThinkingLevel::High => Some("high (thorough)"),
        ThinkingLevel::Max => Some("maximum (deep, exhaustive)"),
        ThinkingLevel::Default | ThinkingLevel::Off => None,
    }
}

pub(super) fn format_gemma4_prompt(
    prepared: &[(String, String)],
    thinking_level: ThinkingLevel,
) -> (String, Option<&'static str>) {
    let mut out = String::new();
    for (role, content) in prepared {
        let gemma_role = if role == "assistant" {
            "model"
        } else {
            role.as_str()
        };
        out.push_str(&format!("<|turn>{gemma_role}\n{content}<turn|>\n"));
    }
    out.push_str("<|turn>model\n");
    let initial_close = if thinking_effort_label(thinking_level).is_some() {
        out.push_str("<|channel>thought\n");
        Some("<channel|>")
    } else {
        None
    };
    (out, initial_close)
}

pub(super) fn format_chatml_fallback(
    prepared: &[(String, String)],
    thinking_level: ThinkingLevel,
) -> (String, Option<&'static str>) {
    let mut fallback = String::new();
    for (role, content) in prepared {
        fallback.push_str(&format!("<|im_start|>{role}\n{content}<|im_end|>\n"));
    }
    fallback.push_str("<|im_start|>assistant\n");
    let initial_close = if thinking_effort_label(thinking_level).is_some() {
        fallback.push_str("<think>\n");
        Some("</think>")
    } else {
        None
    };
    (fallback, initial_close)
}

pub(super) fn post_process_template_output(
    mut formatted: String,
    family: ChatTemplateFamily,
    thinking_level: ThinkingLevel,
) -> (String, Option<&'static str>) {
    if thinking_effort_label(thinking_level).is_none() {
        return (formatted, None);
    }

    let initial_close = match family {
        ChatTemplateFamily::Gemma4 if formatted.ends_with("<|turn>model\n") => {
            formatted.push_str("<|channel>thought\n");
            Some("<channel|>")
        }
        ChatTemplateFamily::QwenThinking => {
            if let Some(stripped) = formatted.strip_suffix("<think>\n\n</think>\n\n") {
                formatted = stripped.to_string();
            }
            if formatted.ends_with("<|im_start|>assistant\n") {
                formatted.push_str("<think>\n");
                Some("</think>")
            } else {
                None
            }
        }
        _ => None,
    };
    (formatted, initial_close)
}
