//! System prompt personalization, granted-folder context, and chat turn construction.

use std::path::Path;

use adw::prelude::*;

use crate::agent::{WEB_SEARCH_TOOL_INSTRUCTIONS, WORKSPACE_TOOLS_INSTRUCTIONS};
use crate::core::attachments::format_attachments_for_prompt;
use crate::inference::provider::{ChatRole, ChatTurn};
use crate::storage::conversations::compose_personalized_system_prompt;
use crate::ui::message_object::MessageObject;

use super::ChatViewModel;
use super::path_mentions::{detect_existing_os_paths_in_text, extract_mentioned_os_paths};

impl ChatViewModel {
    pub(super) fn build_prompt_turns(&self) -> Vec<ChatTurn> {
        let mut turns = Vec::new();
        let assistant = self.assistant_name.borrow().clone();
        let user_name = self.db.get_user_name().ok().flatten();
        let personality = self.db.get_assistant_personality().unwrap_or_default();
        let custom_personality = self.db.get_custom_personality().ok().flatten();
        let user_about = self.db.get_user_about().ok().flatten();
        let mut sys_prompt = compose_personalized_system_prompt(
            &self.active_system_prompt,
            &assistant,
            user_name.as_deref(),
            personality,
            custom_personality.as_deref(),
            user_about.as_deref(),
        );

        if let Some(conv_id) = &self.active_conversation_id {
            let granted: Vec<(String, String)> = self
                .db
                .list_conversation_folders(conv_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|f| Path::new(&f.folder_path).exists())
                .map(|f| (f.folder_path, f.permission_mode))
                .collect();
            if !granted.is_empty() {
                sys_prompt.push_str("\n\nGranted folders & files:\n");
                for (path, mode) in &granted {
                    let kind = if Path::new(path).is_file() {
                        "file"
                    } else {
                        "directory"
                    };
                    let perm = if mode == "read_write" {
                        "read & write"
                    } else {
                        "read-only"
                    };
                    sys_prompt.push_str(&format!("- {path} [{kind}, {perm}]\n"));
                }
                sys_prompt.push('\n');
                sys_prompt.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
            } else {
                sys_prompt.push_str(
                    "\n\nNo folders or files are currently granted for this chat.\n\n",
                );
                sys_prompt.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
            }
            if self.web_search_enabled
                && self.db.get_web_search_global_enabled().unwrap_or(false)
            {
                sys_prompt.push_str("\n\n");
                sys_prompt.push_str(WEB_SEARCH_TOOL_INSTRUCTIONS);
            }
        }
        turns.push(ChatTurn {
            role: ChatRole::System,
            content: sys_prompt,
            images: Vec::new(),
        });

        let total = self.store.n_items();
        let start = total.saturating_sub(48);
        for idx in start..total {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                let entry = obj.entry();
                let role = if entry.role == "user" {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                };
                let images: Vec<String> = entry
                    .attachments
                    .iter()
                    .filter(|a| a.mime_type.starts_with("image/"))
                    .map(|a| a.file_path.clone())
                    .collect();
                let text_only_atts: Vec<_> = entry
                    .attachments
                    .iter()
                    .filter(|a| !a.mime_type.starts_with("image/"))
                    .cloned()
                    .collect();
                let att_prefix = format_attachments_for_prompt(&text_only_atts);
                let full_content = if att_prefix.is_empty() {
                    entry.content
                } else {
                    format!("{att_prefix}{}", entry.content)
                };
                if !full_content.is_empty() || !images.is_empty() {
                    turns.push(ChatTurn {
                        role,
                        content: full_content,
                        images,
                    });
                }
            }
        }

        let mut mentioned_dirs: Vec<String> = Vec::new();
        for t in &turns {
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
            sys_turn
                .content
                .push_str("\n\nUser-mentioned OS paths (ask permission via tool call before reading):\n");
            for d in &mentioned_dirs {
                sys_turn.content.push_str(&format!("- {d}\n"));
            }
            if !sys_turn.content.contains("Available tools:") {
                sys_turn.content.push('\n');
                sys_turn.content.push_str(WORKSPACE_TOOLS_INSTRUCTIONS);
            }
        }
        turns
    }
}
