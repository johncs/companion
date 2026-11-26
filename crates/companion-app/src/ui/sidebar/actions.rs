use relm4::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::storage::Conversation;
use crate::ui::sidebar_rows::build_conversation_row;
use crate::ui::widgets::clear_list_box;

use super::widgets::build_sidebar_error_row;
use super::{SidebarModel, SidebarMsg, SidebarOutput};

impl SidebarModel {
    pub(super) fn on_new_chat(&self, sender: &ComponentSender<Self>) {
        let title = gettext("New Chat");
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let thinking_level = db
                .get_active_model_id()
                .ok()
                .flatten()
                .and_then(|id| db.get_model(&id).ok().flatten())
                .map(|m| m.default_thinking_level())
                .unwrap_or_default();
            let new_conv = crate::storage::NewConversation {
                title: &title,
                system_prompt: Some(crate::core::prompt::default_system_prompt().to_string()),
                thinking_level,
                ..Default::default()
            };
            let res = db.create_conversation(new_conv).map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::ChatCreated(res));
        });
    }

    pub(super) fn on_chat_created(
        &mut self,
        res: Result<Conversation, String>,
        sender: &ComponentSender<Self>,
    ) {
        match res {
            Ok(conv) => {
                self.active_conversation_id = Some(conv.id.clone());
                self.rebuild_lists(sender);
                let _ = sender.output(SidebarOutput::ConversationSelected(conv.id));
            }
            Err(err) => {
                let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                    "Failed to create chat: {err}",
                    &[("err", &err)],
                )));
            }
        }
    }

    pub(super) fn on_rename_conversation(
        &self,
        id: String,
        title: String,
        sender: &ComponentSender<Self>,
    ) {
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let result = db
                .rename_conversation(&id, &title)
                .map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::ChatRenamed { id, title, result });
        });
    }

    pub(super) fn on_chat_renamed(
        &mut self,
        id: String,
        title: String,
        result: Result<(), String>,
        sender: &ComponentSender<Self>,
    ) {
        if let Err(err) = result {
            let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                "Failed to rename chat: {err}",
                &[("err", &err)],
            )));
        } else {
            self.rebuild_lists(sender);
            let _ = sender.output(SidebarOutput::ConversationRenamed { id, title });
        }
    }

    pub(super) fn on_duplicate_conversation(
        &self,
        conv_id: String,
        sender: &ComponentSender<Self>,
    ) {
        let db = self.services.db.clone();
        let copy_label = gettext("Copy");
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let res = (|| -> anyhow::Result<Option<Conversation>> {
                let Some(source) = db.get_conversation(&conv_id)? else {
                    return Ok(None);
                };
                let copy_title = format!("{} ({})", source.title, copy_label);
                db.duplicate_conversation(&conv_id, &copy_title)
            })()
            .map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::ChatDuplicated(res));
        });
    }

    pub(super) fn on_chat_duplicated(
        &mut self,
        res: Result<Option<Conversation>, String>,
        sender: &ComponentSender<Self>,
    ) {
        match res {
            Ok(Some(copy)) => {
                self.active_conversation_id = Some(copy.id.clone());
                self.rebuild_lists(sender);
                let _ = sender.output(SidebarOutput::ConversationSelected(copy.id));
            }
            Ok(None) => {}
            Err(err) => {
                let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                    "Failed to duplicate chat: {err}",
                    &[("err", &err)],
                )));
            }
        }
    }

    pub(super) fn on_delete_conversation(&self, conv_id: String, sender: &ComponentSender<Self>) {
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let result = db.delete_conversation(&conv_id).map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::ChatDeleted {
                id: conv_id,
                result,
            });
        });
    }

    pub(super) fn on_chat_deleted(
        &mut self,
        id: String,
        result: Result<(), String>,
        sender: &ComponentSender<Self>,
    ) {
        if let Err(err) = result {
            let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                "Failed to delete chat: {err}",
                &[("err", &err)],
            )));
        } else {
            self.vm.set_running(id.clone(), false);
            if self.active_conversation_id.as_deref() == Some(&id) {
                self.active_conversation_id = None;
            }
            self.rebuild_lists(sender);
            let _ = sender.output(SidebarOutput::ConversationDeleted(id));
        }
    }

    pub(super) fn on_chats_loaded(
        &mut self,
        request_seq: u64,
        result: Result<Vec<Conversation>, String>,
        sender: &ComponentSender<Self>,
    ) {
        if self.list_seq != request_seq {
            return;
        }
        clear_list_box(&self.chats_list);
        match result {
            Ok(convs) => {
                for conv in convs {
                    let is_active = self.active_conversation_id.as_deref() == Some(&conv.id);
                    let is_running = self.vm.running_conversations.contains(&conv.id);
                    self.chats_list.append(&build_conversation_row(
                        &conv, is_active, is_running, sender,
                    ));
                }
            }
            Err(err) => {
                tracing::error!("Failed to list chats: {err}");
                let sender_clone = sender.clone();
                self.chats_list.append(&build_sidebar_error_row(
                    &gettext("Failed to load chats"),
                    move || sender_clone.input(SidebarMsg::RefreshLists),
                ));
            }
        }
    }
}
