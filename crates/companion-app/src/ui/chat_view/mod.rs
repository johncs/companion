//! Virtualized chat stream (`gtk::ListView`) with paginated SQLite loading, composer, and `llama.cpp` streaming.

mod actions;
mod agent_runner;
mod approval;
mod drop_overlay;
mod handlers;
mod message_actions;
mod page_loader;
mod prompt_prep;
mod setup;
mod state;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::gio;
use relm4::prelude::*;

use crate::agent::permissions::PermissionEngine;
use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;
use crate::i18n::gettext;
use crate::services::AppServices;
use crate::ui::composer::ComposerModel;
use crate::ui::message_object::MessageObject;

pub use page_loader::LoadedPageData;
pub use setup::{ChatViewMsg, ChatViewOutput};

use agent_runner::ActiveChatRun;
use drop_overlay::attach_drop_overlay;
use setup::{build_chat_composer, build_chat_list_factory};

pub struct ChatViewModel {
    services: Arc<AppServices>,
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
    oldest_loaded_id: Option<String>,
    has_more_pages: bool,
    is_loading_older: bool,
    view_state: state::ChatViewState,
    is_loading: bool,
    is_empty: bool,
    is_sending: bool,
    next_run_id: u64,
    active_runs: Rc<RefCell<HashMap<String, ActiveChatRun>>>,
}

#[relm4::component(pub)]
impl SimpleComponent for ChatViewModel {
    type Init = (Arc<AppServices>, Option<String>);
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
                        },

                        add_overlay = model.composer.widget(),
                    },
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
        (services, active_model_id): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let store = gio::ListStore::new::<MessageObject>();
        let assistant_name = Rc::new(RefCell::new(
            crate::core::types::DEFAULT_ASSISTANT_NAME.to_string(),
        ));
        let user_name = Rc::new(RefCell::new(None));
        let factory = build_chat_list_factory(assistant_name.clone(), user_name.clone(), &sender);
        let composer = build_chat_composer(services.clone(), active_model_id.clone(), &sender);
        spawn_personalization_loader(&services, &sender);

        let scrolled_window = gtk::ScrolledWindow::new();
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let message_list = gtk::ListView::new(Some(selection), Some(factory));
        let model = Self::new_model(
            services,
            store,
            (scrolled_window.clone(), message_list.clone()),
            composer,
            active_model_id,
            (assistant_name, user_name),
        );
        let widgets = view_output!();
        connect_scroll_pagination(&scrolled_window, &sender);
        sync_floating_composer_padding(model.composer.widget(), &scrolled_window);
        attach_drop_overlay(
            &widgets.root_overlay,
            &widgets.drop_overlay,
            model.composer.sender(),
        );
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ChatViewMsg::LoadConversation { .. }
            | ChatViewMsg::ConversationPageLoaded { .. }
            | ChatViewMsg::ClearConversation
            | ChatViewMsg::CancelConversation(_)
            | ChatViewMsg::LoadOlderPage
            | ChatViewMsg::OlderPageLoaded { .. }
            | ChatViewMsg::OpenFolderPermissions
            | ChatViewMsg::GrantFolder(_)
            | ChatViewMsg::PresentGrantFolderDialog { .. }
            | ChatViewMsg::RefreshGrantedFolders
            | ChatViewMsg::GrantedFoldersLoaded { .. } => {
                self.handle_nav_or_folder_msg(msg, &sender);
            }
            ChatViewMsg::SendMessage { .. }
            | ChatViewMsg::SendPrepared { .. }
            | ChatViewMsg::EditAndResendMessage { .. }
            | ChatViewMsg::EditPrepared(_)
            | ChatViewMsg::DeleteMessageFrom(_)
            | ChatViewMsg::DeleteCompleted { .. }
            | ChatViewMsg::StopGeneration => self.handle_message_action_msg(msg, &sender),
            _ => self.handle_setting_or_ui_msg(msg, &sender),
        }
    }
}

fn spawn_personalization_loader(
    services: &Arc<AppServices>,
    sender: &ComponentSender<ChatViewModel>,
) {
    let db = services.db.clone();
    let input_sender = sender.input_sender().clone();
    services.rt_handle.spawn_blocking(move || {
        let assistant_name = db
            .get_assistant_name()
            .unwrap_or_else(|_| crate::core::types::DEFAULT_ASSISTANT_NAME.to_string());
        let user_name = db.get_user_name().ok().flatten();
        let _ = input_sender.send(ChatViewMsg::PersonalizationChanged {
            assistant_name,
            user_name,
        });
    });
}

fn connect_scroll_pagination(
    scrolled_window: &gtk::ScrolledWindow,
    sender: &ComponentSender<ChatViewModel>,
) {
    let vadj = scrolled_window.vadjustment();
    let sender_clone = sender.clone();
    vadj.connect_value_changed(move |adj| {
        if adj.value() < 80.0 && adj.upper() > adj.page_size() {
            sender_clone.input(ChatViewMsg::LoadOlderPage);
        }
    });
}

fn sync_floating_composer_padding(
    composer_widget: &adw::Clamp,
    scrolled_window: &gtk::ScrolledWindow,
) {
    composer_widget.set_halign(gtk::Align::Fill);
    composer_widget.set_valign(gtk::Align::End);
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let last_height = std::cell::Cell::new(0i32);
    let scroller_weak = scrolled_window.downgrade();
    composer_widget.add_tick_callback(move |w, _| {
        let h = w.height();
        if h > 0 && h != last_height.get() {
            let was_at_bottom = scroller_weak.upgrade().is_some_and(|s| {
                let adj = s.vadjustment();
                adj.upper() - (adj.value() + adj.page_size()) < 48.0
            });
            last_height.set(h);
            provider.load_from_string(&format!(
                "listview.chat-list {{ padding-bottom: {}px; }}",
                h + 8
            ));
            if was_at_bottom && let Some(s) = scroller_weak.upgrade() {
                page_loader::scroll_scroller_to_bottom(&s);
            }
        }
        gtk::glib::ControlFlow::Continue
    });
}

impl ChatViewModel {
    fn new_model(
        services: Arc<AppServices>,
        store: gio::ListStore,
        (scrolled_window, message_list): (gtk::ScrolledWindow, gtk::ListView),
        composer: Controller<ComposerModel>,
        active_model_id: Option<String>,
        (assistant_name, user_name): (Rc<RefCell<String>>, Rc<RefCell<Option<String>>>),
    ) -> Self {
        Self {
            services,
            permissions: PermissionEngine::new(),
            store,
            scrolled_window,
            message_list,
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
            oldest_loaded_id: None,
            has_more_pages: false,
            is_loading_older: false,
            view_state: state::ChatViewState::default(),
            is_loading: false,
            is_empty: true,
            is_sending: false,
            next_run_id: 0,
            active_runs: Rc::new(RefCell::new(HashMap::new())),
        }
    }
}
