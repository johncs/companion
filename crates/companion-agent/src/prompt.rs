//! Pure prompt assembly from domain history, granted folders, and personalization settings.

use std::collections::HashSet;
use std::path::Path;

use crate::path_mentions::{detect_existing_os_paths_in_text, extract_mentioned_os_paths};
use crate::registry::ToolRegistry;
use crate::tools::{GrantedFolder, resolve_granted_path};
use companion_core::prompt::{PersonalizationProfile, compose_personalized_system_prompt};
use companion_core::provider::{ChatRole, ChatTurn};
use companion_core::types::{MessageRole, StagedAttachment};
use companion_format::attachments::{AttachmentKind, classify_path, format_attachments_for_prompt};
use companion_storage::settings::PersonalizationSettings;

/// UI-agnostic message + attachments item used to construct model prompt turns.
#[derive(Debug, Clone)]
pub struct MessageWithAttachments {
    pub role: MessageRole,
    pub content: String,
    pub attachments: Vec<StagedAttachment>,
}

/// Pure inputs required to assemble the system prompt and conversation turns.
pub struct PromptContext<'a> {
    pub chat_system_prompt: &'a str,
    pub personalization: &'a PersonalizationSettings,
    pub granted_folders: &'a [GrantedFolder],
    pub web_search_active: bool,
    pub history: &'a [MessageWithAttachments],
    pub registry: &'a ToolRegistry,
}

/// Build the full `[System, ...history]` turn list for inference.
pub fn build_prompt_turns(ctx: &PromptContext<'_>) -> Vec<ChatTurn> {
    let date_line = format_local_date(chrono::Local::now().date_naive());
    build_prompt_turns_with_date(ctx, &date_line)
}

pub(crate) fn format_local_date(date: chrono::NaiveDate) -> String {
    date.format("%Y-%m-%d (%A)").to_string()
}

pub(crate) fn build_prompt_turns_with_date(
    ctx: &PromptContext<'_>,
    current_date: &str,
) -> Vec<ChatTurn> {
    let mut sys_prompt = compose_personalized_system_prompt(
        ctx.chat_system_prompt,
        &PersonalizationProfile {
            assistant_name: &ctx.personalization.assistant_name,
            user_name: ctx.personalization.user_name.as_deref(),
            personality: ctx.personalization.personality,
            custom_personality: ctx.personalization.custom_personality.as_deref(),
            user_about: ctx.personalization.user_about.as_deref(),
        },
    );
    sys_prompt.push_str(&format!("\n\nCurrent date: {current_date}"));

    let workspace_instructions = ctx.registry.render_workspace_instructions();
    append_folder_context(
        &mut sys_prompt,
        ctx.granted_folders,
        &workspace_instructions,
    );
    if ctx.web_search_active {
        sys_prompt.push_str("\n\n");
        sys_prompt.push_str(&ctx.registry.render_web_instructions());
    }

    let mut turns = vec![ChatTurn {
        role: ChatRole::System,
        content: sys_prompt,
        images: Vec::new(),
    }];

    let start = ctx.history.len().saturating_sub(48);
    let history_slice = &ctx.history[start..];
    for msg in history_slice {
        if let Some(turn) = message_to_chat_turn(msg, ctx.granted_folders) {
            turns.push(turn);
        }
    }

    // Scan raw user message text before attachment blocks were prepended.
    append_mentioned_os_paths(&mut turns, history_slice, &workspace_instructions);
    turns
}

fn append_folder_context(
    sys_prompt: &mut String,
    granted_folders: &[GrantedFolder],
    workspace_instructions: &str,
) {
    let existing: Vec<&GrantedFolder> =
        granted_folders.iter().filter(|g| g.path.exists()).collect();
    if existing.is_empty() {
        sys_prompt.push_str("\n\nNo folders or files are currently granted for this chat.\n\n");
        sys_prompt.push_str(workspace_instructions);
        return;
    }

    sys_prompt.push_str("\n\nGranted folders & files:\n");
    for folder in existing {
        let kind = if folder.path.is_file() {
            "file"
        } else {
            "directory"
        };
        let perm = if folder.allow_write {
            "read & write"
        } else {
            "read-only"
        };
        sys_prompt.push_str(&format!("- {} [{kind}, {perm}]\n", folder.path.display()));
    }
    sys_prompt.push('\n');
    sys_prompt.push_str(workspace_instructions);
}

