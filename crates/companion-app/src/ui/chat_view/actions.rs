//! Message dispatch and settings persistence helpers for [`ChatViewModel`].

use adw::prelude::*;
use relm4::prelude::*;

use crate::agent::prompt::MessageWithAttachments;
use crate::agent::registry::ToolRegistry;
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::prompt_prep::PromptPrepInput;
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

impl ChatViewModel {
    pub(super) fn handle_nav_or_folder_msg(
        &mut self,
        msg: ChatViewMsg,
        sender: &ComponentSender<Self>,
    ) {
        match msg {
            ChatViewMsg::LoadConversation {
                conversation_id,
                title,
            } => {
                self.on_load_conversation(conversation_id, title, sender);
            }
            ChatViewMsg::ConversationPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => self.on_page_loaded(conversation_id, request_seq, result, sender),
            ChatViewMsg::ClearConversation => self.on_clear_conversation(sender),
            ChatViewMsg::CancelConversation(id) => {
                self.cancel_run_for(&id, sender);
                if self.active_conversation_id.as_deref() == Some(id.as_str()) {
                    self.composer.emit(ComposerMsg::SetGenerating(false));
                }
            }
            ChatViewMsg::LoadOlderPage => self.load_older_page(sender),
            ChatViewMsg::OlderPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => self.on_older_page_loaded(conversation_id, request_seq, result),
            ChatViewMsg::OpenFolderPermissions => self.on_open_folder_permissions(sender),
            ChatViewMsg::GrantFolder(path) => self.handle_grant_folder(path, sender),
            ChatViewMsg::PresentGrantFolderDialog {
                conv_id,
                folder_name,
                folder_str,
            } => {
                self.present_grant_folder_dialog(conv_id, folder_name, folder_str, sender);
            }
            ChatViewMsg::RefreshGrantedFolders => self.on_refresh_granted_folders(sender),
            ChatViewMsg::GrantedFoldersLoaded { conv_id, folders }
                if self.active_conversation_id.as_deref() == Some(conv_id.as_str()) =>
            {
                self.composer.emit(ComposerMsg::SetGrantedFolders(folders));
            }
            _ => {}
        }
    }

    pub(super) fn handle_message_action_msg(
        &mut self,
        msg: ChatViewMsg,
        sender: &ComponentSender<Self>,
    ) {
        match msg {
            ChatViewMsg::SendMessage {
                text,
                thinking_level,
                attachments,
            } => {
                self.handle_send_message(text, thinking_level, attachments, sender);
            }
            ChatViewMsg::SendPrepared {
                conv_id,
                thinking_level,
                result,
            } => {
                self.handle_send_prepared(conv_id, thinking_level, result, sender);
            }
            ChatViewMsg::EditAndResendMessage {
                message_id,
                new_content,
            } => {
                self.handle_edit_and_resend(message_id, new_content, sender);
            }
            ChatViewMsg::EditPrepared(payload) => self.handle_edit_prepared(payload, sender),
            ChatViewMsg::DeleteMessageFrom(id) => self.handle_delete_from(id, sender),
            ChatViewMsg::DeleteCompleted {
                conv_id,
                message_id,
                result,
            } => {
                self.handle_delete_completed(conv_id, message_id, result, sender);
            }
            ChatViewMsg::StopGeneration => self.cancel_active_stream(sender),
            _ => {}
        }
    }

