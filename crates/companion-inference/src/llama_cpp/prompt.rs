//! Chat template formatting, thinking directives, and context-budget turn trimming.

use anyhow::{Result, anyhow};
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::mtmd_default_marker;

use companion_core::ThinkingLevel;
use companion_core::provider::{ChatRole, ChatTurn};

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

    // Gemma 4 uses `<|turn>system / user / model` and `<|think|>` + `<|channel>thought`.
    if family == ChatTemplateFamily::Gemma4 && template_src.contains("<|turn>") {
        let (out, initial_close) = format_gemma4_prompt(&prepared, thinking_level);
        tracing::info!(
            "format_chat_prompt (gemma4): thinking_level={:?}, initial_close={:?}, tail={:?}",
            thinking_level,
            initial_close,
            &out[out.len().saturating_sub(80)..]
        );
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
        let (formatted, initial_close) =
            post_process_template_output(raw, family, thinking_level);
        tracing::info!(
            "format_chat_prompt (template): thinking_level={:?}, initial_close={:?}, tail={:?}",
            thinking_level,
            initial_close,
            &formatted[formatted.len().saturating_sub(80)..]
        );
        return Ok((formatted, initial_close));
    }

    let (fallback, initial_close) = format_chatml_fallback(&prepared, thinking_level);
    tracing::info!(
        "format_chat_prompt (fallback): thinking_level={:?}, initial_close={:?}, tail={:?}",
        thinking_level,
        initial_close,
        &fallback[fallback.len().saturating_sub(80)..]
    );
    Ok((fallback, initial_close))
}

