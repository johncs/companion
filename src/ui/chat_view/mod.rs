//! Virtualized chat stream (`gtk::ListView`) with paginated SQLite loading, composer, and `llama.cpp` streaming.

mod agent_runner;
mod approval;
mod message_actions;
mod page_loader;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio};
use relm4::prelude::*;

use crate::agent::permissions::PermissionEngine;
use crate::core::prompt::default_system_prompt;
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::{gettext, tr_f};
use crate::inference::llama_cpp::LlamaCppProvider;
use crate::storage::{AttachmentRecord, Database};
use crate::ui::composer::{ComposerModel, ComposerMsg, ComposerOutput};
use crate::ui::message_object::MessageObject;
use crate::ui::message_row::build_message_list_factory;

pub use page_loader::LoadedPageData;

use page_loader::load_and_parse_page;
use agent_runner::ActiveChatRun;

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
            Rc::new(move |message_id, new_content| {
                let _ = edit_sender.send(ChatViewMsg::EditAndResendMessage {
                    message_id,
                    new_content,
                });
            }),
            Rc::new(move |message_id| {
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

        let model = Self {
            db,
            provider: LlamaCppProvider::new(),
            permissions: PermissionEngine::new(),
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

        let vadj = scrolled_window.vadjustment();
        let sender_clone = sender.clone();
        vadj.connect_value_changed(move |adj| {
            if adj.value() < 80.0 && adj.upper() > adj.page_size() {
                sender_clone.input(ChatViewMsg::LoadOlderPage);
            }
        });

        attach_drop_overlay(&widgets.root_overlay, &widgets.drop_overlay, model.composer.sender());
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ChatViewMsg::LoadConversation {
                conversation_id,
                title,
            } => self.on_load_conversation(conversation_id, title, &sender),
            ChatViewMsg::ConversationPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => self.on_page_loaded(conversation_id, request_seq, result, &sender),
            ChatViewMsg::ClearConversation => self.on_clear_conversation(&sender),
            ChatViewMsg::CancelConversation(id) => {
                self.cancel_run_for(&id, &sender);
                if self.active_conversation_id.as_deref() == Some(id.as_str()) {
                    self.composer.emit(ComposerMsg::SetGenerating(false));
                }
            }
            ChatViewMsg::LoadOlderPage => self.load_older_page(&sender),
            ChatViewMsg::OlderPageLoaded {
                conversation_id,
                request_seq,
                result,
            } => self.on_older_page_loaded(conversation_id, request_seq, result),
            ChatViewMsg::OpenFolderPermissions => self.on_open_folder_permissions(&sender),
            ChatViewMsg::GrantFolder(path) => self.handle_grant_folder(path, &sender),
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
            } => self.handle_send_message(text, thinking_level, attachments, &sender),
            ChatViewMsg::EditAndResendMessage {
                message_id,
                new_content,
            } => self.handle_edit_and_resend(message_id, new_content, &sender),
            ChatViewMsg::DeleteMessageFrom(message_id) => {
                self.handle_delete_from(message_id, &sender);
            }
            ChatViewMsg::StopGeneration => self.cancel_active_stream(&sender),
            ChatViewMsg::SystemPromptChanged(prompt) => {
                self.active_system_prompt = prompt.clone();
                if let Some(conv_id) = &self.active_conversation_id {
                    let _ = self.db.set_conversation_system_prompt(conv_id, Some(&prompt));
                }
            }
            ChatViewMsg::ThinkingLevelChanged(level) => {
                self.active_thinking_level = level;
                if let Some(conv_id) = &self.active_conversation_id {
                    let _ = self.db.set_conversation_thinking_level(conv_id, level);
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
            ChatViewMsg::RefreshModels(id) => self.on_refresh_models(id, &sender),
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
                let _ = sender.output(ChatViewOutput::OpenModelSettings);
            }
            ChatViewMsg::ShowToast(msg) => {
                let _ = sender.output(ChatViewOutput::ShowToast(msg));
            }
        }
    }
}

impl ChatViewModel {
    fn on_load_conversation(
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

    fn on_page_loaded(
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

    fn on_clear_conversation(&mut self, sender: &ComponentSender<Self>) {
        self.cancel_active_stream(sender);
        self.active_conversation_id = None;
        *self.visible_conv_id.borrow_mut() = None;
        self.store.remove_all();
        self.is_loading = false;
        self.is_empty = true;
        self.web_search_enabled = false;
        self.composer.emit(ComposerMsg::SetWebSearchEnabled(false));
    }

    fn on_older_page_loaded(
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

    fn on_open_folder_permissions(&self, sender: &ComponentSender<Self>) {
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

    fn on_refresh_models(&mut self, id: Option<String>, sender: &ComponentSender<Self>) {
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

    fn on_personalization_changed(
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

fn attach_drop_overlay(
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
