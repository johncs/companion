//! Multi-step streaming inference loop, tool-call execution, and DB persistence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gtk::glib;
use relm4::prelude::*;
use tokio::sync::mpsc;

use crate::agent::parser::{parse_tool_calls, strip_tool_calls};
use crate::agent::tools::GrantedFolder;
use crate::core::types::{
    ExecutionDevice, FolderPermission, MessageRole, PermissionDecision, ToolCallStatus,
};
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::tr_f;
use crate::inference::provider::{ChatEvent, ChatRole, ChatTurn, InferenceParams, ModelProvider};
use crate::storage::{ModelRecord, NewMessage, ToolCallRecord};
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::page_loader::maybe_scroll_bottom;
use super::stream::{ActiveChatRun, StreamStepUpdate, spawn_stream_coalescer};
use super::tool_runner::{run_chat_tool_call, run_fetch_url_call, run_web_search_call};
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

const MAX_AGENT_STEPS: usize = 5;

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
            // Let replies use up to the configured context window (bounded by `pos >= ctx_size`).
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
        let mut turns = self.build_prompt_turns();
        self.scroll_to_bottom();
        let model_id = model_record.id.clone();
        let model_path = model_record.file_path.clone().unwrap_or_default();

        let provider = self.provider.clone();
        let permissions = self.permissions.clone();
        let db = self.db.clone();
        let web_search_enabled = self.web_search_enabled;
        let conv_id = conv_id.to_string();
        let mut granted_folders: Vec<GrantedFolder> = Vec::new();
        for f in self.db.list_conversation_folders(&conv_id).unwrap_or_default() {
            let p = PathBuf::from(&f.folder_path);
            if p.exists() {
                granted_folders.push(GrantedFolder {
                    path: p,
                    allow_write: f.permission_mode.allows_write(),
                });
            } else {
                let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                    "Granted folder unavailable: {path}",
                    &[("path", &f.folder_path)],
                )));
            }
        }
        let composer_sender = self.composer.sender().clone();
        let input_sender = sender.input_sender().clone();
        let out_sender = sender.output_sender().clone();
        let scroller = self.scrolled_window.clone();
        let store = self.store.clone();
        let active_runs = self.active_runs.clone();
        let visible_conv_id = self.visible_conv_id.clone();

        glib::MainContext::default().spawn_local(async move {
            let generation_started = Instant::now();
            let mut total_completion_tokens: i64 = 0;
            let mut total_decode_ms: i64 = 0;
            let mut full_answer = String::new();
            let mut full_reasoning = String::new();
            let mut recorded_tools: Vec<ToolCallRecord> = Vec::new();

            for step in 0..MAX_AGENT_STEPS {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }

                let (raw_tx, raw_rx) = mpsc::unbounded_channel::<ChatEvent>();
                if let Err(err) = provider.stream_chat(
                    model_path.clone(),
                    turns.clone(),
                    base_params.clone(),
                    cancel_flag.clone(),
                    raw_tx,
                ) {
                    let _ = out_sender.send(ChatViewOutput::ShowToast(tr_f(
                        "Failed to start inference: {err}",
                        &[("err", &err.to_string())],
                    )));
                    break;
                }

                let mut ui_rx = spawn_stream_coalescer(
                    raw_rx,
                    full_answer.clone(),
                    full_reasoning.clone(),
                );
                let mut step_answer = String::new();

                while let Some(update) = ui_rx.recv().await {
                    match update {
                        StreamStepUpdate::Status(status) => {
                            stream_obj.set_status(Some(&status));
                        }
                        StreamStepUpdate::Snapshot {
                            combined,
                            blocks,
                            reasoning,
                        } => {
                            stream_obj.apply_streaming_snapshot(combined, blocks, reasoning, true);
                            if visible_conv_id.borrow().as_deref() == Some(conv_id.as_str()) {
                                maybe_scroll_bottom(&scroller);
                            }
                        }
                        StreamStepUpdate::Usage {
                            prompt_tokens,
                            completion_tokens,
                            decode_ms,
                        } => {
                            total_completion_tokens += completion_tokens as i64;
                            if let Some(ms) = decode_ms {
                                total_decode_ms += ms as i64;
                                if completion_tokens >= 2 && ms > 50 {
                                    let tps = (completion_tokens as f64) / ((ms as f64) / 1000.0);
                                    let db_speed = db.clone();
                                    let mid = model_id.clone();
                                    runtime().spawn_blocking(move || {
                                        let _ = db_speed.record_model_speed(&mid, tps);
                                    });
                                }
                            }
                            if visible_conv_id.borrow().as_deref() == Some(conv_id.as_str()) {
                                let _ = composer_sender.send(ComposerMsg::SetContextUsed(
                                    prompt_tokens + completion_tokens,
                                ));
                            }
                        }
                        StreamStepUpdate::Notice(msg) => {
                            let _ = out_sender.send(ChatViewOutput::ShowToast(msg));
                        }
                        StreamStepUpdate::Error(err) => {
                            let _ = out_sender.send(ChatViewOutput::ShowToast(tr_f(
                                "Inference error: {err}",
                                &[("err", &err)],
                            )));
                        }
                        StreamStepUpdate::Done {
                            step_answer: final_step,
                            full_reasoning: final_reasoning,
                        } => {
                            step_answer = final_step;
                            full_reasoning = final_reasoning;
                            break;
                        }
                    }
                }

                let visible_step = strip_tool_calls(&step_answer);
                if !visible_step.is_empty() {
                    if !full_answer.is_empty() {
                        full_answer.push_str("\n\n");
                    }
                    full_answer.push_str(&visible_step);
                }

                let calls = if !cancel_flag.load(Ordering::Relaxed) {
                    let parsed = parse_tool_calls(&step_answer);
                    if parsed.is_empty() || step + 1 >= MAX_AGENT_STEPS {
                        Vec::new()
                    } else {
                        parsed
                    }
                } else {
                    Vec::new()
                };

                if calls.is_empty() {
                    break;
                }

                let mut tool_feedback = String::new();
                for call in calls {
                    let (tc, output, ungranted_target) = if call.name == "web_search"
                        || call.name == "fetch_url"
                    {
                        let mut local_texts: Vec<String> = recorded_tools
                            .iter()
                            .filter(|t| t.tool_name == "read_file" && t.status == ToolCallStatus::Ok)
                            .filter_map(|t| t.output.clone())
                            .collect();
                        for t in &turns {
                            if matches!(t.role, ChatRole::User) && t.content.contains("<attachment") {
                                local_texts.push(t.content.clone());
                            }
                        }
                        let (tc, out) = if call.name == "web_search" {
                            run_web_search_call(
                                &scroller,
                                &stream_obj,
                                &db,
                                web_search_enabled,
                                &local_texts,
                                call,
                            )
                            .await
                        } else {
                            run_fetch_url_call(
                                &scroller,
                                &stream_obj,
                                &db,
                                web_search_enabled,
                                &local_texts,
                                call,
                            )
                            .await
                        };
                        (tc, out, None)
                    } else {
                        run_chat_tool_call(
                            &scroller,
                            &stream_obj,
                            &permissions,
                            &granted_folders,
                            &conv_id,
                            call,
                        )
                        .await
                    };
                    let tool_name = tc.tool_name.clone();
                    if tc.permission_decision == PermissionDecision::AllowChat {
                        let needs_write = matches!(
                            tool_name.as_str(),
                            "write_file" | "create_directory" | "move_path"
                        );
                        if let Some(new_path) = ungranted_target {
                            if !granted_folders.iter().any(|g| g.path == new_path) {
                                granted_folders.push(GrantedFolder {
                                    path: new_path.clone(),
                                    allow_write: needs_write,
                                });
                            }
                            if let Ok(entry) =
                                db.add_conversation_folder(&conv_id, &new_path.to_string_lossy())
                                && needs_write
                            {
                                let _ = db.set_conversation_folder_permission(
                                    &entry.id,
                                    FolderPermission::ReadWrite,
                                );
                            }
                        } else {
                            for gf in &mut granted_folders {
                                if let Ok(entry) = db
                                    .add_conversation_folder(&conv_id, &gf.path.to_string_lossy())
                                    && tool_name == "write_file"
                                {
                                    gf.allow_write = true;
                                    let _ = db.set_conversation_folder_permission(
                                        &entry.id,
                                        FolderPermission::ReadWrite,
                                    );
                                }
                            }
                        }
                        if visible_conv_id.borrow().as_deref() == Some(conv_id.as_str()) {
                            let _ = input_sender.send(ChatViewMsg::RefreshGrantedFolders);
                        }
                    }
                    stream_obj.append_tool_call(tc.clone());
                    recorded_tools.push(tc);

                    tool_feedback.push_str(&format!(
                        "<tool_result name=\"{tool_name}\">\n{output}\n</tool_result>\n",
                    ));
                }

                turns.push(ChatTurn {
                    role: ChatRole::Assistant,
                    content: step_answer,
                    images: Vec::new(),
                });
                turns.push(ChatTurn {
                    role: ChatRole::User,
                    content: tool_feedback,
                    images: Vec::new(),
                });
            }

            if active_runs
                .borrow()
                .get(&conv_id)
                .is_some_and(|r| Arc::ptr_eq(&r.cancel_flag, &cancel_flag))
            {
                active_runs.borrow_mut().remove(&conv_id);
                let _ = out_sender.send(ChatViewOutput::ChatRunningChanged {
                    conversation_id: conv_id.clone(),
                    is_running: false,
                });
            }
            let is_visible = visible_conv_id.borrow().as_deref() == Some(conv_id.as_str());

            if full_answer.is_empty() && full_reasoning.is_empty() && recorded_tools.is_empty() {
                if is_visible && let Some(pos) = store.find(&stream_obj) {
                    store.remove(pos);
                }
            } else {
                let elapsed_ms = if total_decode_ms > 0 {
                    total_decode_ms
                } else {
                    generation_started.elapsed().as_millis() as i64
                };
                let r_opt = (!full_reasoning.is_empty()).then_some(full_reasoning.as_str());
                stream_obj.update_streaming(&full_answer, r_opt, false);
                stream_obj.set_metrics(total_completion_tokens, elapsed_ms);
                if is_visible {
                    maybe_scroll_bottom(&scroller);
                }

                let db_save = db.clone();
                let conv_save = conv_id.clone();
                let answer_save = full_answer.clone();
                let reasoning_save = (!full_reasoning.is_empty()).then(|| full_reasoning.clone());
                let model_name_save = model_display_name.clone();
                let _ = runtime()
                    .spawn_blocking(move || {
                        if let Ok(saved) = db_save.insert_message(NewMessage {
                            conversation_id: &conv_save,
                            role: MessageRole::Assistant,
                            content: &answer_save,
                            reasoning_content: reasoning_save.as_deref(),
                            thinking_level: Some(thinking_level),
                            model_name: Some(&model_name_save),
                            execution_device: Some(execution_device),
                            token_count: total_completion_tokens,
                            generation_ms: Some(elapsed_ms),
                        }) {
                            for mut tc in recorded_tools {
                                tc.message_id = saved.id.clone();
                                let _ = db_save.insert_tool_call(&tc);
                            }
                        }
                    })
                    .await;
            }
            if is_visible {
                let _ = composer_sender.send(ComposerMsg::SetGenerating(false));
            }

            if !cancel_flag.load(Ordering::Relaxed)
                && !full_answer.trim().is_empty()
                && let Some(new_title) = crate::inference::title_gen::maybe_generate_title(
                    &db,
                    &provider,
                    &model_path,
                    &base_params,
                    &conv_id,
                    &turns,
                    &cancel_flag,
                )
                .await
            {
                let _ = input_sender.send(ChatViewMsg::ConversationTitleUpdated {
                    id: conv_id.clone(),
                    title: new_title.clone(),
                });
                let _ = out_sender.send(ChatViewOutput::ConversationTitleUpdated {
                    id: conv_id,
                    title: new_title,
                });
            }
        });
    }
}
