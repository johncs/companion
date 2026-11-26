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
use crate::core::types::ExecutionDevice;
use crate::i18n::{gettext, tr_f};
use crate::inference::provider::{ChatTurn, InferenceParams};
use crate::storage::settings::PersonalizationSettings;
use crate::storage::{Database, ModelRecord};
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::approval::GtkApprovalHandler;
use super::page_loader::maybe_scroll_bottom;
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

pub(super) struct PromptPrepInput {
    pub(super) db: Database,
    pub(super) conv_id: String,
    pub(super) active_model_id: Option<String>,
    pub(super) chat_system_prompt: String,
    pub(super) web_search_enabled: bool,
    pub(super) fallback_assistant_name: String,
    pub(super) fallback_user_name: Option<String>,
    pub(super) history: Vec<MessageWithAttachments>,
}

#[derive(Debug)]
pub struct PreparedPrompt {
    pub(super) model_record: ModelRecord,
    pub(super) granted_folders: Vec<GrantedFolder>,
    pub(super) turns: Vec<ChatTurn>,
    pub(super) notices: Vec<String>,
}

pub(super) struct StreamLaunch {
    pub(super) conv_id: String,
    pub(super) model_record: ModelRecord,
    pub(super) thinking_level: ThinkingLevel,
    pub(super) granted_folders: Vec<GrantedFolder>,
    pub(super) turns: Vec<ChatTurn>,
    pub(super) notices: Vec<String>,
}

pub(super) struct ActiveChatRun {
    pub(super) run_id: u64,
    pub(super) stream_obj: MessageObject,
    pub(super) cancel_flag: Arc<AtomicBool>,
}

impl ChatViewModel {
    pub(super) fn start_llama_stream(
        &mut self,
        launch: StreamLaunch,
        sender: &ComponentSender<Self>,
    ) {
        let StreamLaunch {
            conv_id,
            model_record,
            thinking_level,
            granted_folders,
            turns,
            notices,
        } = launch;
        self.cancel_run_for(&conv_id, sender);
        for notice in notices {
            let _ = sender.output(ChatViewOutput::ShowToast(notice));
        }
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
        };
        let execution_device = if base_params.n_gpu_layers > 0 {
            ExecutionDevice::Gpu
        } else {
            ExecutionDevice::Cpu
        };
        let model_display_name = crate::models::friendly_model_name(&model_record.display_name);
        let stream_obj = MessageObject::new_streaming(
            uuid::Uuid::new_v4().to_string(),
            Some(thinking_level),
            Some(model_display_name.clone()),
            Some(execution_device),
            chrono::Utc::now().timestamp_millis(),
        );
        self.store.append(&stream_obj);
        self.next_run_id = self.next_run_id.wrapping_add(1);
        let run_id = self.next_run_id;
        self.active_runs.borrow_mut().insert(
            conv_id.clone(),
            ActiveChatRun {
                run_id,
                stream_obj: stream_obj.clone(),
                cancel_flag: cancel_flag.clone(),
            },
        );
        let _ = sender.output(ChatViewOutput::ChatRunningChanged {
            conversation_id: conv_id.clone(),
            is_running: true,
        });
        self.scroll_to_bottom();

