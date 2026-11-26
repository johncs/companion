//! Pure prompt assembly from domain history, granted folders, and personalization settings.

use crate::path_mentions::{detect_existing_os_paths_in_text, extract_mentioned_os_paths};
use crate::tools::GrantedFolder;
use crate::{WEB_SEARCH_TOOL_INSTRUCTIONS, WORKSPACE_TOOLS_INSTRUCTIONS};
use companion_core::attachments::format_attachments_for_prompt;
use companion_core::prompt::compose_personalized_system_prompt;
use companion_core::provider::{ChatRole, ChatTurn};
use companion_core::types::MessageRole;
use companion_storage::AttachmentRecord;
use companion_storage::settings::PersonalizationSettings;

/// UI-agnostic message + attachments item used to construct model prompt turns.
#[derive(Debug, Clone)]
pub struct MessageWithAttachments {
    pub role: MessageRole,
    pub content: String,
    pub attachments: Vec<AttachmentRecord>,
}

/// Pure inputs required to assemble the system prompt and conversation turns.
pub struct PromptContext<'a> {
    pub chat_system_prompt: &'a str,
    pub personalization: &'a PersonalizationSettings,
    pub granted_folders: &'a [GrantedFolder],
    pub web_search_active: bool,
    pub history: &'a [MessageWithAttachments],
}

/// Build the full `[System, ...history]` turn list for inference.
pub fn build_prompt_turns(ctx: &PromptContext<'_>) -> Vec<ChatTurn> {
    let mut sys_prompt = compose_personalized_system_prompt(
        ctx.chat_system_prompt,
        &ctx.personalization.assistant_name,
        ctx.personalization.user_name.as_deref(),
        ctx.personalization.personality,
        ctx.personalization.custom_personality.as_deref(),
        ctx.personalization.user_about.as_deref(),
    );

    append_folder_context(&mut sys_prompt, ctx.granted_folders);
    if ctx.web_search_active {
        sys_prompt.push_str("\n\n");
        sys_prompt.push_str(WEB_SEARCH_TOOL_INSTRUCTIONS);
    }

    let mut turns = vec![ChatTurn {
        role: ChatRole::System,
        content: sys_prompt,
        images: Vec::new(),
    }];

    let start = ctx.history.len().saturating_sub(48);
    for msg in &ctx.history[start..] {
        if let Some(turn) = message_to_chat_turn(msg) {
            turns.push(turn);
        }
    }

    append_mentioned_os_paths(&mut turns);
    turns
}

fn append_folder_context(sys_prompt: &mut String, granted_folders: &[GrantedFolder]) {
    let existing: Vec<&GrantedFolder> =
        granted_folders.iter().filter(|g| g.path.exists()).collect();
    if existing.is_empty() {
        sys_prompt.push_str("\n\nNo folders or files are currently granted for this chat.\n\n");
        sys_prompt.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
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
    sys_prompt.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
}

fn message_to_chat_turn(msg: &MessageWithAttachments) -> Option<ChatTurn> {
    let role = match msg.role {
        MessageRole::User => ChatRole::User,
        MessageRole::System => ChatRole::System,
        MessageRole::Assistant => ChatRole::Assistant,
    };
    let images: Vec<String> = msg
        .attachments
        .iter()
        .filter(|a| a.mime_type.starts_with("image/"))
        .map(|a| a.file_path.clone())
        .collect();
    let text_only: Vec<AttachmentRecord> = msg
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

fn append_mentioned_os_paths(turns: &mut [ChatTurn]) {
    let mut mentioned_dirs: Vec<String> = Vec::new();
    for t in turns.iter() {
        if matches!(t.role, ChatRole::User) {
            for p in extract_mentioned_os_paths(&t.content)
                .into_iter()
                .chain(detect_existing_os_paths_in_text(&t.content))
            {
                let s = p.to_string_lossy().to_string();
                if !mentioned_dirs.contains(&s) {
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
            sys_turn.content.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_core::AssistantPersonality;

    fn sample_personalization() -> PersonalizationSettings {
        PersonalizationSettings {
            assistant_name: "Companion".to_string(),
            user_name: Some("Ada".to_string()),
            personality: AssistantPersonality::Friendly,
            custom_personality: None,
            user_about: None,
        }
    }

    #[test]
    fn builds_prompt_turns_with_folders_web_search_attachments_and_mentions() {
        let dir = std::env::temp_dir().join(format!("companion_pt_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mentioned = dir.join("README.md");
        std::fs::write(&mentioned, "hello").unwrap();

        let prefs = sample_personalization();
        let history = vec![MessageWithAttachments {
            role: MessageRole::User,
            content: format!("Check @{}", mentioned.display()),
            attachments: vec![
                AttachmentRecord {
                    id: "img1".to_string(),
                    message_id: "m1".to_string(),
                    file_name: "photo.png".to_string(),
                    mime_type: "image/png".to_string(),
                    file_path: "/tmp/photo.png".to_string(),
                    extracted_text: None,
                    byte_size: 100,
                },
                AttachmentRecord {
                    id: "txt1".to_string(),
                    message_id: "m1".to_string(),
                    file_name: "notes.txt".to_string(),
                    mime_type: "text/plain".to_string(),
                    file_path: "/tmp/notes.txt".to_string(),
                    extracted_text: Some("note body".to_string()),
                    byte_size: 9,
                },
            ],
        }];

        let turns = build_prompt_turns(&PromptContext {
            chat_system_prompt: "Be helpful.",
            personalization: &prefs,
            granted_folders: &[GrantedFolder {
                path: dir.clone(),
                allow_write: true,
            }],
            web_search_active: true,
            history: &history,
        });

        assert_eq!(turns.len(), 2);
        let sys = &turns[0].content;
        assert!(sys.contains("[directory, read & write]"));
        assert!(sys.contains("Web Search is enabled"));
        assert!(sys.contains("User-mentioned OS paths"));
        assert_eq!(turns[1].images, vec!["/tmp/photo.png".to_string()]);
        assert!(turns[1].content.contains("note body"));

        let empty_turns = build_prompt_turns(&PromptContext {
            chat_system_prompt: "",
            personalization: &prefs,
            granted_folders: &[],
            web_search_active: false,
            history: &[],
        });
        assert!(
            empty_turns[0]
                .content
                .contains("No folders or files are currently granted")
        );
        assert!(!empty_turns[0].content.contains("Web Search is enabled"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
