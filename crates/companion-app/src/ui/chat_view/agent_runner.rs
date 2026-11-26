//! Thin GTK adapter that launches [`AgentRunner`] and applies [`AgentEvent`]s to the UI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gtk::{gio, glib};
use relm4::prelude::*;

use crate::agent::registry::ToolRegistry;
use crate::agent::runner::{AgentEvent, AgentRunConfig, AgentRunner};
use crate::agent::tools::GrantedFolder;
use crate::core::ThinkingLevel;
use crate::core::types::ExecutionDevice;
use crate::inference::provider::{ChatTurn, InferenceParams};
use crate::storage::ModelRecord;
use crate::ui::composer::ComposerMsg;
use crate::ui::message_object::MessageObject;

use super::approval::GtkApprovalHandler;
use super::page_loader::maybe_scroll_bottom;
pub(super) use super::prompt_prep::{PreparedPrompt, prepare_prompt_blocking};
use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

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
        let conv_id = launch.conv_id.clone();
        self.cancel_run_for(&conv_id, sender);
        for notice in &launch.notices {
            let _ = sender.output(ChatViewOutput::ShowToast(notice.clone()));
        }
        let is_visible = self.active_conversation_id.as_deref() == Some(conv_id.as_str());
        if is_visible {
            self.composer.emit(ComposerMsg::SetGenerating(true));
        }

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (base_params, execution_device, model_display_name, stream_obj) =
            build_stream_objects(&launch.model_record, launch.thinking_level);
        if is_visible {
            self.store.append(&stream_obj);
            self.scroll_to_bottom();
        }
        let run_id = self.register_active_run(&conv_id, &stream_obj, &cancel_flag, sender);

        let rx = self.spawn_agent_runner(
            launch,
            base_params,
            execution_device,
            model_display_name,
            cancel_flag,
        );
        let ctx = self.build_stream_loop_ctx(conv_id, run_id, stream_obj, sender);
        glib::MainContext::default().spawn_local(async move {
            run_stream_event_loop(rx, ctx).await;
        });
    }

    fn register_active_run(
        &mut self,
        conv_id: &str,
        stream_obj: &MessageObject,
        cancel_flag: &Arc<AtomicBool>,
        sender: &ComponentSender<Self>,
    ) -> u64 {
        self.next_run_id = self.next_run_id.wrapping_add(1);
        let run_id = self.next_run_id;
        self.active_runs.borrow_mut().insert(
            conv_id.to_string(),
            ActiveChatRun {
                run_id,
                stream_obj: stream_obj.clone(),
                cancel_flag: cancel_flag.clone(),
            },
        );
        let _ = sender.output(ChatViewOutput::ChatRunningChanged {
            conversation_id: conv_id.to_string(),
            is_running: true,
        });
        run_id
    }

    fn spawn_agent_runner(
        &self,
        launch: StreamLaunch,
        base_params: InferenceParams,
        execution_device: ExecutionDevice,
        model_display_name: String,
        cancel_flag: Arc<AtomicBool>,
    ) -> tokio::sync::mpsc::UnboundedReceiver<AgentEvent> {
        let registry = ToolRegistry::with_builtins(self.services.web_search.clone());
        let runner = AgentRunner {
            provider: self.services.provider.clone(),
            registry: registry.clone(),
            permissions: self.permissions.clone(),
            approver: Arc::new(GtkApprovalHandler::new(&self.scrolled_window, registry)),
            db: self.services.db.clone(),
        };
        runner.spawn_on(
            &self.services.rt_handle,
            AgentRunConfig {
                conv_id: launch.conv_id,
                model_id: launch.model_record.id,
                model_path: launch.model_record.file_path.unwrap_or_default(),
                model_display_name,
                base_params,
                thinking_level: launch.thinking_level,
                execution_device,
                web_search_enabled: self.web_search_enabled,
                granted_folders: launch.granted_folders,
                turns: launch.turns,
                cancel_flag,
            },
        )
    }

    fn build_stream_loop_ctx(
        &self,
        conv_id: String,
        run_id: u64,
        stream_obj: MessageObject,
        sender: &ComponentSender<Self>,
    ) -> StreamLoopCtx {
        StreamLoopCtx {
            conv_id,
            run_id,
            stream_obj,
            composer_sender: self.composer.sender().clone(),
            input_sender: sender.input_sender().clone(),
            out_sender: sender.output_sender().clone(),
            scroller: self.scrolled_window.clone(),
            store: self.store.clone(),
            active_runs: self.active_runs.clone(),
            visible_conv_id: self.visible_conv_id.clone(),
            rt_handle: self.services.rt_handle.clone(),
        }
    }
}

struct StreamLoopCtx {
    conv_id: String,
    run_id: u64,
    stream_obj: MessageObject,
    composer_sender: relm4::Sender<ComposerMsg>,
    input_sender: relm4::Sender<ChatViewMsg>,
    out_sender: relm4::Sender<ChatViewOutput>,
    scroller: gtk::ScrolledWindow,
    store: gio::ListStore,
    active_runs: Rc<RefCell<HashMap<String, ActiveChatRun>>>,
    visible_conv_id: Rc<RefCell<Option<String>>>,
    rt_handle: tokio::runtime::Handle,
}

