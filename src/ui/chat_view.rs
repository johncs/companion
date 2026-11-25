//! Virtualized chat stream (`gtk::ListView`) with paginated SQLite loading, composer, and `llama.cpp` streaming.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::prelude::*;
use tokio::sync::mpsc;

use crate::agent::web_search::{
    execute_fetch_url, execute_web_search, inspect_fetch_url, inspect_search_query,
    validate_fetch_url,
};
use crate::agent::{WEB_SEARCH_TOOL_INSTRUCTIONS, WORKSPACE_TOOLS_INSTRUCTIONS};
use crate::agent::parser::{parse_tool_calls, strip_tool_calls};
use crate::agent::permissions::{PermissionDecision, PermissionEngine};
use crate::agent::tools::{
    GrantedFolder, describe_tool_action, execute_tool_with_grants, is_write_already_granted,
    requested_ungranted_path,
};
use crate::core::attachments::format_attachments_for_prompt;
use crate::core::markdown::{CodeHighlightCache, MarkdownBlock, parse_markdown_blocks_streaming};
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::{gettext, tr_f};
use crate::inference::llama_cpp::LlamaCppProvider;
use crate::inference::provider::{ChatEvent, ChatRole, ChatTurn, InferenceParams, ModelProvider};
use crate::storage::conversations::{compose_personalized_system_prompt, default_system_prompt};
use crate::storage::{AttachmentRecord, Database, ModelRecord, ToolCallRecord};
use crate::ui::composer::{ComposerModel, ComposerMsg, ComposerOutput};
use crate::ui::message_object::MessageObject;
use crate::ui::message_row::build_message_list_factory;
use crate::ui::permission_dialog::{
    FolderDropGrantChoice, present_fetch_url_confirm_dialog, present_folder_drop_confirm_dialog,
    present_tool_permission_dialog, present_web_search_confirm_dialog,
};

const PAGE_SIZE: usize = 20;
const MAX_AGENT_STEPS: usize = 5;
const STREAM_UI_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

#[derive(Debug)]
pub enum ChatViewMsg {
    LoadConversation {
        conversation_id: String,
        title: String,
    },
    ConversationPageLoaded {
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
    },
    ClearConversation,
    CancelConversation(String),
    OpenFolderPermissions,
    GrantFolder(PathBuf),
    RefreshGrantedFolders,
    SendMessage {
        text: String,
        thinking_level: ThinkingLevel,
        attachments: Vec<AttachmentRecord>,
    },
    EditAndResendMessage {
        message_id: String,
        new_content: String,
    },
    DeleteMessageFrom(String),
    StopGeneration,
    LoadOlderPage,
    OlderPageLoaded {
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
    },
    SystemPromptChanged(String),
    ThinkingLevelChanged(ThinkingLevel),
    WebSearchToggled(bool),
    RefreshWebSearchGlobal(bool),
    RefreshModels(Option<String>),
    PersonalizationChanged {
        assistant_name: String,
        user_name: Option<String>,
    },
    ConversationTitleUpdated {
        id: String,
        title: String,
    },
    OpenModelSettings,
    ShowToast(String),
}

#[derive(Debug)]
pub struct LoadedPageData {
    items: Vec<(
        crate::storage::entities::Message,
        Vec<AttachmentRecord>,
        Vec<ToolCallRecord>,
        Vec<crate::core::markdown::MarkdownBlock>,
    )>,
    has_more_pages: bool,
    oldest_loaded_ts: Option<i64>,
    est_tokens: u32,
}

#[derive(Debug)]
pub enum ChatViewOutput {
    ShowToast(String),
    ConversationTitleUpdated {
        id: String,
        title: String,
    },
    ChatRunningChanged {
        conversation_id: String,
        is_running: bool,
    },
    OpenModelSettings,
}

struct ActiveChatRun {
    stream_obj: MessageObject,
    cancel_flag: Arc<AtomicBool>,
}

pub struct ChatViewModel {
    db: Database,
    provider: LlamaCppProvider,
    permissions: PermissionEngine,
    store: gio::ListStore,
    scrolled_window: gtk::ScrolledWindow,
    message_list: gtk::ListView,
    composer: Controller<ComposerModel>,
    active_conversation_id: Option<String>,
    visible_conv_id: Rc<RefCell<Option<String>>>,
    active_model_id: Option<String>,
    assistant_name: Rc<RefCell<String>>,
    user_name: Rc<RefCell<Option<String>>>,
    active_system_prompt: String,
    active_title: String,
    active_thinking_level: ThinkingLevel,
    web_search_enabled: bool,
    oldest_loaded_ts: Option<i64>,
    has_more_pages: bool,
    is_loading_older: bool,
    load_seq: u64,
    is_loading: bool,
    is_empty: bool,
    active_runs: Rc<RefCell<HashMap<String, ActiveChatRun>>>,
}

