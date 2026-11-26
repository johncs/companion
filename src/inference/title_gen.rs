//! Automatic concise chat title generation using the active local model.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use crate::core::ThinkingLevel;
use crate::i18n::gettext;
use crate::inference::provider::{ChatEvent, ChatRole, ChatTurn, InferenceParams, ModelProvider};
use crate::storage::Database;

/// Generate and persist a short topic title after the first exchange if the chat is still untitled.
/// Returns `Some(new_title)` when the conversation was renamed.
pub async fn maybe_generate_title(
    db: &Database,
    provider: &dyn ModelProvider,
    model_path: &str,
    base_params: &InferenceParams,
    conv_id: &str,
    turns: &[ChatTurn],
    cancel_flag: &Arc<AtomicBool>,
) -> Option<String> {
    let conv = db.get_conversation(conv_id).ok().flatten()?;
    if !is_default_conversation_title(&conv.title) {
        return None;
    }
    let messages = db.list_messages_page(conv_id, None, 3).ok()?;
    if messages.len() > 2 {
        return None;
    }

    let first_user = turns
        .iter()
        .find(|t| t.role == ChatRole::User)
        .map(|t| t.content.as_str())
        .unwrap_or("");
    let user_snippet = truncate_chars(first_user, 350);

    let title_turns = vec![
        ChatTurn {
            role: ChatRole::System,
            content: "Generate a short 2 to 5 word title summarizing the user's message. Output only the plain title without quotes or punctuation.".to_string(),
            images: Vec::new(),
        },
        ChatTurn {
            role: ChatRole::User,
            content: format!("Topic: {user_snippet}"),
            images: Vec::new(),
        },
    ];
    let title_params = InferenceParams {
        n_ctx: base_params.n_ctx,
        n_gpu_layers: base_params.n_gpu_layers,
        max_tokens: 64,
        temperature: 0.2,
        thinking_level: ThinkingLevel::Off,
        mmproj_path: None,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ChatEvent>();
    let mut raw_title = String::new();
    if provider
        .stream_chat(
            model_path.to_string(),
            title_turns,
            title_params,
            cancel_flag.clone(),
            tx,
        )
        .is_ok()
    {
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Token(t) => raw_title.push_str(&t),
                ChatEvent::Done | ChatEvent::Error(_) => break,
                _ => {}
            }
        }
    }

    let new_title = sanitize_generated_title(&raw_title)
        .or_else(|| sanitize_generated_title(&truncate_chars(first_user, 45)))?;
    db.rename_conversation(conv_id, &new_title).ok()?;
    Some(new_title)
}

pub fn is_default_conversation_title(title: &str) -> bool {
    let t = title.trim();
    t.is_empty()
        || t == "New Chat"
        || t == "New Project Chat"
        || t == "Project Chat"
        || t == gettext("New Chat")
        || t == gettext("New Project Chat")
        || t == gettext("Project Chat")
}

pub fn sanitize_generated_title(raw: &str) -> Option<String> {
    let first_line = raw.lines().find(|l| !l.trim().is_empty())?.trim();
    let without_label = first_line
        .strip_prefix("Title:")
        .or_else(|| first_line.strip_prefix("title:"))
        .unwrap_or(first_line)
        .trim();
    if without_label.starts_with("User:") || without_label.starts_with("Assistant:") {
        return None;
    }
    let cleaned = without_label
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '#' | '*' | '-' | ':' | '.'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let final_title = truncate_chars(&cleaned, 60);
    (!final_title.is_empty()).then_some(final_title)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_default_titles_and_sanitizes_generated_titles() {
        assert!(is_default_conversation_title("New Chat"));
        assert!(is_default_conversation_title("New Project Chat"));
        assert!(!is_default_conversation_title("Rust GTK4 Refactoring"));

        assert_eq!(
            sanitize_generated_title("\n\"## Real Estate Photo Guide.\"\nExtra line"),
            Some("Real Estate Photo Guide".to_string())
        );
        assert_eq!(sanitize_generated_title("   \"\"  "), None);
    }
}