    pub(super) fn handle_setting_or_ui_msg(
        &mut self,
        msg: ChatViewMsg,
        sender: &ComponentSender<Self>,
    ) {
        match msg {
            ChatViewMsg::SystemPromptChanged(prompt) => {
                self.active_system_prompt = prompt.clone();
                self.save_conv_setting(sender, "Failed to save system prompt", move |db, id| {
                    db.set_conversation_system_prompt(id, Some(&prompt))
                });
            }
            ChatViewMsg::ThinkingLevelChanged(level) => {
                self.active_thinking_level = level;
                self.save_conv_setting(sender, "Failed to save thinking level", move |db, id| {
                    db.set_conversation_thinking_level(id, level)
                });
            }
            ChatViewMsg::WebSearchToggled(enabled) => {
                self.web_search_enabled = enabled;
                self.save_conv_setting(
                    sender,
                    "Failed to save web search setting",
                    move |db, id| db.set_conversation_web_search_enabled(id, enabled),
                );
            }
            ChatViewMsg::RefreshWebSearchGlobal(enabled) => {
                self.composer
                    .emit(ComposerMsg::SetWebSearchGlobalEnabled(enabled));
            }
            ChatViewMsg::RefreshModels(id) => self.on_refresh_models(id, sender),
            ChatViewMsg::PersonalizationChanged {
                assistant_name,
                user_name,
            } => self.on_personalization_changed(assistant_name, user_name),
            ChatViewMsg::ConversationTitleUpdated { id, title } => {
                if self.active_conversation_id.as_deref() == Some(&id) {
                    self.active_title = title;
                }
            }
            ChatViewMsg::OpenModelSettings => {
                drop(sender.output(ChatViewOutput::OpenModelSettings))
            }
            ChatViewMsg::ModelContextLengthChanged(n_ctx) => {
                let _ = sender.output(ChatViewOutput::ModelContextLengthChanged(n_ctx));
            }
            ChatViewMsg::ShowToast(msg) => drop(sender.output(ChatViewOutput::ShowToast(msg))),
            _ => {}
        }
    }

    fn on_refresh_granted_folders(&self, sender: &ComponentSender<Self>) {
        if let Some(conv_id) = self.active_conversation_id.clone() {
            let db = self.services.db.clone();
            let input_sender = sender.input_sender().clone();
            self.services.rt_handle.spawn_blocking(move || {
                let folders = db.list_conversation_folders(&conv_id).unwrap_or_default();
                let _ = input_sender.send(ChatViewMsg::GrantedFoldersLoaded { conv_id, folders });
            });
        }
    }

    fn save_conv_setting(
        &self,
        sender: &ComponentSender<Self>,
        err_prefix: &'static str,
        save_fn: impl FnOnce(&crate::storage::Database, &str) -> anyhow::Result<()> + Send + 'static,
    ) {
        if let Some(conv_id) = self.active_conversation_id.clone() {
            let db = self.services.db.clone();
            let out = sender.output_sender().clone();
            self.services.rt_handle.spawn_blocking(move || {
                if let Err(err) = save_fn(&db, &conv_id) {
                    tracing::error!("{err_prefix}: {err}");
                    let _ = out.send(ChatViewOutput::ShowToast(format!("{err_prefix}: {err}")));
                }
            });
        }
    }

    pub(super) fn capture_history_snapshot(
        &self,
        max_idx: Option<u32>,
    ) -> Vec<MessageWithAttachments> {
        let total = max_idx.unwrap_or_else(|| self.store.n_items());
        let start = total.saturating_sub(48);
        let mut history = Vec::with_capacity((total - start) as usize);
        for idx in start..total {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                let entry = obj.entry();
                history.push(MessageWithAttachments {
                    role: entry.role,
                    content: entry.content,
                    attachments: entry.attachments.iter().map(|a| a.to_staged()).collect(),
                });
            }
        }
        history
    }

    pub(super) fn build_prompt_prep_input(
        &self,
        conv_id: String,
        history: Vec<MessageWithAttachments>,
    ) -> PromptPrepInput {
        PromptPrepInput {
            db: self.services.db.clone(),
            conv_id,
            active_model_id: self.active_model_id.clone(),
            chat_system_prompt: self.active_system_prompt.clone(),
            web_search_enabled: self.web_search_enabled,
            fallback_assistant_name: self.assistant_name.borrow().clone(),
            fallback_user_name: self.user_name.borrow().clone(),
            history,
            load_history_from_db: false,
            pending_user_turn: None,
            registry: ToolRegistry::with_builtins(self.services.web_search.clone()),
        }
    }
}
