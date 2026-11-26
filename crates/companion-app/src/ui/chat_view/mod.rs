//! Virtualized chat stream (`gtk::ListView`) with paginated SQLite loading, composer, and `llama.cpp` streaming.

mod agent_runner;
mod approval;
mod handlers;
mod message_actions;
mod page_loader;
mod setup;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use relm4::prelude::*;

use crate::agent::permissions::PermissionEngine;
use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;
use crate::i18n::gettext;
use crate::inference::llama_cpp::LlamaCppProvider;
use crate::storage::Database;
use crate::ui::composer::{ComposerModel, ComposerMsg};
use crate::ui::message_object::MessageObject;

pub use page_loader::LoadedPageData;
pub use setup::{ChatViewMsg, ChatViewOutput};

use agent_runner::ActiveChatRun;
use handlers::attach_drop_overlay;
use setup::{build_chat_composer, build_chat_list_factory};

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
    next_run_id: u64,
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
        let factory = build_chat_list_factory(assistant_name.clone(), user_name.clone(), &sender);
        let composer = build_chat_composer(db.clone(), active_model_id.clone(), &sender);

        let scrolled_window = gtk::ScrolledWindow::new();
        let message_list = gtk::ListView::new(Some(selection), Some(factory));

        let model = Self {
            db,
            provider: LlamaCppProvider::new(crate::core::runtime().handle().clone()),
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
            next_run_id: 0,
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

        attach_drop_overlay(
            &widgets.root_overlay,
            &widgets.drop_overlay,
            model.composer.sender(),
        );
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
            ChatViewMsg::PresentGrantFolderDialog {
                folder_name,
                folder_str,
            } => self.present_grant_folder_dialog(folder_name, folder_str, &sender),
            ChatViewMsg::RefreshGrantedFolders => {
                if let Some(conv_id) = self.active_conversation_id.clone() {
                    let db = self.db.clone();
                    let input_sender = sender.input_sender().clone();
                    crate::core::runtime().spawn_blocking(move || {
                        let folders = db.list_conversation_folders(&conv_id).unwrap_or_default();
                        let _ = input_sender
                            .send(ChatViewMsg::GrantedFoldersLoaded { conv_id, folders });
                    });
                }
            }
            ChatViewMsg::GrantedFoldersLoaded { conv_id, folders } => {
                if self.active_conversation_id.as_deref() == Some(conv_id.as_str()) {
                    self.composer.emit(ComposerMsg::SetGrantedFolders(folders));
                }
            }
            ChatViewMsg::SendMessage {
                text,
                thinking_level,
                attachments,
            } => self.handle_send_message(text, thinking_level, attachments, &sender),
            ChatViewMsg::SendPrepared {
                conv_id,
                thinking_level,
                result,
            } => self.handle_send_prepared(conv_id, thinking_level, result, &sender),
            ChatViewMsg::EditAndResendMessage {
                message_id,
                new_content,
            } => self.handle_edit_and_resend(message_id, new_content, &sender),
            ChatViewMsg::EditPrepared(payload) => {
                self.handle_edit_prepared(payload, &sender);
            }
            ChatViewMsg::DeleteMessageFrom(message_id) => {
                self.handle_delete_from(message_id, &sender);
            }
            ChatViewMsg::DeleteCompleted {
                conv_id,
                pos,
                result,
            } => self.handle_delete_completed(conv_id, pos, result, &sender),
            ChatViewMsg::StopGeneration => self.cancel_active_stream(&sender),
            ChatViewMsg::SystemPromptChanged(prompt) => {
                self.active_system_prompt = prompt.clone();
                if let Some(conv_id) = self.active_conversation_id.clone() {
                    let db = self.db.clone();
                    crate::core::runtime().spawn_blocking(move || {
                        let _ = db.set_conversation_system_prompt(&conv_id, Some(&prompt));
                    });
                }
            }
            ChatViewMsg::ThinkingLevelChanged(level) => {
                self.active_thinking_level = level;
                if let Some(conv_id) = self.active_conversation_id.clone() {
                    let db = self.db.clone();
                    crate::core::runtime().spawn_blocking(move || {
                        let _ = db.set_conversation_thinking_level(&conv_id, level);
                    });
                }
            }
            ChatViewMsg::WebSearchToggled(enabled) => {
                self.web_search_enabled = enabled;
                if let Some(conv_id) = self.active_conversation_id.clone() {
                    let db = self.db.clone();
                    crate::core::runtime().spawn_blocking(move || {
                        let _ = db.set_conversation_web_search_enabled(&conv_id, enabled);
                    });
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
