//! Automatic concise chat title generation using the active local model.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use companion_core::ThinkingLevel;
use companion_core::i18n::gettext;
use companion_core::model_params::ModelParams;
use companion_core::provider::{
    ChatEvent, ChatRequest, ChatRole, ChatTurn, InferenceParams, ModelProvider,
};
use companion_storage::Database;

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
    use std::sync::atomic::Ordering;

    if cancel_flag.load(Ordering::Relaxed) || !should_auto_title(db, conv_id).await {
        return None;
    }

    let first_user = turns
        .iter()
        .find(|t| t.role == ChatRole::User)
        .map(|t| t.content.as_str())
        .unwrap_or("");
    let cleaned_user = strip_attachment_tags(first_user);
    let raw_title = request_title_from_provider(
        provider,
        model_path,
        base_params,
        &cleaned_user,
        cancel_flag,
    )
    .await;

    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }

    let new_title = sanitize_generated_title(&raw_title)
        .or_else(|| sanitize_generated_title(&truncate_chars(&cleaned_user, 45)))?;
    persist_generated_title(db, conv_id, new_title).await
}

async fn should_auto_title(db: &Database, conv_id: &str) -> bool {
    let db_conv = db.clone();
    let cid = conv_id.to_string();
    let Some(conv) =
        tokio::task::spawn_blocking(move || db_conv.get_conversation(&cid).ok().flatten())
            .await
            .ok()
            .flatten()
    else {
        return false;
    };
    if !is_default_conversation_title(&conv.title) {
        return false;
    }
    let db_msgs = db.clone();
    let cid = conv_id.to_string();
    tokio::task::spawn_blocking(move || db_msgs.list_messages_page(&cid, None, 3).ok())
        .await
        .ok()
        .flatten()
        .is_some_and(|msgs| msgs.len() <= 2)
}

async fn request_title_from_provider(
    provider: &dyn ModelProvider,
    model_path: &str,
    base_params: &InferenceParams,
    cleaned_user: &str,
    cancel_flag: &Arc<AtomicBool>,
) -> String {
    let (title_turns, title_params) = build_title_request_params(base_params, cleaned_user);

    let (tx, mut rx) = mpsc::unbounded_channel::<ChatEvent>();
    let mut raw_title = String::new();
    if provider
        .stream_chat(ChatRequest {
            model_path: model_path.to_string(),
            messages: title_turns,
            params: title_params,
            cancel_flag: cancel_flag.clone(),
            tx,
        })
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
    raw_title
}

async fn persist_generated_title(
    db: &Database,
    conv_id: &str,
    new_title: String,
) -> Option<String> {
    let db = db.clone();
    let cid = conv_id.to_string();
    let title_save = new_title.clone();
    if let Err(err) = tokio::task::spawn_blocking(move || db.rename_conversation(&cid, &title_save))
        .await
        .ok()?
    {
        tracing::warn!("Failed to persist generated chat title: {err}");
        return None;
    }
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

fn strip_attachment_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<attachment") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(end_tag) = after.find("</attachment>") {
            rest = &after[end_tag + "</attachment>".len()..];
        } else if let Some(self_close) = after.find("/>") {
            rest = &after[self_close + "/>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_title_request_params(
    base_params: &InferenceParams,
    cleaned_user: &str,
) -> (Vec<ChatTurn>, InferenceParams) {
    let user_snippet = truncate_chars(cleaned_user, 350);
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
        model: ModelParams {
            n_ctx: base_params.n_ctx,
            n_gpu_layers: base_params.n_gpu_layers,
            max_tokens: 64,
            temperature: 0.2,
            thinking_level: ThinkingLevel::Off,
        },
        mmproj_path: None,
    };
    (title_turns, title_params)
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

    #[test]
    fn strip_attachment_tags_removes_xml_for_fallback_title() {
        let raw = "<attachment name=\"notes.txt\">secret body</attachment> Summarize this file";
        let cleaned = strip_attachment_tags(raw);
        assert_eq!(cleaned, "Summarize this file");
        assert_eq!(
            sanitize_generated_title(&truncate_chars(&cleaned, 45)),
            Some("Summarize this file".to_string())
        );
    }
}
