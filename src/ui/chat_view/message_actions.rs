//! Conversation loading, message edit/delete, folder granting, and stream cancellation helpers.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::core::types::{FolderPermission, MessageRole};
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::{gettext, tr_f};
use crate::storage::{AttachmentRecord, ModelRecord, NewMessage};
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;
use crate::ui::permission_dialog::{FolderDropGrantChoice, present_folder_drop_confirm_dialog};

use super::agent_runner::ActiveChatRun;
use super::page_loader::{load_and_parse_page, scroll_scroller_to_bottom};
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

impl ChatViewModel {
    pub(super) fn handle_grant_folder(&self, path: PathBuf, sender: &ComponentSender<Self>) {
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };
        let folder_str = path.to_string_lossy().to_string();
        let folder_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&folder_str)
            .to_string();
        let existing = self.db.list_conversation_folders(&conv_id).unwrap_or_default();
        if existing.iter().any(|f| f.folder_path == folder_str) {
            let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                "Folder already granted: {folder}",
                &[("folder", &folder_name)],
            )));
            return;
        }
        let db = self.db.clone();
        let sender_clone = sender.clone();
        let folder_name_cb = folder_name.clone();
        let folder_str_cb = folder_str.clone();
        present_folder_drop_confirm_dialog(
            &self.scrolled_window,
            &folder_name,
            &folder_str,
            move |choice| {
                let allow_write = match choice {
                    FolderDropGrantChoice::Cancel => return,
                    FolderDropGrantChoice::ReadOnly => false,
                    FolderDropGrantChoice::ReadWrite => true,
                };
                match db.add_conversation_folder(&conv_id, &folder_str_cb) {
                    Ok(entry) => {
                        if allow_write {
                            let _ = db.set_conversation_folder_permission(
                                &entry.id,
                                FolderPermission::ReadWrite,
                            );
                        }
                        sender_clone.input(ChatViewMsg::RefreshGrantedFolders);
                        let msg = if allow_write {
                            tr_f(
                                "Granted read & write access to {folder}",
                                &[("folder", &folder_name_cb)],
                            )
                        } else {
                            tr_f(
                                "Granted read-only access to {folder}",
                                &[("folder", &folder_name_cb)],
                            )
                        };
                        let _ = sender_clone.output(ChatViewOutput::ShowToast(msg));
                    }
                    Err(err) => {
                        let _ = sender_clone.output(ChatViewOutput::ShowToast(tr_f(
                            "Failed to grant folder: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                }
            },
        );
    }

    pub(super) fn handle_send_message(
        &mut self,
        text: String,
        thinking_level: ThinkingLevel,
        attachments: Vec<AttachmentRecord>,
        sender: &ComponentSender<Self>,
    ) {
        self.active_thinking_level = thinking_level;
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };
        let Some(model_record) = self.resolve_active_model(sender) else {
            return;
        };

        match self.db.insert_message(NewMessage {
            conversation_id: &conv_id,
            role: MessageRole::User,
            content: &text,
            reasoning_content: None,
            thinking_level: Some(thinking_level),
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        }) {
            Ok(user_msg) => {
                if let Err(err) = self.db.insert_attachments(&user_msg.id, &attachments) {
                    tracing::warn!("Failed to persist attachments: {err}");
                }
                self.store.append(&MessageObject::from_message_with_extras(
                    &user_msg,
                    attachments,
                    Vec::new(),
                ));
                self.is_empty = false;
                self.scroll_to_bottom();
                self.start_llama_stream(&conv_id, &model_record, thinking_level, sender);
            }
            Err(err) => {
                let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                    "Failed to save message: {err}",
                    &[("err", &err.to_string())],
                )));
            }
        }
    }

    pub(super) fn handle_edit_and_resend(
        &mut self,
        message_id: String,
        new_content: String,
        sender: &ComponentSender<Self>,
    ) {
        self.cancel_active_stream(sender);
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };
        let Some(model_record) = self.resolve_active_model(sender) else {
            return;
        };

        let mut found: Option<(u32, MessageObject, i64)> = None;
        for idx in 0..self.store.n_items() {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                let entry = obj.entry();
                if entry.id == message_id {
                    found = Some((idx, obj, entry.created_at));
                    break;
                }
            }
        }
        let Some((pos, msg_obj, created_at)) = found else {
            return;
        };

        if let Err(err) = self.db.edit_message_and_truncate_after(
            &conv_id,
            &message_id,
            created_at,
            &new_content,
        ) {
            let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                "Failed to update message: {err}",
                &[("err", &err.to_string())],
            )));
            return;
        }

        let total = self.store.n_items();
        if pos + 1 < total {
            self.store
                .splice(pos + 1, total - (pos + 1), &[] as &[MessageObject]);
        }
        msg_obj.set_user_content(&new_content);
        self.scroll_to_bottom();
        self.start_llama_stream(&conv_id, &model_record, self.active_thinking_level, sender);
    }

    pub(super) fn handle_delete_from(
        &mut self,
        message_id: String,
        sender: &ComponentSender<Self>,
    ) {
        self.cancel_active_stream(sender);
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };

        let mut found: Option<(u32, i64)> = None;
        for idx in 0..self.store.n_items() {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                let entry = obj.entry();
                if entry.id == message_id {
                    found = Some((idx, entry.created_at));
                    break;
                }
            }
        }
        let Some((pos, created_at)) = found else {
            return;
        };

        if let Err(err) = self
            .db
            .delete_message_and_after(&conv_id, &message_id, created_at)
        {
            let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                "Failed to delete message: {err}",
                &[("err", &err.to_string())],
            )));
            return;
        }

        let total = self.store.n_items();
        if pos < total {
            self.store
                .splice(pos, total - pos, &[] as &[MessageObject]);
        }
        self.is_empty = self.store.n_items() == 0;
    }

    pub(super) fn load_older_page(&mut self, sender: &ComponentSender<Self>) {
        if !self.has_more_pages || self.is_loading_older {
            return;
        }
        let Some(conv_id) = self.active_conversation_id.clone() else {
            return;
        };
        let Some(cursor) = self.oldest_loaded_ts else {
            return;
        };

        self.is_loading_older = true;
        let db = self.db.clone();
        let req_seq = self.load_seq;
        let input_sender = sender.input_sender().clone();

        runtime().spawn_blocking(move || {
            let result = load_and_parse_page(&db, &conv_id, Some(cursor), 0);
            let _ = input_sender.send(ChatViewMsg::OlderPageLoaded {
                conversation_id: conv_id,
                request_seq: req_seq,
                result,
            });
        });
    }

    pub(super) fn resolve_active_model(&self, sender: &ComponentSender<Self>) -> Option<ModelRecord> {
        let mut model_record = self
            .active_model_id
            .as_deref()
            .and_then(|id| self.db.get_model(id).ok().flatten())
            .filter(|m| {
                m.file_path
                    .as_deref()
                    .is_some_and(|p| std::path::Path::new(p).exists())
            });
        if let Some(ref mut rec) = model_record
            && !rec.supports_thinking
            && let Some(path) = rec.file_path.as_deref()
            && crate::models::detect_gguf_supports_thinking(std::path::Path::new(path))
        {
            rec.supports_thinking = true;
            let _ = self.db.upsert_model(rec);
        }
        if model_record.is_none() {
            let _ = sender.output(ChatViewOutput::ShowToast(gettext(
                "AI model not found — download or select one in Settings → AI Models",
            )));
            let _ = sender.output(ChatViewOutput::OpenModelSettings);
        }
        model_record
    }

    pub(super) fn empty_description(&self) -> String {
        let name = self.assistant_name.borrow();
        tr_f(
            "Ask {name} anything, or share a file or image below.",
            &[("name", &name)],
        )
    }

    pub(super) fn scroll_to_bottom(&self) {
        let count = self.store.n_items();
        if count == 0 {
            return;
        }
        let last_idx = count - 1;
        let scroller = self.scrolled_window.clone();
        let list = self.message_list.clone();
        glib::idle_add_local_once(move || {
            list.scroll_to(last_idx, gtk::ListScrollFlags::NONE, None);
            scroll_scroller_to_bottom(&scroller);
            let scroller2 = scroller.clone();
            let list2 = list.clone();
            glib::timeout_add_local_once(Duration::from_millis(60), move || {
                list2.scroll_to(last_idx, gtk::ListScrollFlags::NONE, None);
                scroll_scroller_to_bottom(&scroller2);
            });
        });
    }

    pub(super) fn cancel_active_stream(&mut self, sender: &ComponentSender<Self>) {
        if let Some(conv_id) = self.active_conversation_id.clone() {
            self.cancel_run_for(&conv_id, sender);
        }
        self.composer.emit(ComposerMsg::SetGenerating(false));
    }

    pub(super) fn cancel_run_for(&mut self, conv_id: &str, sender: &ComponentSender<Self>) {
        if let Some(run) = self.active_runs.borrow_mut().remove(conv_id) {
            run.cancel_flag.store(true, Ordering::Relaxed);
            let entry = run.stream_obj.entry();
            run.stream_obj
                .update_streaming(&entry.content, entry.reasoning_content.as_deref(), false);
            let _ = sender.output(ChatViewOutput::ChatRunningChanged {
                conversation_id: conv_id.to_string(),
                is_running: false,
            });
        }
    }

    pub(super) fn cancel_all_runs(&mut self, sender: &ComponentSender<Self>) {
        let runs: Vec<(String, ActiveChatRun)> = self.active_runs.borrow_mut().drain().collect();
        for (conv_id, run) in runs {
            run.cancel_flag.store(true, Ordering::Relaxed);
            let entry = run.stream_obj.entry();
            run.stream_obj
                .update_streaming(&entry.content, entry.reasoning_content.as_deref(), false);
            let _ = sender.output(ChatViewOutput::ChatRunningChanged {
                conversation_id: conv_id,
                is_running: false,
            });
        }
        self.composer.emit(ComposerMsg::SetGenerating(false));
    }
}
