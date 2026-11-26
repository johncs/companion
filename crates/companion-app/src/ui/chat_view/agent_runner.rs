//! Thin GTK adapter that launches [`AgentRunner`] and applies [`AgentEvent`]s to the UI.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::agent::prompt::{MessageWithAttachments, PromptContext, build_prompt_turns};
use crate::agent::registry::ToolRegistry;
use crate::agent::runner::{AgentEvent, AgentRunConfig, AgentRunner};
use crate::agent::tools::GrantedFolder;
use crate::core::ThinkingLevel;
use crate::core::types::{ExecutionDevice, MessageRole};
use crate::inference::provider::InferenceParams;
use crate::storage::ModelRecord;
use crate::storage::settings::PersonalizationSettings;
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::approval::GtkApprovalHandler;
use super::page_loader::maybe_scroll_bottom;
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

pub(super) struct ActiveChatRun {
    pub(super) stream_obj: MessageObject,
    pub(super) cancel_flag: Arc<AtomicBool>,
}

impl ChatViewModel {
    pub(super) fn start_llama_stream(
        &mut self,
        conv_id: &str,
        model_record: &ModelRecord,
        thinking_level: ThinkingLevel,
        sender: &ComponentSender<Self>,
    ) {
        self.cancel_run_for(conv_id, sender);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.composer.emit(ComposerMsg::SetGenerating(true));

        let base_params = InferenceParams {
            n_ctx: model_record.default_params.n_ctx,
            n_gpu_layers: model_record.default_params.n_gpu_layers,
            max_tokens: model_record
                .default_params
                .max_tokens
                .max(model_record.default_params.n_ctx),
            temperature: model_record.default_params.temperature,
            thinking_level,
            mmproj_path: model_record.mmproj_path.clone(),
            ..InferenceParams::default()
        };
        let execution_device = if base_params.n_gpu_layers > 0 {
            ExecutionDevice::Gpu
        } else {
            ExecutionDevice::Cpu
        };
        let model_display_name = crate::models::friendly_model_name(&model_record.display_name);
        let stream_obj = MessageObject::new_streaming(
            uuid::Uuid::new_v4().to_string(),
            Some(thinking_level.as_str().to_string()),
            Some(model_display_name.clone()),
            Some(execution_device.as_str().to_string()),
            chrono::Utc::now().timestamp_millis(),
        );
        self.store.append(&stream_obj);
        self.active_runs.borrow_mut().insert(
            conv_id.to_string(),
            ActiveChatRun {
                stream_obj: stream_obj.clone(),
                cancel_flag: cancel_flag.clone(),
            },
        );
        let _ = sender.output(ChatViewOutput::ChatRunningChanged {
            conversation_id: conv_id.to_string(),
            is_running: true,
        });
        self.scroll_to_bottom();

        let granted_folders = self.collect_granted_folders(conv_id, sender);
        let turns = self.assemble_turns(&granted_folders);

        let runner = AgentRunner {
            provider: Arc::new(self.provider.clone()),
            registry: ToolRegistry::with_builtins(),
            permissions: self.permissions.clone(),
            approver: Arc::new(GtkApprovalHandler::new(&self.scrolled_window)),
            db: self.db.clone(),
        };
        let mut rx = runner.spawn(AgentRunConfig {
            conv_id: conv_id.to_string(),
            model_id: model_record.id.clone(),
            model_path: model_record.file_path.clone().unwrap_or_default(),
            model_display_name,
            base_params,
            thinking_level,
            execution_device,
            web_search_enabled: self.web_search_enabled,
            granted_folders,
            turns,
            cancel_flag: cancel_flag.clone(),
        });

        let conv_id = conv_id.to_string();
        let composer_sender = self.composer.sender().clone();
        let input_sender = sender.input_sender().clone();
        let out_sender = sender.output_sender().clone();
        let scroller = self.scrolled_window.clone();
        let store = self.store.clone();
        let active_runs = self.active_runs.clone();
        let visible_conv_id = self.visible_conv_id.clone();

        glib::MainContext::default().spawn_local(async move {
            while let Some(ev) = rx.recv().await {
                let is_visible = visible_conv_id.borrow().as_deref() == Some(conv_id.as_str());
                match ev {
                    AgentEvent::Status(s) => stream_obj.set_status(Some(&s)),
                    AgentEvent::StreamSnapshot {
                        combined_answer,
                        blocks,
                        reasoning,
                    } => {
                        stream_obj.apply_streaming_snapshot(combined_answer, blocks, reasoning, true);
                        if is_visible {
                            maybe_scroll_bottom(&scroller);
                        }
                    }
                    AgentEvent::ToolCallRecorded(tc) => stream_obj.append_tool_call(tc),
                    AgentEvent::FolderGrantsUpdated => {
                        if is_visible {
                            let _ = input_sender.send(ChatViewMsg::RefreshGrantedFolders);
                        }
                    }
                    AgentEvent::ContextUsage { total_tokens } => {
                        if is_visible {
                            let _ = composer_sender.send(ComposerMsg::SetContextUsed(total_tokens));
                        }
                    }
                    AgentEvent::Notice(msg) | AgentEvent::Error(msg) => {
                        let _ = out_sender.send(ChatViewOutput::ShowToast(msg));
                    }
                    AgentEvent::Finished {
                        total_tokens,
                        elapsed_ms,
                        empty,
                        ..
                    } => {
                        active_runs.borrow_mut().remove(&conv_id);
                        let _ = out_sender.send(ChatViewOutput::ChatRunningChanged {
                            conversation_id: conv_id.clone(),
                            is_running: false,
                        });
                        if empty {
                            if let Some(pos) = store.find(&stream_obj) {
                                store.remove(pos);
                            }
                        } else {
                            let entry = stream_obj.entry();
                            stream_obj.update_streaming(
                                &entry.content,
                                entry.reasoning_content.as_deref(),
                                false,
                            );
                            stream_obj.set_metrics(total_tokens, elapsed_ms);
                            if is_visible {
                                maybe_scroll_bottom(&scroller);
                            }
                        }
                        if is_visible {
                            let _ = composer_sender.send(ComposerMsg::SetGenerating(false));
                        }
                    }
                    AgentEvent::TitleUpdated(new_title) => {
                        let _ = input_sender.send(ChatViewMsg::ConversationTitleUpdated {
                            id: conv_id.clone(),
                            title: new_title.clone(),
                        });
                        let _ = out_sender.send(ChatViewOutput::ConversationTitleUpdated {
                            id: conv_id.clone(),
                            title: new_title,
                        });
                    }
                }
            }
        });
    }