#[relm4::component(pub)]
impl SimpleComponent for ChatViewModel {
    type Init = (Database, Option<String>);
    type Input = ChatViewMsg;
    type Output = ChatViewOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_vexpand: true,
            set_hexpand: true,

            #[watch]
            set_visible: model.active_conversation_id.is_some(),

            #[name = "root_overlay"]
            gtk::Overlay {
                set_vexpand: true,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_vexpand: true,

                    gtk::Overlay {
                        set_vexpand: true,

                        #[local_ref]
                        scrolled_window -> gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            set_vexpand: true,

                            #[local_ref]
                            message_list -> gtk::ListView {
                                set_show_separators: false,
                                add_css_class: "chat-list",
                            }
                        },

                        add_overlay = &adw::StatusPage {
                            #[watch]
                            set_visible: model.is_empty && !model.is_loading,
                            set_icon_name: Some("chat-message-new-symbolic"),
                            #[watch]
                            set_title: &model.active_title,
                            #[watch]
                            set_description: Some(&model.empty_description()),
                        },

                        add_overlay = &gtk::Spinner {
                            #[watch]
                            set_visible: model.is_loading,
                            #[watch]
                            set_spinning: model.is_loading,
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            set_width_request: 36,
                            set_height_request: 36,
                            set_can_target: false,
                        }
                    },