        let runner = AgentRunner {
            provider: Arc::new(self.provider.clone()),
            registry: ToolRegistry::with_builtins(),
            permissions: self.permissions.clone(),
            approver: Arc::new(GtkApprovalHandler::new(&self.scrolled_window)),
            db: self.db.clone(),
        };
        let mut rx = runner.spawn_on(
            crate::core::runtime().handle(),
            AgentRunConfig {
                conv_id: conv_id.clone(),
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
            },
        );
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
                        stream_obj.apply_streaming_snapshot(
                            combined_answer,
                            blocks,
                            reasoning,
                            true,
                        );
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
                        saved_message_id,
                        created_at,
                        final_answer,
                        final_reasoning,
                        total_tokens,
                        elapsed_ms,
                        empty,
                    } => {
                        let is_current_run = {
                            let mut runs = active_runs.borrow_mut();
                            if runs.get(&conv_id).map(|r| r.run_id) == Some(run_id) {
                                runs.remove(&conv_id);
                                true
                            } else {
                                false
                            }
                        };
                        if is_current_run {
                            let _ = out_sender.send(ChatViewOutput::ChatRunningChanged {
                                conversation_id: conv_id.clone(),
                                is_running: false,
                            });
                            if is_visible {
                                let _ = composer_sender.send(ComposerMsg::SetGenerating(false));
                            }
                        }
                        if empty {
                            if let Some(pos) = store.find(&stream_obj) {
                                store.remove(pos);
                            }
                        } else {
                            stream_obj.finalize_persisted(
                                saved_message_id.as_deref(),
                                created_at,
                                &final_answer,
                                final_reasoning.as_deref(),
                                total_tokens,
                                elapsed_ms,
                            );
                            if is_visible {
                                maybe_scroll_bottom(&scroller);
                            }
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
                    attachments: entry.attachments,
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
            db: self.db.clone(),
            conv_id,
            active_model_id: self.active_model_id.clone(),
            chat_system_prompt: self.active_system_prompt.clone(),
            web_search_enabled: self.web_search_enabled,
            fallback_assistant_name: self.assistant_name.borrow().clone(),
            fallback_user_name: self.user_name.borrow().clone(),
            history,
        }
    }
}

pub(super) fn prepare_prompt_blocking(input: &PromptPrepInput) -> Result<PreparedPrompt, String> {
    let model_record = resolve_active_model_blocking(&input.db, input.active_model_id.as_deref())?;
    let mut notices = Vec::new();
    let mut granted_folders = Vec::new();
    for f in input
        .db
        .list_conversation_folders(&input.conv_id)
        .unwrap_or_default()
    {
        let p = PathBuf::from(&f.folder_path);
        if p.exists() {
            granted_folders.push(GrantedFolder {
                path: p,
                allow_write: f.permission_mode.allows_write(),
            });
        } else {
            notices.push(tr_f(
                "Granted folder unavailable: {path}",
                &[("path", &f.folder_path)],
            ));
        }
    }
    let personalization =
        input
            .db
            .get_personalization_settings()
            .unwrap_or_else(|_| PersonalizationSettings {
                assistant_name: input.fallback_assistant_name.clone(),
                user_name: input.fallback_user_name.clone(),
                personality: Default::default(),
                custom_personality: None,
                user_about: None,
            });
    let web_search_active =
        input.web_search_enabled && input.db.get_web_search_global_enabled().unwrap_or(false);
    let turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: &input.chat_system_prompt,
        personalization: &personalization,
        granted_folders: &granted_folders,
        web_search_active,
        history: &input.history,
    });
    Ok(PreparedPrompt {
        model_record,
        granted_folders,
        turns,
        notices,
    })
}

fn resolve_active_model_blocking(
    db: &Database,
    active_model_id: Option<&str>,
) -> Result<ModelRecord, String> {
    let mut model_record = active_model_id
        .and_then(|id| db.get_model(id).ok().flatten())
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
        let _ = db.upsert_model(rec);
    }
    model_record.ok_or_else(|| {
        gettext("No model selected or model file is missing. Choose a model in Settings.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::MessageRole;

    #[test]
    fn prepare_prompt_blocking_validates_model_and_missing_folders() {
        let db = Database::open_in_memory().expect("in-memory db");
        let conv = db.create_conversation("Test").expect("conv");
        let missing_dir = std::env::temp_dir().join("companion_missing_folder_test_xyz");
        let _ = std::fs::remove_dir_all(&missing_dir);
        db.add_conversation_folder(&conv.id, &missing_dir.to_string_lossy())
            .expect("grant folder");

        let err = prepare_prompt_blocking(&PromptPrepInput {
            db: db.clone(),
            conv_id: conv.id.clone(),
            active_model_id: Some("missing-id".to_string()),
            chat_system_prompt: "Be helpful.".to_string(),
            web_search_enabled: false,
            fallback_assistant_name: "Companion".to_string(),
            fallback_user_name: None,
            history: vec![MessageWithAttachments {
                role: MessageRole::User,
                content: "hi".to_string(),
                attachments: Vec::new(),
            }],
        })
        .unwrap_err();
        assert!(err.contains("No model selected"));
    }
}