    fn collect_granted_folders(
        &self,
        conv_id: &str,
        sender: &ComponentSender<Self>,
    ) -> Vec<GrantedFolder> {
        let mut granted = Vec::new();
        for f in self.db.list_conversation_folders(conv_id).unwrap_or_default() {
            let p = PathBuf::from(&f.folder_path);
            if p.exists() {
                granted.push(GrantedFolder {
                    path: p,
                    allow_write: f.permission_mode.allows_write(),
                });
            } else {
                let _ = sender.output(ChatViewOutput::ShowToast(crate::i18n::tr_f(
                    "Granted folder unavailable: {path}",
                    &[("path", &f.folder_path)],
                )));
            }
        }
        granted
    }

    fn assemble_turns(&self, granted_folders: &[GrantedFolder]) -> Vec<crate::inference::provider::ChatTurn> {
        let personalization = self
            .db
            .get_personalization_settings()
            .unwrap_or_else(|_| PersonalizationSettings {
                assistant_name: self.assistant_name.borrow().clone(),
                user_name: self.user_name.borrow().clone(),
                personality: Default::default(),
                custom_personality: None,
                user_about: None,
            });
        let web_search_active =
            self.web_search_enabled && self.db.get_web_search_global_enabled().unwrap_or(false);

        let total = self.store.n_items();
        let start = total.saturating_sub(48);
        let mut history = Vec::with_capacity((total - start) as usize);
        for idx in start..total {
            if let Some(obj) = self.store.item(idx).and_downcast::<MessageObject>() {
                let entry = obj.entry();
                history.push(MessageWithAttachments {
                    role: MessageRole::from_str_lossy(&entry.role),
                    content: entry.content,
                    attachments: entry.attachments,
                });
            }
        }

        build_prompt_turns(&PromptContext {
            chat_system_prompt: &self.active_system_prompt,
            personalization: &personalization,
            granted_folders,
            web_search_active,
            history: &history,
        })
    }
}
