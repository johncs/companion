//! Conversation loading, message edit/delete, folder granting, and stream cancellation helpers.

use std::sync::atomic::Ordering;

use adw::prelude::*;
use relm4::prelude::*;

use crate::agent::prompt::MessageWithAttachments;
use crate::core::types::MessageRole;
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::{gettext, tr_f};
use crate::storage::entities::Message;
use crate::storage::{AttachmentRecord, NewMessage};
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::agent_runner::{ActiveChatRun, PreparedPrompt, StreamLaunch, prepare_prompt_blocking};
use super::page_loader::load_and_parse_page;
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

#[derive(Debug)]
pub struct EditPreparedPayload {
    pub(super) conv_id: String,
    pub(super) pos: u32,
    pub(super) message_id: String,
    pub(super) new_content: String,
    pub(super) thinking_level: ThinkingLevel,
    pub(super) result: Result<PreparedPrompt, String>,
}

impl ChatViewModel {
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
        if self.active_model_id.is_none() {
            let _ = sender.output(ChatViewOutput::ShowToast(gettext(
                "No model selected or model file is missing. Choose a model in Settings.",
            )));
            return;
        }

        let mut history = self.capture_history_snapshot(None);
        history.push(MessageWithAttachments {
            role: MessageRole::User,
            content: text.clone(),
            attachments: attachments.clone(),
        });
        let prep_input = self.build_prompt_prep_input(conv_id.clone(), history);
        let db = self.db.clone();
        let input_sender = sender.input_sender().clone();