fn message_to_chat_turn(
    msg: &MessageWithAttachments,
    granted_folders: &[GrantedFolder],
) -> Option<ChatTurn> {
    let role = match msg.role {
        MessageRole::User => ChatRole::User,
        MessageRole::System => ChatRole::System,
        MessageRole::Assistant => ChatRole::Assistant,
    };
    let mut images: Vec<String> = msg
        .attachments
        .iter()
        .filter(|a| a.mime_type.starts_with("image/"))
        .map(|a| a.file_path.clone())
        .collect();
    if msg.role == MessageRole::User {
        append_granted_mentioned_images(&mut images, &msg.content, granted_folders);
    }
    let text_only: Vec<StagedAttachment> = msg
        .attachments
        .iter()
        .filter(|a| !a.mime_type.starts_with("image/"))
        .cloned()
        .collect();
    let att_prefix = format_attachments_for_prompt(&text_only);
    let content = if att_prefix.is_empty() {
        msg.content.clone()
    } else {
        format!("{att_prefix}{}", msg.content)
    };
    if content.is_empty() && images.is_empty() {
        return None;
    }
    Some(ChatTurn {
        role,
        content,
        images,
    })
}

fn append_granted_mentioned_images(
    images: &mut Vec<String>,
    content: &str,
    granted_folders: &[GrantedFolder],
) {
    if granted_folders.is_empty() {
        return;
    }
    for candidate in extract_mentioned_os_paths(content)
        .into_iter()
        .chain(detect_existing_os_paths_in_text(content))
    {
        if classify_path(&candidate) != AttachmentKind::Image {
            continue;
        }
        if let Some(verified) = resolve_granted_image(granted_folders, &candidate) {
            let s = verified.to_string_lossy().to_string();
            if !images.contains(&s) {
                images.push(s);
            }
        }
    }
}

fn resolve_granted_image(
    granted_folders: &[GrantedFolder],
    candidate: &Path,
) -> Option<std::path::PathBuf> {
    for folder in granted_folders {
        let Ok(canon_root) = std::fs::canonicalize(&folder.path) else {
            continue;
        };
        if canon_root.is_file() {
            if canon_root == candidate && candidate.is_file() {
                return Some(canon_root);
            }
            continue;
        }
        if let Ok(rel) = candidate.strip_prefix(&canon_root)
            && let Some(rel_str) = rel.to_str()
            && !rel_str.is_empty()
            && let Ok(resolved) = resolve_granted_path(&folder.path, rel_str)
            && resolved.is_file()
        {
            return Some(resolved);
        }
    }
    None
}

fn append_mentioned_os_paths(
    turns: &mut [ChatTurn],
    history: &[MessageWithAttachments],
    workspace_instructions: &str,
) {
    let attached_images: HashSet<String> = turns
        .iter()
        .flat_map(|t| t.images.iter().cloned())
        .collect();
    let mut mentioned_dirs: Vec<String> = Vec::new();
    for msg in history {
        if msg.role == MessageRole::User {
            for p in extract_mentioned_os_paths(&msg.content)
                .into_iter()
                .chain(detect_existing_os_paths_in_text(&msg.content))
            {
                let s = p.to_string_lossy().to_string();
                if !attached_images.contains(&s) && !mentioned_dirs.contains(&s) {
                    mentioned_dirs.push(s);
                }
            }
        }
    }
    if !mentioned_dirs.is_empty()
        && let Some(sys_turn) = turns.first_mut()
        && matches!(sys_turn.role, ChatRole::System)
    {
        sys_turn.content.push_str(
            "\n\nUser-mentioned OS paths (ask permission via tool call before reading):\n",
        );
        for d in &mentioned_dirs {
            sys_turn.content.push_str(&format!("- {d}\n"));
        }
        if !sys_turn.content.contains("Available tools:") {
            sys_turn.content.push('\n');
            sys_turn.content.push_str(workspace_instructions);
        }
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