                    append = model.composer.widget(),
                },

                #[name = "drop_overlay"]
                add_overlay = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Fill,
                    set_valign: gtk::Align::Fill,
                    set_margin_top: 16,
                    set_margin_bottom: 16,
                    set_margin_start: 16,
                    set_margin_end: 16,
                    set_can_target: false,
                    set_visible: false,
                    add_css_class: "drop-zone-overlay",

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_halign: gtk::Align::Center,
                        set_vexpand: true,
                        set_spacing: 12,

                        gtk::Image {
                            set_icon_name: Some("document-send-symbolic"),
                            set_pixel_size: 48,
                            add_css_class: "accent",
                        },

                        gtk::Label {
                            set_label: &gettext("Drop files to attach"),
                            add_css_class: "title-2",
                        },

                        gtk::Label {
                            set_label: &gettext("Images, PDFs, text, and code files are supported"),
                            add_css_class: "dim-label",
                        },
                    }
                }
            }
        }
    }

    fn init(
        (db, active_model_id): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let store = gio::ListStore::new::<MessageObject>();
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let assistant_name = Rc::new(RefCell::new(
            db.get_assistant_name()
                .unwrap_or_else(|_| "Companion".to_string()),
        ));
        let user_name = Rc::new(RefCell::new(db.get_user_name().ok().flatten()));
        let edit_sender = sender.input_sender().clone();
        let delete_sender = sender.input_sender().clone();
        let factory = build_message_list_factory(
            assistant_name.clone(),
            user_name.clone(),
            std::rc::Rc::new(move |message_id, new_content| {
                let _ = edit_sender.send(ChatViewMsg::EditAndResendMessage {
                    message_id,
                    new_content,
                });
            }),
            std::rc::Rc::new(move |message_id| {
                let _ = delete_sender.send(ChatViewMsg::DeleteMessageFrom(message_id));
            }),
        );

        let composer = ComposerModel::builder()
            .launch((db.clone(), active_model_id.clone()))
            .forward(sender.input_sender(), |output| match output {
                ComposerOutput::Send {
                    text,
                    thinking_level,
                    attachments,
                } => ChatViewMsg::SendMessage {
                    text,
                    thinking_level,
                    attachments,
                },
                ComposerOutput::StopRequested => ChatViewMsg::StopGeneration,
                ComposerOutput::OpenFolderPermissions => ChatViewMsg::OpenFolderPermissions,
                ComposerOutput::GrantFolder(path) => ChatViewMsg::GrantFolder(path),
                ComposerOutput::WebSearchToggled(enabled) => ChatViewMsg::WebSearchToggled(enabled),
                ComposerOutput::SystemPromptChanged(p) => ChatViewMsg::SystemPromptChanged(p),
                ComposerOutput::ThinkingLevelChanged(lvl) => {
                    ChatViewMsg::ThinkingLevelChanged(lvl)
                }
                ComposerOutput::OpenModelSettings => ChatViewMsg::OpenModelSettings,
                ComposerOutput::ShowToast(msg) => ChatViewMsg::ShowToast(msg),
            });

        let scrolled_window = gtk::ScrolledWindow::new();
        let message_list = gtk::ListView::new(Some(selection), Some(factory));
        let permissions = PermissionEngine::new();

        let model = Self {
            db,
            provider: LlamaCppProvider::new(),
            permissions,
            store,
            scrolled_window: scrolled_window.clone(),
            message_list: message_list.clone(),
            composer,
            active_conversation_id: None,
            visible_conv_id: Rc::new(RefCell::new(None)),
            active_model_id,
            assistant_name,
            user_name,
            active_system_prompt: default_system_prompt().to_string(),
            active_title: String::new(),
            active_thinking_level: ThinkingLevel::Off,
            web_search_enabled: false,
            oldest_loaded_ts: None,
            has_more_pages: false,
            is_loading_older: false,
            load_seq: 0,
            is_loading: false,
            is_empty: true,
            active_runs: Rc::new(RefCell::new(HashMap::new())),
        };

        let widgets = view_output!();

        // Lazy-load older messages when scrolling near the top.
        let vadj = scrolled_window.vadjustment();
        let sender_clone = sender.clone();
        vadj.connect_value_changed(move |adj| {
            if adj.value() < 80.0 && adj.upper() > adj.page_size() {
                sender_clone.input(ChatViewMsg::LoadOlderPage);
            }
        });

        // Capture-phase DropTarget across the entire chat pane so TextView doesn't swallow file drops as text.
        let drop_target = gtk::DropTarget::builder()
            .actions(gdk::DragAction::COPY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

        {
            let overlay = widgets.drop_overlay.clone();
            drop_target.connect_enter(move |_, _, _| {
                overlay.set_visible(true);
                gdk::DragAction::COPY
            });
        }
        {
            let overlay = widgets.drop_overlay.clone();
            drop_target.connect_leave(move |_| {
                overlay.set_visible(false);
            });
        }
        {
            let overlay = widgets.drop_overlay.clone();
            let composer_sender = model.composer.sender().clone();
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
        widgets.root_overlay.add_controller(drop_target);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ChatViewMsg::LoadConversation {
                conversation_id,
                title,
            } => {
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

                let loaded_conv = self.db.get_conversation(&conversation_id).ok().flatten();
                self.active_system_prompt = loaded_conv
                    .as_ref()
                    .and_then(|c| c.system_prompt.clone())
                    .unwrap_or_else(|| default_system_prompt().to_string());
                self.active_thinking_level = loaded_conv
                    .as_ref()
                    .map(|c| ThinkingLevel::from_str_lossy(&c.thinking_level))
                    .unwrap_or_default();
                self.web_search_enabled = loaded_conv
                    .as_ref()
                    .map(|c| c.web_search_enabled)
                    .unwrap_or(false);
                self.composer.emit(ComposerMsg::SetSystemPrompt(
                    self.active_system_prompt.clone(),
                ));
                self.composer
                    .emit(ComposerMsg::SetThinkingLevel(self.active_thinking_level));
                self.composer
                    .emit(ComposerMsg::SetWebSearchEnabled(self.web_search_enabled));
                let granted = self
                    .db
                    .list_conversation_folders(&conversation_id)
                    .unwrap_or_default();
                self.composer
                    .emit(ComposerMsg::SetGrantedFolders(granted));

                let db = self.db.clone();
                let conv_id = conversation_id.clone();
                let sys_prompt_len = self.active_system_prompt.len();
                let input_sender = sender.input_sender().clone();

                runtime().spawn_blocking(move || {
                    let result =
                        load_and_parse_page(&db, &conv_id, None, sys_prompt_len);
                    let _ = input_sender.send(ChatViewMsg::ConversationPageLoaded {
                        conversation_id: conv_id,
                        request_seq: req_seq,
                        result,
                    });
                });
            }
            ChatViewMsg::ConversationPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => {
                if self.load_seq != request_seq
                    || self.active_conversation_id.as_deref() != Some(conversation_id.as_str())
                {
                    return;
                }
                self.is_loading = false;
                match result {
                    Ok(data) => {
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
            ChatViewMsg::ClearConversation => {
                self.cancel_active_stream(&sender);
                self.active_conversation_id = None;
                *self.visible_conv_id.borrow_mut() = None;
                self.store.remove_all();
                self.is_loading = false;
                self.is_empty = true;
                self.web_search_enabled = false;
                self.composer.emit(ComposerMsg::SetWebSearchEnabled(false));
            }
            ChatViewMsg::CancelConversation(id) => {
                self.cancel_run_for(&id, &sender);
                if self.active_conversation_id.as_deref() == Some(id.as_str()) {
                    self.composer.emit(ComposerMsg::SetGenerating(false));
                }
            }
            ChatViewMsg::LoadOlderPage => {
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
            ChatViewMsg::OlderPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => {
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
            ChatViewMsg::OpenFolderPermissions => {
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
            ChatViewMsg::GrantFolder(path) => {
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
                                        "read_write",
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
            ChatViewMsg::RefreshGrantedFolders => {
                if let Some(conv_id) = &self.active_conversation_id {
                    let granted = self.db.list_conversation_folders(conv_id).unwrap_or_default();
                    self.composer.emit(ComposerMsg::SetGrantedFolders(granted));
                }
            }
            ChatViewMsg::SendMessage {
                text,
                thinking_level,
                attachments,
            } => {
                self.active_thinking_level = thinking_level;
                let Some(conv_id) = self.active_conversation_id.clone() else {
                    return;
                };

                let Some(model_record) = self.resolve_active_model(&sender) else {
                    return;
                };

                match self.db.insert_message(
                    &conv_id,
                    "user",
                    &text,
                    None,
                    Some(thinking_level.as_str()),
                ) {
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
                        self.start_llama_stream(&conv_id, &model_record, thinking_level, &sender);
                    }
                    Err(err) => {
                        let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                            "Failed to save message: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                }
            }
            ChatViewMsg::EditAndResendMessage {
                message_id,
                new_content,
            } => {
                self.cancel_active_stream(&sender);
                let Some(conv_id) = self.active_conversation_id.clone() else {
                    return;
                };
                let Some(model_record) = self.resolve_active_model(&sender) else {
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
                self.start_llama_stream(&conv_id, &model_record, self.active_thinking_level, &sender);
            }
            ChatViewMsg::DeleteMessageFrom(message_id) => {
                self.cancel_active_stream(&sender);
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

                if let Err(err) =
                    self.db
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
            ChatViewMsg::StopGeneration => {
                self.cancel_active_stream(&sender);
            }
            ChatViewMsg::SystemPromptChanged(prompt) => {
                self.active_system_prompt = prompt.clone();
                if let Some(conv_id) = &self.active_conversation_id {
                    let _ = self
                        .db
                        .set_conversation_system_prompt(conv_id, Some(&prompt));
                }
            }
            ChatViewMsg::ThinkingLevelChanged(level) => {
                self.active_thinking_level = level;
                if let Some(conv_id) = &self.active_conversation_id {
                    let _ = self
                        .db
                        .set_conversation_thinking_level(conv_id, level.as_str());
                }
            }
            ChatViewMsg::WebSearchToggled(enabled) => {
                self.web_search_enabled = enabled;
                if let Some(conv_id) = &self.active_conversation_id {
                    let _ = self
                        .db
                        .set_conversation_web_search_enabled(conv_id, enabled);
                }
            }
            ChatViewMsg::RefreshWebSearchGlobal(enabled) => {
                self.composer
                    .emit(ComposerMsg::SetWebSearchGlobalEnabled(enabled));
            }
            ChatViewMsg::RefreshModels(id) => {
                let model_changed = self.active_model_id != id;
                self.active_model_id = id.clone();
                if model_changed && !self.active_runs.borrow().is_empty() {
                    self.cancel_all_runs(&sender);
                    self.provider.unload();
                    let _ = sender.output(ChatViewOutput::ShowToast(gettext(
                        "Stopped active chats to switch AI model",
                    )));
                }
                self.composer.emit(ComposerMsg::RefreshModels(id));
            }
            ChatViewMsg::PersonalizationChanged {
                assistant_name,
                user_name,
            } => {
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
            ChatViewMsg::ConversationTitleUpdated { id, title } => {
                if self.active_conversation_id.as_deref() == Some(&id) {
                    self.active_title = title;
                }
            }
            ChatViewMsg::OpenModelSettings => {
                let _ = sender.output(ChatViewOutput::OpenModelSettings);
            }
            ChatViewMsg::ShowToast(msg) => {
                let _ = sender.output(ChatViewOutput::ShowToast(msg));
            }
        }
    }
}

impl ChatViewModel {
    fn resolve_active_model(&self, sender: &ComponentSender<Self>) -> Option<ModelRecord> {
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

    fn empty_description(&self) -> String {
        let name = self.assistant_name.borrow();
        tr_f(
            "Ask {name} anything, or share a file or image below.",
            &[("name", &name)],
        )
    }

    fn scroll_to_bottom(&self) {
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

    fn cancel_active_stream(&mut self, sender: &ComponentSender<Self>) {
        if let Some(conv_id) = self.active_conversation_id.clone() {
            self.cancel_run_for(&conv_id, sender);
        }
        self.composer.emit(ComposerMsg::SetGenerating(false));
    }

    fn cancel_run_for(&mut self, conv_id: &str, sender: &ComponentSender<Self>) {
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

    fn cancel_all_runs(&mut self, sender: &ComponentSender<Self>) {
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

    fn build_prompt_turns(&self) -> Vec<ChatTurn> {
        let mut turns = Vec::new();
        let assistant = self.assistant_name.borrow().clone();
        let user_name = self.db.get_user_name().ok().flatten();
        let personality = self.db.get_assistant_personality().unwrap_or_default();
        let custom_personality = self.db.get_custom_personality().ok().flatten();
        let user_about = self.db.get_user_about().ok().flatten();
        let base_prompt = compose_personalized_system_prompt(
            &self.active_system_prompt,
            &assistant,
            user_name.as_deref(),
            personality,
            custom_personality.as_deref(),
            user_about.as_deref(),
        );
        let mut sys_prompt = base_prompt;
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

    fn start_llama_stream(
        &mut self,
        conv_id: &str,
        model_record: &ModelRecord,
        thinking_level: ThinkingLevel,
        sender: &ComponentSender<Self>,
    ) {
        self.cancel_run_for(conv_id, sender);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.composer.emit(ComposerMsg::SetGenerating(true));

        let mut base_params = parse_model_params(&model_record.default_params_json);
        base_params.thinking_level = thinking_level;
        base_params.mmproj_path = model_record.mmproj_path.clone();
        // Let replies use up to the configured context window (bounded by `pos >= ctx_size`).
        base_params.max_tokens = base_params.max_tokens.max(base_params.n_ctx);
        let execution_device = if base_params.n_gpu_layers > 0 {
            "gpu"
        } else {
            "cpu"
        };

        let model_display_name = crate::models::friendly_model_name(&model_record.display_name);
        let stream_obj = MessageObject::new_streaming(
            uuid::Uuid::new_v4().to_string(),
            Some(thinking_level.as_str().to_string()),
            Some(model_display_name.clone()),
            Some(execution_device.to_string()),
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
                    allow_write: f.permission_mode == "read_write",
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
                            .filter(|t| t.tool_name == "read_file" && t.status == "ok")
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
                    if tc.permission_decision == "allow_chat" {
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
                                    "read_write",
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
                                        "read_write",
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

                if let Ok(saved) = db.insert_message_with_metrics(
                    &conv_id,
                    "assistant",
                    &full_answer,
                    r_opt,
                    Some(thinking_level.as_str()),
                    Some(&model_display_name),
                    Some(execution_device),
                    total_completion_tokens,
                    Some(elapsed_ms),
                ) {
                    for mut tc in recorded_tools {
                        tc.message_id = saved.id.clone();
                        let _ = db.insert_tool_call(&tc);
                    }
                }
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

fn extract_mentioned_os_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let cleaned = raw.trim_end_matches([',', '.', ';', ':', ')', ']', '"', '\'']);
        if !cleaned.starts_with('/') {
            continue;
        }
        let path = Path::new(cleaned);
        if (path.is_dir() || path.is_file())
            && let Ok(canon) = std::fs::canonicalize(path)
            && !out.contains(&canon)
        {
            out.push(canon);
        }
    }
    out
}

/// Detect existing absolute OS paths (including ones containing spaces) in free-form user text.
/// Used only as a system-prompt hint so the model passes the exact path; never auto-grants access.
fn detect_existing_os_paths_in_text(text: &str) -> Vec<PathBuf> {
    const MAX_STAT_CHECKS: usize = 32;
    let mut out = Vec::new();
    let mut stat_checks = 0usize;
    if text.len() > 8192 {
        return out;
    }
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() && stat_checks < MAX_STAT_CHECKS {
            if bytes[i] == b'/'
                && (i == 0
                    || matches!(
                        bytes[i - 1],
                        b' ' | b'\t' | b'"' | b'\'' | b'(' | b'[' | b'@'
                    ))
            {
                let rest = &line[i..];
                let mut matched_len = 0usize;
                // Test longest-to-shortest prefixes ending at quote, whitespace, or line end.
                let mut end_indices: Vec<usize> = rest
                    .char_indices()
                    .filter_map(|(idx, ch)| {
                        if ch.is_whitespace() || matches!(ch, '"' | '\'' | '?' | ',' | ')' | ']') {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .take(12)
                    .collect();
                end_indices.push(rest.len());

                for &end in end_indices.iter().rev() {
                    if stat_checks >= MAX_STAT_CHECKS {
                        break;
                    }
                    let candidate = rest[..end]
                        .trim()
                        .trim_end_matches(['?', ',', '.', ';', ':', ')', ']', '"', '\'']);
                    if candidate.len() > 1 {
                        stat_checks += 1;
                        let p = Path::new(candidate);
                        if (p.is_file() || p.is_dir())
                            && let Ok(canon) = std::fs::canonicalize(p)
                        {
                            if !out.contains(&canon) {
                                out.push(canon);
                            }
                            matched_len = end;
                            break;
                        }
                    }
                }
                if matched_len > 0 {
                    i += matched_len;
                    continue;
                }
            }
            i += line[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

fn combine_answer(prefix: &str, step_visible: &str) -> String {
    if prefix.is_empty() {
        step_visible.to_string()
    } else if step_visible.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}\n\n{step_visible}")
    }
}

enum StreamStepUpdate {
    Status(String),
    Snapshot {
        combined: String,
        blocks: Option<Vec<MarkdownBlock>>,
        reasoning: Option<String>,
    },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        decode_ms: Option<u64>,
    },
    Notice(String),
    Error(String),
    Done {
        step_answer: String,
        full_reasoning: String,
    },
}

/// Coalesce token bursts and parse streaming Markdown on Tokio so the GTK main thread stays responsive.
fn spawn_stream_coalescer(
    mut raw_rx: mpsc::UnboundedReceiver<ChatEvent>,
    full_answer_prefix: String,
    mut full_reasoning: String,
) -> mpsc::UnboundedReceiver<StreamStepUpdate> {
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    runtime().spawn(async move {
        let mut step_answer = String::new();
        let mut last_combined = String::new();
        let mut code_cache = CodeHighlightCache::new();
        let mut last_flush = Instant::now();
        let mut dirty = false;

        let flush_snapshot = |step_answer: &str,
                              full_reasoning: &str,
                              last_combined: &mut String,
                              code_cache: &mut CodeHighlightCache| {
            let visible_step = strip_tool_calls(step_answer);
            let combined = combine_answer(&full_answer_prefix, &visible_step);
            let blocks = if combined != *last_combined {
                *last_combined = combined.clone();
                Some(parse_markdown_blocks_streaming(&combined, code_cache))
            } else {
                None
            };
            let reasoning = (!full_reasoning.is_empty()).then(|| full_reasoning.to_string());
            let _ = ui_tx.send(StreamStepUpdate::Snapshot {
                combined,
                blocks,
                reasoning,
            });
        };

        loop {
            let ev_opt = if dirty {
                let deadline = tokio::time::Instant::from_std(last_flush + STREAM_UI_FLUSH_INTERVAL);
                tokio::select! {
                    biased;
                    ev = raw_rx.recv() => Some(ev),
                    _ = tokio::time::sleep_until(deadline) => None,
                }
            } else {
                Some(raw_rx.recv().await)
            };

            match ev_opt {
                Some(None) => {
                    let _ = ui_tx.send(StreamStepUpdate::Done {
                        step_answer,
                        full_reasoning,
                    });
                    break;
                }
                Some(Some(first_ev)) => {
                    let mut done = false;
                    let mut pending = Some(first_ev);
                    while let Some(ev) = pending.take().or_else(|| raw_rx.try_recv().ok()) {
                        match ev {
                            ChatEvent::Status(status) => {
                                if full_answer_prefix.is_empty() && full_reasoning.is_empty() {
                                    let _ = ui_tx.send(StreamStepUpdate::Status(status));
                                }
                            }
                            ChatEvent::Token(t) => {
                                step_answer.push_str(&t);
                                dirty = true;
                            }
                            ChatEvent::ReasoningToken(r) => {
                                full_reasoning.push_str(&r);
                                dirty = true;
                            }
                            ChatEvent::Usage {
                                prompt_tokens,
                                completion_tokens,
                                decode_ms,
                            } => {
                                let _ = ui_tx.send(StreamStepUpdate::Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    decode_ms,
                                });
                            }
                            ChatEvent::Notice(msg) => {
                                let _ = ui_tx.send(StreamStepUpdate::Notice(msg));
                            }
                            ChatEvent::Error(err) => {
                                let _ = ui_tx.send(StreamStepUpdate::Error(err));
                            }
                            ChatEvent::Done => {
                                done = true;
                                break;
                            }
                        }
                    }

                    if done {
                        let _ = ui_tx.send(StreamStepUpdate::Done {
                            step_answer,
                            full_reasoning,
                        });
                        break;
                    }

                    if dirty && last_flush.elapsed() >= STREAM_UI_FLUSH_INTERVAL {
                        flush_snapshot(
                            &step_answer,
                            &full_reasoning,
                            &mut last_combined,
                            &mut code_cache,
                        );
                        dirty = false;
                        last_flush = Instant::now();
                    }
                }
                None => {
                    if dirty {
                        flush_snapshot(
                            &step_answer,
                            &full_reasoning,
                            &mut last_combined,
                            &mut code_cache,
                        );
                        dirty = false;
                        last_flush = Instant::now();
                    }
                }
            }
        }
    });
    ui_rx
}

fn maybe_scroll_bottom(scroller: &gtk::ScrolledWindow) {
    let vadj = scroller.vadjustment();
    let near_bottom = vadj.value() + vadj.page_size() >= vadj.upper() - 120.0;
    if near_bottom {
        scroll_scroller_to_bottom(scroller);
    }
}

fn scroll_scroller_to_bottom(scroller: &gtk::ScrolledWindow) {
    let vadj = scroller.vadjustment();
    let target = (vadj.upper() - vadj.page_size()).max(0.0);
    vadj.set_value(target);
}

fn parse_model_params(json_str: &str) -> InferenceParams {
    let mut params = InferenceParams::default();
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
        if let Some(ctx) = val.get("n_ctx").and_then(|v| v.as_u64()) {
            params.n_ctx = ctx as u32;
        }
        if let Some(gpu) = val.get("n_gpu_layers").and_then(|v| v.as_u64()) {
            params.n_gpu_layers = gpu as u32;
        }
        if let Some(temp) = val.get("temperature").and_then(|v| v.as_f64()) {
            params.temperature = temp as f32;
        }
    }
    params
}

/// Request permission if needed, execute a single folder/file tool off-thread, and build its record.
async fn run_chat_tool_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    permissions: &PermissionEngine,
    granted_folders: &[GrantedFolder],
    conv_id: &str,
    call: crate::agent::parser::ParsedToolCall,
) -> (ToolCallRecord, String, Option<PathBuf>) {
    let ungranted_target =
        requested_ungranted_path(granted_folders, &call.name, &call.arguments);
    let decision = if ungranted_target.is_none()
        && (permissions.is_auto_allowed(conv_id, &call.name)
            || is_write_already_granted(granted_folders, &call.name, &call.arguments))
    {
        PermissionDecision::AllowOnce
    } else {
        let preview = describe_tool_action(granted_folders, &call.name, &call.arguments);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_tool_permission_dialog(parent, &call.name, &preview, move |d| {
            let _ = ptx.send(d);
        });
        let d = prx.await.unwrap_or(PermissionDecision::Deny);
        if ungranted_target.is_none() {
            permissions.record_decision(conv_id, &call.name, d);
        }
        d
    };

    let decision_str = match decision {
        PermissionDecision::Deny => "deny",
        PermissionDecision::AllowOnce => "allow_once",
        PermissionDecision::AllowChat => "allow_chat",
    };

    let (status, output) = if decision == PermissionDecision::Deny {
        ("denied".to_string(), "User denied permission.".to_string())
    } else {
        stream_obj.set_status(Some(&tr_f("Running {tool}…", &[("tool", &call.name)])));
        let tname = call.name.clone();
        let targs = call.arguments.clone();
        let mut grants = granted_folders.to_vec();
        let needs_write = matches!(
            call.name.as_str(),
            "write_file"
                | "delete_file"
                | "create_directory"
                | "move_path"
                | "delete_directory"
        );
        if let Some(ref target) = ungranted_target {
            grants.push(GrantedFolder {
                path: target.clone(),
                allow_write: needs_write,
            });
        } else if needs_write {
            for g in &mut grants {
                g.allow_write = true;
            }
        }
        let (ttx, trx) = tokio::sync::oneshot::channel();
        runtime().spawn_blocking(move || {
            let _ = ttx.send(execute_tool_with_grants(&grants, &tname, &targs));
        });
        match trx.await {
            Ok(Ok(out)) => ("ok".to_string(), out),
            Ok(Err(err)) => ("error".to_string(), err.to_string()),
            Err(_) => ("error".to_string(), "Tool task cancelled".to_string()),
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision_str.to_string(),
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output, ungranted_target)
}

/// Inspect, ask user approval (with editable query), and run `web_search` off-thread.
async fn run_web_search_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    db: &Database,
    web_search_enabled: bool,
    local_texts: &[String],
    call: crate::agent::parser::ParsedToolCall,
) -> (ToolCallRecord, String) {
    let proposed_query = call
        .arguments
        .get("query")
        .or_else(|| call.arguments.get("q"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
    let (status, output, decision_str) = if !global_enabled || !web_search_enabled {
        (
            "denied".to_string(),
            "Web search is disabled for this chat.".to_string(),
            "deny".to_string(),
        )
    } else if proposed_query.is_empty() {
        (
            "error".to_string(),
            "Missing 'query' argument".to_string(),
            "deny".to_string(),
        )
    } else {
        let engine = db.get_search_engine().unwrap_or_default();
        let searxng_url = db.get_searxng_url().ok().flatten();
        let warnings = inspect_search_query(&proposed_query, local_texts);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_web_search_confirm_dialog(
            parent,
            &engine.label(),
            &proposed_query,
            &warnings,
            move |approved_query| {
                let _ = ptx.send(approved_query);
            },
        );
        match prx.await.unwrap_or(None) {
            None => (
                "denied".to_string(),
                "User denied web search.".to_string(),
                "deny".to_string(),
            ),
            Some(final_query) => {
                stream_obj.set_status(Some(&tr_f(
                    "Searching {engine}…",
                    &[("engine", &engine.label())],
                )));
                let (ttx, trx) = tokio::sync::oneshot::channel();
                runtime().spawn(async move {
                    let res =
                        execute_web_search(engine, searxng_url.as_deref(), &final_query).await;
                    let _ = ttx.send(res);
                });
                match trx.await {
                    Ok(Ok(out)) => ("ok".to_string(), out, "allow_once".to_string()),
                    Ok(Err(err)) => ("error".to_string(), err.to_string(), "allow_once".to_string()),
                    Err(_) => (
                        "error".to_string(),
                        "Web search task cancelled".to_string(),
                        "allow_once".to_string(),
                    ),
                }
            }
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision_str,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output)
}

/// Validate, ask user approval (with editable HTTPS URL), and fetch a web page off-thread.
async fn run_fetch_url_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    db: &Database,
    web_search_enabled: bool,
    local_texts: &[String],
    call: crate::agent::parser::ParsedToolCall,
) -> (ToolCallRecord, String) {
    let proposed_url = call
        .arguments
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
    let (status, output, decision_str) = if !global_enabled || !web_search_enabled {
        (
            "denied".to_string(),
            "Web search is disabled for this chat.".to_string(),
            "deny".to_string(),
        )
    } else if proposed_url.is_empty() {
        (
            "error".to_string(),
            "Missing 'url' argument".to_string(),
            "deny".to_string(),
        )
    } else if let Err(err) = validate_fetch_url(&proposed_url) {
        (
            "error".to_string(),
            err.to_string(),
            "deny".to_string(),
        )
    } else {
        let warnings = inspect_fetch_url(&proposed_url, local_texts);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_fetch_url_confirm_dialog(parent, &proposed_url, &warnings, move |approved_url| {
            let _ = ptx.send(approved_url);
        });
        match prx.await.unwrap_or(None) {
            None => (
                "denied".to_string(),
                "User denied web page fetch.".to_string(),
                "deny".to_string(),
            ),
            Some(final_url) => {
                if let Err(err) = validate_fetch_url(&final_url) {
                    ("error".to_string(), err.to_string(), "deny".to_string())
                } else {
                    stream_obj.set_status(Some(&gettext("Fetching web page…")));
                    let (ttx, trx) = tokio::sync::oneshot::channel();
                    runtime().spawn(async move {
                        let res = execute_fetch_url(&final_url).await;
                        let _ = ttx.send(res);
                    });
                    match trx.await {
                        Ok(Ok(out)) => ("ok".to_string(), out, "allow_once".to_string()),
                        Ok(Err(err)) => ("error".to_string(), err.to_string(), "allow_once".to_string()),
                        Err(_) => (
                            "error".to_string(),
                            "Fetch task cancelled".to_string(),
                            "allow_once".to_string(),
                        ),
                    }
                }
            }
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision_str,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output)
}

/// Fetch a conversation page from SQLite and parse all Markdown/syntect blocks in parallel across CPU cores.
fn load_and_parse_page(
    db: &Database,
    conv_id: &str,
    before_created_at: Option<i64>,
    sys_prompt_len: usize,
) -> Result<LoadedPageData, String> {
    let messages = db
        .list_messages_page(conv_id, before_created_at, PAGE_SIZE)
        .map_err(|e| e.to_string())?;
    let has_more_pages = messages.len() == PAGE_SIZE;
    let oldest_loaded_ts = messages.first().map(|m| m.created_at);

    let msg_ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
    let mut att_map = db.list_attachments_for_messages(&msg_ids).unwrap_or_default();
    let mut tc_map = db.list_tool_calls_for_messages(&msg_ids).unwrap_or_default();

    let mut approx_chars = sys_prompt_len;
    for m in &messages {
        approx_chars += m.content.len();
        if let Some(atts) = att_map.get(&m.id) {
            for a in atts {
                if let Some(t) = &a.extracted_text {
                    approx_chars += t.len();
                }
            }
        }
    }
    let est_tokens = if messages.is_empty() {
        0
    } else {
        (approx_chars / 4).max(1) as u32
    };

    // Parse assistant Markdown & syntect code blocks in parallel across CPU cores.
    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(messages.len().max(1));
    let chunk_size = messages.len().div_ceil(worker_count).max(1);

    let mut parsed_blocks: Vec<Vec<crate::core::markdown::MarkdownBlock>> =
        vec![Vec::new(); messages.len()];

    if !messages.is_empty() {
        std::thread::scope(|s| {
            for (msg_chunk, out_chunk) in messages
                .chunks(chunk_size)
                .zip(parsed_blocks.chunks_mut(chunk_size))
            {
                s.spawn(move || {
                    for (m, slot) in msg_chunk.iter().zip(out_chunk.iter_mut()) {
                        *slot = if m.role == "assistant" {
                            crate::core::markdown::parse_markdown_blocks(&m.content)
                        } else {
                            vec![crate::core::markdown::MarkdownBlock::Text(
                                glib::markup_escape_text(&m.content).to_string(),
                            )]
                        };
                    }
                });
            }
        });
    }

    let items = messages
        .into_iter()
        .zip(parsed_blocks)
        .map(|(m, blocks)| {
            let atts = att_map.remove(&m.id).unwrap_or_default();
            let tcs = tc_map.remove(&m.id).unwrap_or_default();
            (m, atts, tcs, blocks)
        })
        .collect();

    Ok(LoadedPageData {
        items,
        has_more_pages,
        oldest_loaded_ts,
        est_tokens,
    })
}
