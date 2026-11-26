//! Chat template formatting, thinking directives, and context-budget turn trimming.

use anyhow::{Result, anyhow};
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::mtmd_default_marker;

use crate::core::ThinkingLevel;
use crate::inference::provider::ChatTurn;

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
    let is_gemma_think = template_src.contains("<|think|>")
        || template_src.contains("<|channel>thought")
        || template_src.contains("<|turn>");
    let is_qwen_think = !is_gemma_think
        && (template_src.contains("enable_thinking") || template_src.contains("/no_think"));

    let media_marker = mtmd_default_marker();
    let mut prepared: Vec<(String, String)> = messages
        .iter()
        .map(|m| {
            let mut content = m.content.clone();
            if has_vision && !m.images.is_empty() {
                let markers = vec![media_marker; m.images.len()].join("\n");
                content = if content.is_empty() {
                    markers
                } else {
                    format!("{markers}\n{content}")
                };
            } else if !m.images.is_empty() {
                let notes = m
                    .images
                    .iter()
                    .map(|p| format!("[Attached image: {p}]"))
                    .collect::<Vec<_>>()
                    .join("\n");
                content = if content.is_empty() {
                    notes
                } else {
                    format!("{notes}\n{content}")
                };
            }
            (m.role.as_str().to_string(), content)
        })
        .collect();

    let is_explicit_thinking = !matches!(thinking_level, ThinkingLevel::Default | ThinkingLevel::Off);

    if thinking_level == ThinkingLevel::Off {
        if is_qwen_think
            && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
        {
            content.push_str(" /no_think");
        }
    } else if is_explicit_thinking {
        let effort = match thinking_level {
            ThinkingLevel::Low => "low (brief)",
            ThinkingLevel::Medium => "medium (balanced)",
            ThinkingLevel::High => "high (thorough)",
            ThinkingLevel::Max => "maximum (deep, exhaustive)",
            ThinkingLevel::Default | ThinkingLevel::Off => "",
        };
        let directive = if is_gemma_think {
            format!(
                "<|think|>\nReasoning effort: {effort}. Think step by step before giving your final answer."
            )
        } else {
            format!(
                "Reasoning effort: {effort}. Show your step-by-step thinking inside <think>...</think> tags before your final answer."
            )
        };

        if let Some((_, sys)) = prepared.iter_mut().find(|(r, _)| r == "system") {
            if is_gemma_think && !sys.starts_with("<|think|>") {
                *sys = format!("{directive}\n\n{sys}");
            } else if !is_gemma_think {
                sys.push_str("\n\n");
                sys.push_str(&directive);
            }
        } else {
            prepared.insert(0, ("system".to_string(), directive));
        }

        if is_qwen_think
            && let Some((_, content)) = prepared.iter_mut().rev().find(|(r, _)| r == "user")
        {
            content.push_str(" /think");
        }
    }

    // Gemma 4 uses `<|turn>system / user / model` and `<|think|>` + `<|channel>thought`.
    if is_gemma_think && template_src.contains("<|turn>") {
        let mut out = String::new();
        for (role, content) in &prepared {
            let gemma_role = if role == "assistant" { "model" } else { role.as_str() };
            out.push_str(&format!("<|turn>{gemma_role}\n{content}<turn|>\n"));
        }
        out.push_str("<|turn>model\n");
        let initial_close = if is_explicit_thinking {
            out.push_str("<|channel>thought\n");
            Some("<channel|>")
        } else {
            None
        };
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
        && let Ok(mut formatted) = model.apply_chat_template(template, &chat_msgs, true)
    {
        let initial_close = if is_explicit_thinking {
            if is_gemma_think && formatted.ends_with("<|turn>model\n") {
                formatted.push_str("<|channel>thought\n");
                Some("<channel|>")
            } else if is_qwen_think {
                if let Some(stripped) = formatted.strip_suffix("<think>\n\n</think>\n\n") {
                    formatted = stripped.to_string();
                }
                if formatted.ends_with("<|im_start|>assistant\n") {
                    formatted.push_str("<think>\n");
                    Some("</think>")
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        tracing::info!(
            "format_chat_prompt (template): thinking_level={:?}, initial_close={:?}, tail={:?}",
            thinking_level,
            initial_close,
            &formatted[formatted.len().saturating_sub(80)..]
        );
        return Ok((formatted, initial_close));
    }

    // ChatML fallback if the GGUF lacks a supported built-in template.
    let mut fallback = String::new();
    for (role, content) in &prepared {
        fallback.push_str(&format!("<|im_start|>{role}\n{content}<|im_end|>\n"));
    }
    fallback.push_str("<|im_start|>assistant\n");
    let initial_close = if is_explicit_thinking {
        fallback.push_str("<think>\n");
        Some("</think>")
    } else {
        None
    };
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
        if matches!(turn.role, crate::inference::provider::ChatRole::System) {
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
    use crate::inference::provider::ChatRole;

    fn turn(role: ChatRole, content: &str) -> ChatTurn {
        ChatTurn {
            role,
            content: content.to_string(),
            images: Vec::new(),
        }
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
}