fn build_stream_objects(
    model_record: &crate::storage::ModelRecord,
    thinking_level: ThinkingLevel,
) -> (InferenceParams, ExecutionDevice, String, MessageObject) {
    let base_params = InferenceParams {
        model: crate::core::model_params::ModelParams {
            n_ctx: model_record.default_params.n_ctx,
            n_gpu_layers: model_record.default_params.n_gpu_layers,
            max_tokens: model_record
                .default_params
                .max_tokens
                .max(model_record.default_params.n_ctx),
            temperature: model_record.default_params.temperature,
            thinking_level,
        },
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
    (
        base_params,
        execution_device,
        model_display_name,
        stream_obj,
    )
}

async fn run_stream_event_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ctx: StreamLoopCtx,
) {
    while let Some(ev) = rx.recv().await {
        let is_visible = ctx.visible_conv_id.borrow().as_deref() == Some(ctx.conv_id.as_str());
        handle_stream_event(ev, is_visible, &ctx);
    }
}

fn handle_stream_event(ev: AgentEvent, is_visible: bool, ctx: &StreamLoopCtx) {
    match ev {
        AgentEvent::Status(s) => ctx.stream_obj.set_status(Some(&s)),
        AgentEvent::StreamSnapshot {
            combined_answer,
            blocks,
            reasoning,
        } => apply_snapshot_and_scroll(ctx, is_visible, combined_answer, blocks, reasoning),
        AgentEvent::ToolCallRecorded(tc) => ctx.stream_obj.append_tool_call(tc),
        AgentEvent::FolderGrantsUpdated if is_visible => {
            drop(ctx.input_sender.send(ChatViewMsg::RefreshGrantedFolders));
        }
        AgentEvent::ContextUsage { total_tokens } if is_visible => {
            drop(
                ctx.composer_sender
                    .send(ComposerMsg::SetContextUsed(total_tokens)),
            );
        }
        AgentEvent::FolderGrantsUpdated | AgentEvent::ContextUsage { .. } => {}
        AgentEvent::Notice(msg) | AgentEvent::Error(msg) => {
            drop(ctx.out_sender.send(ChatViewOutput::ShowToast(msg)));
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
            let p = StreamFinishedPayload {
                saved_message_id,
                created_at,
                final_answer,
                final_reasoning,
                total_tokens,
                elapsed_ms,
                empty,
            };
            handle_stream_finished(ctx, is_visible, p);
        }
        AgentEvent::TitleUpdated(new_title) => emit_title_updated(ctx, new_title),
    }
}

fn apply_snapshot_and_scroll(
    ctx: &StreamLoopCtx,
    is_visible: bool,
    combined_answer: String,
    blocks: Option<Vec<crate::format::markdown::MarkdownBlock>>,
    reasoning: Option<String>,
) {
    ctx.stream_obj
        .apply_streaming_snapshot(combined_answer, blocks, reasoning, true);
    if is_visible {
        maybe_scroll_bottom(&ctx.scroller);
    }
}

fn emit_title_updated(ctx: &StreamLoopCtx, new_title: String) {
    let _ = ctx
        .input_sender
        .send(ChatViewMsg::ConversationTitleUpdated {
            id: ctx.conv_id.clone(),
            title: new_title.clone(),
        });
    let _ = ctx
        .out_sender
        .send(ChatViewOutput::ConversationTitleUpdated {
            id: ctx.conv_id.clone(),
            title: new_title,
        });
}

struct StreamFinishedPayload {
    saved_message_id: Option<String>,
    created_at: Option<i64>,
    final_answer: String,
    final_reasoning: Option<String>,
    total_tokens: i64,
    elapsed_ms: i64,
    empty: bool,
}

fn handle_stream_finished(ctx: &StreamLoopCtx, is_visible: bool, payload: StreamFinishedPayload) {
    let StreamFinishedPayload {
        saved_message_id,
        created_at,
        final_answer,
        final_reasoning,
        total_tokens,
        elapsed_ms,
        empty,
    } = payload;
    let is_current_run = clear_current_run(ctx);
    if is_current_run {
        let _ = ctx.out_sender.send(ChatViewOutput::ChatRunningChanged {
            conversation_id: ctx.conv_id.clone(),
            is_running: false,
        });
        if is_visible {
            let _ = ctx.composer_sender.send(ComposerMsg::SetGenerating(false));
        }
    }
    if empty {
        if let Some(pos) = ctx.store.find(&ctx.stream_obj) {
            ctx.store.remove(pos);
        }
        return;
    }
    let answer_bg = final_answer.clone();
    let (stream_obj_ui, scroller_ui) = (ctx.stream_obj.clone(), ctx.scroller.clone());
    crate::ui::widgets::spawn_blocking_ui(
        &ctx.rt_handle,
        move || crate::format::markdown::parse_markdown_blocks(&answer_bg),
        move |blocks| {
            stream_obj_ui.finalize_persisted_with_blocks(
                crate::ui::message_object::FinalizedMessage {
                    saved_id: saved_message_id.as_deref(),
                    created_at,
                    final_answer: &final_answer,
                    final_reasoning: final_reasoning.as_deref(),
                    total_tokens,
                    elapsed_ms,
                    pre_rendered: Some(blocks),
                },
            );
            if is_visible {
                maybe_scroll_bottom(&scroller_ui);
            }
        },
    );
}

fn clear_current_run(ctx: &StreamLoopCtx) -> bool {
    let mut runs = ctx.active_runs.borrow_mut();
    if runs.get(&ctx.conv_id).map(|r| r.run_id) == Some(ctx.run_id) {
        runs.remove(&ctx.conv_id);
        true
    } else {
        false
    }
}