/// Pin system turns and the latest non-system turn, then keep as many recent turns as fit
/// inside `n_ctx` minus a reply reserve so long chats continue seamlessly without overflowing.
pub(super) fn trim_turns_to_budget(
    turns: &[ChatTurn],
    n_ctx: u32,
    count_tokens: impl Fn(&str) -> usize,
) -> Vec<ChatTurn> {
    const TURN_FRAMING_OVERHEAD: usize = 8;
    let ctx_budget = n_ctx.max(512);
    let reply_reserve = (ctx_budget / 4).clamp(256, 2048);
    let max_prompt_tokens = (ctx_budget - reply_reserve) as usize;

    let mut system_turns = Vec::new();
    let mut dialog_turns = Vec::new();
    let mut used_tokens = 0usize;

    for turn in turns {
        if matches!(turn.role, ChatRole::System) {
            used_tokens += count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
            system_turns.push(turn.clone());
        } else {
            dialog_turns.push(turn);
        }
    }

    let Some((&latest, earlier)) = dialog_turns.split_last() else {
        return system_turns;
    };

    used_tokens += count_tokens(&latest.content) + TURN_FRAMING_OVERHEAD;
    let mut kept_rev = vec![latest.clone()];

    for &turn in earlier.iter().rev() {
        let cost = count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
        if used_tokens + cost > max_prompt_tokens {
            break;
        }
        used_tokens += cost;
        kept_rev.push(turn.clone());
    }

    kept_rev.reverse();
    system_turns.extend(kept_rev);
    system_turns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: ChatRole, content: &str) -> ChatTurn {
        ChatTurn {
            role,
            content: content.to_string(),
            images: Vec::new(),
        }
    }

    #[test]
    fn detects_chat_template_families() {
        assert_eq!(
            detect_template_family("<|turn>user\n{{ content }}<turn|>"),
            ChatTemplateFamily::Gemma4
        );
        assert_eq!(
            detect_template_family("{% if enable_thinking %}<think>{% endif %}"),
            ChatTemplateFamily::QwenThinking
        );
        assert_eq!(
            detect_template_family("<|im_start|>user\n{{ content }}<|im_end|>"),
            ChatTemplateFamily::Generic
        );
    }

    #[test]
    fn prepares_vision_markers_and_non_vision_notes() {
        let mut msg = turn(ChatRole::User, "Describe this");
        msg.images = vec!["/tmp/a.png".to_string(), "/tmp/b.png".to_string()];

        let with_vision = prepare_turns(
            &[msg.clone()],
            ThinkingLevel::Default,
            true,
            "<__media__>",
            ChatTemplateFamily::Generic,
        );
        assert_eq!(
            with_vision[0].1,
            "<__media__>\n<__media__>\nDescribe this"
        );

        let without_vision = prepare_turns(
            &[msg],
            ThinkingLevel::Default,
            false,
            "<__media__>",
            ChatTemplateFamily::Generic,
        );
        assert_eq!(
            without_vision[0].1,
            "[Attached image: /tmp/a.png]\n[Attached image: /tmp/b.png]\nDescribe this"
        );
    }

    #[test]
    fn formats_gemma4_with_and_without_thinking() {
        let msgs = vec![
            turn(ChatRole::System, "Be helpful."),
            turn(ChatRole::User, "Hi"),
            turn(ChatRole::Assistant, "Hello"),
        ];
        let prepared = prepare_turns(
            &msgs,
            ThinkingLevel::Medium,
            false,
            "<__media__>",
            ChatTemplateFamily::Gemma4,
        );
        let (prompt, close_tag) = format_gemma4_prompt(&prepared, ThinkingLevel::Medium);
        assert_eq!(close_tag, Some("<channel|>"));
        assert!(prompt.contains("<|turn>system\n<|think|>\nReasoning effort: medium (balanced)."));
        assert!(prompt.contains("<|turn>model\nHello<turn|>\n"));
        assert!(prompt.ends_with("<|turn>model\n<|channel>thought\n"));

        let prepared_off = prepare_turns(
            &msgs,
            ThinkingLevel::Off,
            false,
            "<__media__>",
            ChatTemplateFamily::Gemma4,
        );
        let (prompt_off, close_off) = format_gemma4_prompt(&prepared_off, ThinkingLevel::Off);
        assert_eq!(close_off, None);
        assert!(prompt_off.ends_with("<|turn>model\n"));
    }

    #[test]
    fn applies_qwen_think_and_no_think_flags() {
        let msgs = vec![turn(ChatRole::User, "Solve 2+2")];
        let on = prepare_turns(
            &msgs,
            ThinkingLevel::High,
            false,
            "<__media__>",
            ChatTemplateFamily::QwenThinking,
        );
        assert_eq!(on[0].0, "system");
        assert!(on[1].1.ends_with(" /think"));

        let off = prepare_turns(
            &msgs,
            ThinkingLevel::Off,
            false,
            "<__media__>",
            ChatTemplateFamily::QwenThinking,
        );
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].1, "Solve 2+2 /no_think");

        let (processed, close) = post_process_template_output(
            "<|im_start|>assistant\n<think>\n\n</think>\n\n".to_string(),
            ChatTemplateFamily::QwenThinking,
            ThinkingLevel::High,
        );
        assert_eq!(close, Some("</think>"));
        assert_eq!(processed, "<|im_start|>assistant\n<think>\n");
    }

    #[test]
    fn formats_chatml_fallback_with_explicit_thinking() {
        let msgs = vec![turn(ChatRole::User, "Ping")];
        let prepared = prepare_turns(
            &msgs,
            ThinkingLevel::Low,
            false,
            "<__media__>",
            ChatTemplateFamily::Generic,
        );
        let (prompt, close) = format_chatml_fallback(&prepared, ThinkingLevel::Low);
        assert_eq!(close, Some("</think>"));
        assert!(prompt.contains("<|im_start|>system\nReasoning effort: low (brief)."));
        assert!(prompt.ends_with("<|im_start|>assistant\n<think>\n"));
    }

    #[test]
    fn trims_oldest_turns_while_preserving_system_and_latest_user() {
        let turns = vec![
            turn(ChatRole::System, &"s".repeat(40)),
            turn(ChatRole::User, &"1".repeat(200)),
            turn(ChatRole::Assistant, &"2".repeat(200)),
            turn(ChatRole::User, &"3".repeat(100)),
            turn(ChatRole::Assistant, &"4".repeat(100)),
            turn(ChatRole::User, &"5".repeat(80)),
        ];
        // n_ctx=512 -> reply_reserve=256 -> max_prompt_tokens=256.
        // system (48) + latest "5" (88) = 136; adding "4" (108) = 244 <= 256; adding "3" (108) = 352 > 256.
        let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
        assert_eq!(kept.len(), 3);
        assert!(matches!(kept[0].role, ChatRole::System));
        assert!(kept[1].content.starts_with('4'));
        assert!(kept[2].content.starts_with('5'));
    }

    #[test]
    fn keeps_latest_turn_even_when_over_budget() {
        let turns = vec![
            turn(ChatRole::System, "sys"),
            turn(ChatRole::User, "old"),
            turn(ChatRole::User, &"x".repeat(2000)),
        ];
        let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].content, "sys");
        assert_eq!(kept[1].content.len(), 2000);
    }

    #[test]
    fn prepare_turns_merges_existing_system_prompt_idempotently() {
        let generic_msgs = vec![
            turn(ChatRole::System, "You are helpful."),
            turn(ChatRole::User, "Hi"),
        ];
        let generic = prepare_turns(
            &generic_msgs,
            ThinkingLevel::Max,
            false,
            "<__media__>",
            ChatTemplateFamily::Generic,
        );
        assert_eq!(generic.len(), 2);
        assert!(generic[0].1.starts_with("You are helpful.\n\nReasoning effort: maximum"));

        let gemma_msgs = vec![
            turn(ChatRole::System, "<|think|>\nAlready set."),
            turn(ChatRole::User, "Hi"),
        ];
        let gemma = prepare_turns(
            &gemma_msgs,
            ThinkingLevel::Low,
            false,
            "<__media__>",
            ChatTemplateFamily::Gemma4,
        );
        assert_eq!(gemma[0].1, "<|think|>\nAlready set.");
    }
}
