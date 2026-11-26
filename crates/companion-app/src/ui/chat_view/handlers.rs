//! Conversation loading, page application, and drag-and-drop overlay helpers for [`ChatViewModel`].

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio};
use relm4::prelude::*;

use crate::core::prompt::default_system_prompt;
use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::page_loader::{LoadedPageData, load_and_parse_page};
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

impl ChatViewModel {
    pub(super) fn on_load_conversation(
        &mut self,
        conversation_id: String,
        title: String,
        sender: &ComponentSender<Self>,
    ) {
        self.load_seq = self.load_seq.wrapping_add(1);
        let req_seq = self.load_seq;
        self.active_conversation_id = Some(conversation_id.clone());
        *self.visible_conv_id.borrow_mut() = Some(conversation_id.clone());
        let is_running = self.active_runs.borrow().contains_key(&conversation_id);
        self.composer.emit(ComposerMsg::SetGenerating(is_running));
        self.active_title = title;
        self.store.remove_all();
        self.oldest_loaded_ts = None;
        self.has_more_pages = false;
        self.is_loading_older = false;
        self.is_loading = true;
        self.is_empty = false;

        let db = self.db.clone();
        let input_sender = sender.input_sender().clone();
        runtime().spawn_blocking(move || {
            let result = load_and_parse_page(&db, &conversation_id, None, 0);
            let _ = input_sender.send(ChatViewMsg::ConversationPageLoaded {
                conversation_id,
                request_seq: req_seq,
                result,
            });
        });
    }

    pub(super) fn on_page_loaded(
        &mut self,
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
        sender: &ComponentSender<Self>,
    ) {
        if self.load_seq != request_seq
            || self.active_conversation_id.as_deref() != Some(conversation_id.as_str())
        {
            return;
        }
        self.is_loading = false;
        match result {
            Ok(data) => {
                if let Some(loaded_conv) = data.conversation {
                    self.active_system_prompt = loaded_conv
                        .system_prompt
                        .unwrap_or_else(|| default_system_prompt().to_string());
                    self.active_thinking_level = loaded_conv.thinking_level;
                    self.web_search_enabled = loaded_conv.web_search_enabled;
                    self.composer.emit(ComposerMsg::SetSystemPrompt(
                        self.active_system_prompt.clone(),
                    ));
                    self.composer
                        .emit(ComposerMsg::SetThinkingLevel(self.active_thinking_level));
                    self.composer
                        .emit(ComposerMsg::SetWebSearchEnabled(self.web_search_enabled));
                }
                if let Some(granted) = data.granted_folders {
                    self.composer.emit(ComposerMsg::SetGrantedFolders(granted));
                }
                self.has_more_pages = data.has_more_pages;
                self.oldest_loaded_ts = data.oldest_loaded_ts;
                let objs: Vec<MessageObject> = data
                    .items
                    .into_iter()
                    .map(|(m, atts, tcs, blocks)| {
                        MessageObject::from_message_with_blocks(&m, atts, tcs, blocks)
                    })
                    .collect();
                self.store.splice(0, self.store.n_items(), &objs);
                if let Some(run) = self.active_runs.borrow().get(&conversation_id) {
                    self.store.append(&run.stream_obj);
                }
                self.composer.emit(ComposerMsg::SetContextUsed(data.est_tokens));
                self.is_empty = self.store.n_items() == 0;
                self.scroll_to_bottom();
            }
            Err(err) => {
                self.is_empty = true;
                let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                    "Failed to load messages: {err}",
                    &[("err", &err)],
                )));
            }
        }
    }

    pub(super) fn on_clear_conversation(&mut self, sender: &ComponentSender<Self>) {
        self.cancel_active_stream(sender);
        self.active_conversation_id = None;
        *self.visible_conv_id.borrow_mut() = None;
        self.store.remove_all();
        self.is_loading = false;
        self.is_empty = true;
        self.web_search_enabled = false;
        self.composer.emit(ComposerMsg::SetWebSearchEnabled(false));
    }

    pub(super) fn on_older_page_loaded(
        &mut self,
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
    ) {
        if self.load_seq != request_seq
            || self.active_conversation_id.as_deref() != Some(conversation_id.as_str())
        {
            return;
        }
        self.is_loading = false;
        self.is_loading_older = false;
        if let Ok(data) = result {
            if data.items.is_empty() {
                self.has_more_pages = false;
                return;
            }
            self.has_more_pages = data.has_more_pages;
            self.oldest_loaded_ts = data.oldest_loaded_ts;
            let objects: Vec<MessageObject> = data
                .items
                .into_iter()
                .map(|(m, atts, tcs, blocks)| {
                    MessageObject::from_message_with_blocks(&m, atts, tcs, blocks)
                })
                .collect();
            self.store.splice(0, 0, &objects);
        }
    }

    pub(super) fn on_open_folder_permissions(&self, sender: &ComponentSender<Self>) {
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };
        let sender_clone = sender.clone();
        crate::ui::folder_permissions_dialog::present_folder_permissions_dialog(
            &self.scrolled_window,
            self.db.clone(),
            conv_id,
            move || {
                sender_clone.input(ChatViewMsg::RefreshGrantedFolders);
            },
        );
    }

    pub(super) fn on_refresh_models(&mut self, id: Option<String>, sender: &ComponentSender<Self>) {
        let model_changed = self.active_model_id != id;
        self.active_model_id = id.clone();
        if model_changed && !self.active_runs.borrow().is_empty() {
            self.cancel_all_runs(sender);
            self.provider.unload();
            let _ = sender.output(ChatViewOutput::ShowToast(gettext(
                "Stopped active chats to switch AI model",
            )));
        }
        self.composer.emit(ComposerMsg::RefreshModels(id));
    }

    pub(super) fn on_personalization_changed(
        &mut self,
        assistant_name: String,
        user_name: Option<String>,
    ) {
        self.composer
            .emit(ComposerMsg::SetAssistantName(assistant_name.clone()));
        *self.assistant_name.borrow_mut() = assistant_name;
        *self.user_name.borrow_mut() = user_name;
        for idx in 0..self.store.n_items() {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                obj.emit_updated();
            }
        }
    }
}

pub(super) fn attach_drop_overlay(
    root_overlay: &gtk::Overlay,
    drop_overlay: &gtk::Box,
    composer_sender: &relm4::Sender<ComposerMsg>,
) {
    let drop_target = gtk::DropTarget::builder()
        .actions(gdk::DragAction::COPY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

    {
        let overlay = drop_overlay.clone();
        drop_target.connect_enter(move |_, _, _| {
            overlay.set_visible(true);
            gdk::DragAction::COPY
        });
    }
    {
        let overlay = drop_overlay.clone();
        drop_target.connect_leave(move |_| {
            overlay.set_visible(false);
        });
    }
    {
        let overlay = drop_overlay.clone();
        let composer_sender = composer_sender.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            overlay.set_visible(false);
            let mut paths: Vec<PathBuf> = Vec::new();
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                paths.extend(file_list.files().into_iter().filter_map(|f| f.path()));
            } else if let Ok(file) = value.get::<gio::File>()
                && let Some(p) = file.path()
            {
                paths.push(p);
            }

            if !paths.is_empty() {
                let _ = composer_sender.send(ComposerMsg::FilesDropped(paths));
                true
            } else {
                false
            }
        });
    }
    root_overlay.add_controller(drop_target);
}