        runtime().spawn_blocking(move || {
            let result =
                (|| -> Result<(Message, Vec<AttachmentRecord>, PreparedPrompt), String> {
                    let prepared = prepare_prompt_blocking(&prep_input)?;
                    let user_msg = db
                        .insert_message(NewMessage {
                            conversation_id: &conv_id,
                            role: MessageRole::User,
                            content: &text,
                            reasoning_content: None,
                            thinking_level: Some(thinking_level),
                            model_name: None,
                            execution_device: None,
                            token_count: 0,
                            generation_ms: None,
                        })
                        .map_err(|e| {
                            tr_f("Failed to save message: {err}", &[("err", &e.to_string())])
                        })?;
                    if let Err(err) = db.insert_attachments(&user_msg.id, &attachments) {
                        tracing::warn!("Failed to persist attachments: {err}");
                    }
                    Ok((user_msg, attachments, prepared))
                })();
            let _ = input_sender.send(ChatViewMsg::SendPrepared {
                conv_id: prep_input.conv_id,
                thinking_level,
                result,
            });
        });
    }

    pub(super) fn handle_send_prepared(
        &mut self,
        conv_id: String,
        thinking_level: ThinkingLevel,
        result: Result<(Message, Vec<AttachmentRecord>, PreparedPrompt), String>,
        sender: &ComponentSender<Self>,
    ) {
        match result {
            Ok((user_msg, attachments, prepared)) => {
                if self.active_conversation_id.as_deref() == Some(conv_id.as_str()) {
                    self.store.append(&MessageObject::from_message_with_extras(
                        &user_msg,
                        attachments,
                        Vec::new(),
                    ));
                    self.is_empty = false;
                    self.scroll_to_bottom();
                }
                self.start_llama_stream(
                    StreamLaunch {
                        conv_id,
                        model_record: prepared.model_record,
                        thinking_level,
                        granted_folders: prepared.granted_folders,
                        turns: prepared.turns,
                        notices: prepared.notices,
                    },
                    sender,
                );
            }
            Err(err) => {
                let _ = sender.output(ChatViewOutput::ShowToast(err));
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
        if self.active_model_id.is_none() {
            let _ = sender.output(ChatViewOutput::ShowToast(gettext(
                "No model selected or model file is missing. Choose a model in Settings.",
            )));
            return;
        }

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

        let mut history = self.capture_history_snapshot(Some(pos + 1));
        if let Some(last) = history.last_mut() {
            last.content = new_content.clone();
        }
        let prep_input = self.build_prompt_prep_input(conv_id.clone(), history);
        let thinking_level = self.active_thinking_level;
        let db = self.db.clone();
        let input_sender = sender.input_sender().clone();
        let new_content_bg = new_content.clone();
        let message_id_bg = message_id.clone();

        runtime().spawn_blocking(move || {
            let result = (|| -> Result<PreparedPrompt, String> {
                let prepared = prepare_prompt_blocking(&prep_input)?;
                db.edit_message_and_truncate_after(
                    &conv_id,
                    &message_id_bg,
                    created_at,
                    &new_content_bg,
                )
                .map_err(|e| {
                    tr_f(
                        "Failed to update message: {err}",
                        &[("err", &e.to_string())],
                    )
                })?;
                Ok(prepared)
            })();
            let _ = input_sender.send(ChatViewMsg::EditPrepared(EditPreparedPayload {
                conv_id: prep_input.conv_id,
                pos,
                message_id,
                new_content,
                thinking_level,
                result,
            }));
        });
    }

    pub(super) fn handle_edit_prepared(
        &mut self,
        payload: EditPreparedPayload,
        sender: &ComponentSender<Self>,
    ) {
        let EditPreparedPayload {
            conv_id,
            pos,
            message_id,
            new_content,
            thinking_level,
            result,
        } = payload;
        match result {
            Ok(prepared) => {
                if self.active_conversation_id.as_deref() == Some(conv_id.as_str()) {
                    let total = self.store.n_items();
                    if pos + 1 < total {
                        self.store
                            .splice(pos + 1, total - (pos + 1), &[] as &[MessageObject]);
                    }
                    if let Some(msg_obj) = self.store.item(pos).and_downcast::<MessageObject>()
                        && msg_obj.with_entry(|e| e.id == message_id)
                    {
                        msg_obj.set_user_content(&new_content);
                    }
                    self.scroll_to_bottom();
                }
                self.start_llama_stream(
                    StreamLaunch {
                        conv_id,
                        model_record: prepared.model_record,
                        thinking_level,
                        granted_folders: prepared.granted_folders,
                        turns: prepared.turns,
                        notices: prepared.notices,
                    },
                    sender,
                );
            }
            Err(err) => {
                let _ = sender.output(ChatViewOutput::ShowToast(err));
            }
        }
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

        let db = self.db.clone();
        let input_sender = sender.input_sender().clone();
        runtime().spawn_blocking(move || {
            let result = db
                .delete_message_and_after(&conv_id, &message_id, created_at)
                .map_err(|e| {
                    tr_f(
                        "Failed to delete message: {err}",
                        &[("err", &e.to_string())],
                    )
                });
            let _ = input_sender.send(ChatViewMsg::DeleteCompleted {
                conv_id,
                pos,
                result,
            });
        });
    }

    pub(super) fn handle_delete_completed(
        &mut self,
        conv_id: String,
        pos: u32,
        result: Result<(), String>,
        sender: &ComponentSender<Self>,
    ) {
        match result {
            Ok(()) => {
                if self.active_conversation_id.as_deref() == Some(conv_id.as_str()) {
                    let total = self.store.n_items();
                    if pos < total {
                        self.store.splice(pos, total - pos, &[] as &[MessageObject]);
                    }
                    self.is_empty = self.store.n_items() == 0;
                }
            }
            Err(err) => {
                let _ = sender.output(ChatViewOutput::ShowToast(err));
            }
        }
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
            run.stream_obj.update_streaming(
                &entry.content,
                entry.reasoning_content.as_deref(),
                false,
            );
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
            run.stream_obj.update_streaming(
                &entry.content,
                entry.reasoning_content.as_deref(),
                false,
            );
            let _ = sender.output(ChatViewOutput::ChatRunningChanged {
                conversation_id: conv_id,
                is_running: false,
            });
        }
        self.composer.emit(ComposerMsg::SetGenerating(false));
    }
}
