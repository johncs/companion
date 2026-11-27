//! Sidebar component: live FTS5 search, chat list, and header actions.

mod actions;
mod view_model;
mod widgets;

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::i18n::gettext;
use crate::services::AppServices;
use crate::storage::Conversation;
use crate::ui::sidebar_rows::{build_conversation_row, new_nav_list};
use crate::ui::widgets::clear_list_box;

use widgets::{
    build_section_box, build_sidebar_empty_row, build_sidebar_error_row, build_sidebar_loading_row,
};

#[derive(Debug)]
pub enum SidebarMsg {
    SearchChanged(String),
    ApplySearch,
    SearchResultsLoaded {
        request_seq: u64,
        result: Result<Vec<crate::storage::Conversation>, String>,
    },
    NewChat,
    ChatCreated(Result<crate::storage::Conversation, String>),
    SelectConversation(String),
    RenameConversation {
        id: String,
        title: String,
    },
    ChatRenamed {
        id: String,
        title: String,
        result: Result<(), String>,
    },
    DuplicateConversation(String),
    ChatDuplicated(Result<Option<crate::storage::Conversation>, String>),
    DeleteConversation(String),
    ChatDeleted {
        id: String,
        result: Result<(), String>,
    },
    ChatsLoaded {
        request_seq: u64,
        result: Result<Vec<crate::storage::Conversation>, String>,
    },
    SetConversationRunning {
        id: String,
        is_running: bool,
    },
    RefreshLists,
    FocusSearch,
}

#[derive(Debug)]
pub enum SidebarOutput {
    ConversationSelected(String),
    ConversationRenamed { id: String, title: String },
    ConversationDeleted(String),
    ShowToast(String),
}

pub struct SidebarModel {
    services: Arc<AppServices>,
    search_query: String,
    search_debounce: Option<glib::SourceId>,
    list_seq: u64,
    active_conversation_id: Option<String>,
    vm: view_model::SidebarViewModel,
    chats_list: gtk::ListBox,
    search_list: gtk::ListBox,
    search_section: gtk::Box,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
}

#[relm4::component(pub)]
impl SimpleComponent for SidebarModel {
    type Init = Arc<AppServices>;
    type Input = SidebarMsg;
    type Output = SidebarOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                pack_start = &gtk::Button {
                    set_icon_name: "list-add-symbolic",
                    set_tooltip_text: Some(&gettext("New Chat")),
                    connect_clicked => SidebarMsg::NewChat,
                },
                #[name = "search_toggle"]
                pack_start = &gtk::ToggleButton {
                    set_icon_name: "system-search-symbolic",
                    set_tooltip_text: Some(&gettext("Search chats")),
                },
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Companion",
                },
                pack_end = &gtk::MenuButton {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some(&gettext("Main Menu")),
                    set_menu_model: Some(&build_primary_menu()),
                },
            },

            #[name = "search_bar"]
            add_top_bar = &gtk::SearchBar {
                #[wrap(Some)]
                #[name = "search_entry"]
                set_child = &gtk::SearchEntry {
                    set_hexpand: true,
                    set_placeholder_text: Some(&gettext("Search chats…")),
                    connect_search_changed[sender] => move |entry| {
                        sender.input(SidebarMsg::SearchChanged(entry.text().to_string()));
                    },
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_margin_top: 8,
                        set_margin_bottom: 16,
                        set_margin_start: 8,
                        set_margin_end: 8,

                        #[local_ref]
                        search_section -> gtk::Box {
                            set_visible: false,
                        },

                        #[local_ref]
                        chats_list -> gtk::ListBox {},
                    }
                }
            }
        }
    }

    fn init(
        services: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let chats_list = new_nav_list();
        let search_list = new_nav_list();
        connect_list_activation(&[&chats_list, &search_list], &sender);
        let search_section = build_section_box(&gettext("Search Results"), &search_list);

        let widgets = view_output!();
        connect_search_bar(
            &widgets.search_bar,
            &widgets.search_entry,
            &widgets.search_toggle,
        );

        let mut model = Self {
            services,
            search_query: String::new(),
            search_debounce: None,
            list_seq: 0,
            active_conversation_id: None,
            vm: view_model::SidebarViewModel::default(),
            chats_list,
            search_list,
            search_section,
            search_bar: widgets.search_bar.clone(),
            search_entry: widgets.search_entry.clone(),
        };
        model.rebuild_lists(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            SidebarMsg::SearchChanged(query) => self.on_search_changed(query, &sender),
            SidebarMsg::ApplySearch => {
                self.search_debounce = None;
                self.rebuild_lists(&sender);
            }
            SidebarMsg::SearchResultsLoaded {
                request_seq,
                result,
            } => {
                self.on_search_results_loaded(request_seq, result, &sender);
            }
            SidebarMsg::NewChat => self.on_new_chat(&sender),
            SidebarMsg::ChatCreated(res) => self.on_chat_created(res, &sender),
            SidebarMsg::SelectConversation(conv_id) => {
                self.active_conversation_id = Some(conv_id.clone());
                self.rebuild_lists(&sender);
                let _ = sender.output(SidebarOutput::ConversationSelected(conv_id));
            }
            SidebarMsg::RenameConversation { id, title } => {
                self.on_rename_conversation(id, title, &sender)
            }
            SidebarMsg::ChatRenamed { id, title, result } => {
                self.on_chat_renamed(id, title, result, &sender)
            }
            SidebarMsg::DuplicateConversation(conv_id) => {
                self.on_duplicate_conversation(conv_id, &sender)
            }
            SidebarMsg::ChatDuplicated(res) => self.on_chat_duplicated(res, &sender),
            SidebarMsg::DeleteConversation(conv_id) => {
                self.on_delete_conversation(conv_id, &sender)
            }
            SidebarMsg::ChatDeleted { id, result } => self.on_chat_deleted(id, result, &sender),
            SidebarMsg::ChatsLoaded {
                request_seq,
                result,
            } => self.on_chats_loaded(request_seq, result, &sender),
            SidebarMsg::SetConversationRunning { id, is_running } => {
                if self.vm.set_running(id, is_running) {
                    self.rebuild_lists(&sender);
                }
            }
            SidebarMsg::RefreshLists => self.rebuild_lists(&sender),
            SidebarMsg::FocusSearch => {
                self.search_bar.set_search_mode(true);
                self.search_entry.grab_focus();
            }
        }
    }
}

impl SidebarModel {
    fn on_search_changed(&mut self, query: String, sender: &ComponentSender<Self>) {
        self.search_query = query;
        if let Some(source_id) = self.search_debounce.take() {
            source_id.remove();
        }
        if self.search_query.trim().is_empty() {
            self.rebuild_lists(sender);
        } else {
            let sender_clone = sender.clone();
            self.search_debounce = Some(glib::timeout_add_local_once(
                std::time::Duration::from_millis(150),
                move || sender_clone.input(SidebarMsg::ApplySearch),
            ));
        }
    }

    fn on_search_results_loaded(
        &self,
        request_seq: u64,
        result: Result<Vec<Conversation>, String>,
        sender: &ComponentSender<Self>,
    ) {
        if !self.vm.is_current_search(request_seq) || self.search_query.trim().is_empty() {
            return;
        }
        clear_list_box(&self.search_list);
        match result {
            Ok(convs) if convs.is_empty() => {
                self.search_list.append(&build_sidebar_empty_row(&gettext(
                    "No matching conversations",
                )));
            }
            Ok(convs) => {
                for conv in convs {
                    let is_active = self.active_conversation_id.as_deref() == Some(&conv.id);
                    let is_running = self.vm.running_conversations.contains(&conv.id);
                    self.search_list.append(&build_conversation_row(
                        &conv, is_active, is_running, sender,
                    ));
                }
            }
            Err(err) => {
                tracing::error!("Search failed: {err}");
                let sender_clone = sender.clone();
                self.search_list.append(&build_sidebar_error_row(
                    &gettext("Failed to search chats"),
                    move || sender_clone.input(SidebarMsg::ApplySearch),
                ));
            }
        }
    }
}

fn connect_list_activation(lists: &[&gtk::ListBox], sender: &ComponentSender<SidebarModel>) {
    for list in lists {
        let sender = sender.clone();
        list.connect_row_activated(move |_, row| {
            let conv_id = row.widget_name();
            if !conv_id.is_empty() {
                sender.input(SidebarMsg::SelectConversation(conv_id.to_string()));
            }
        });
    }
}

fn connect_search_bar(bar: &gtk::SearchBar, entry: &gtk::SearchEntry, toggle: &gtk::ToggleButton) {
    bar.connect_entry(entry);
    bar.bind_property("search-mode-enabled", toggle, "active")
        .bidirectional()
        .sync_create()
        .build();
    let entry_c = entry.clone();
    bar.connect_search_mode_enabled_notify(move |bar| {
        if !bar.is_search_mode() && !entry_c.text().is_empty() {
            entry_c.set_text("");
        }
    });
}

impl SidebarModel {
    fn rebuild_lists(&mut self, sender: &ComponentSender<Self>) {
        clear_list_box(&self.chats_list);
        clear_list_box(&self.search_list);

        let trimmed = self.search_query.trim().to_string();
        if !trimmed.is_empty() {
            self.populate_search_results(&trimmed, sender);
            return;
        }

        self.search_section.set_visible(false);
        self.chats_list.set_visible(true);

        self.populate_chats(sender);
    }

    fn populate_search_results(&mut self, query: &str, sender: &ComponentSender<Self>) {
        self.search_section.set_visible(true);
        self.chats_list.set_visible(false);

        self.search_list
            .append(&build_sidebar_loading_row(&gettext("Searching…")));

        let req_seq = self.vm.begin_search(query.to_string());
        let db = self.services.db.clone();
        let query_owned = query.to_string();
        let input_sender = sender.input_sender().clone();

        self.services.rt_handle.spawn_blocking(move || {
            let result = db
                .search_conversations(&query_owned, 50)
                .map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::SearchResultsLoaded {
                request_seq: req_seq,
                result,
            });
        });
    }

    fn populate_chats(&mut self, sender: &ComponentSender<Self>) {
        self.list_seq = self.list_seq.wrapping_add(1);
        let req_seq = self.list_seq;
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();

        self.services.rt_handle.spawn_blocking(move || {
            let result = db.list_conversations(100).map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::ChatsLoaded {
                request_seq: req_seq,
                result,
            });
        });
    }
}

fn build_primary_menu() -> gtk::gio::Menu {
    let menu = gtk::gio::Menu::new();
    let main_sec = gtk::gio::Menu::new();
    main_sec.append(Some(&gettext("New Chat")), Some("win.new-chat"));
    main_sec.append(Some(&gettext("Preferences")), Some("win.preferences"));
    menu.append_section(None, &main_sec);
    let help_sec = gtk::gio::Menu::new();
    help_sec.append(
        Some(&gettext("Keyboard Shortcuts")),
        Some("win.show-help-overlay"),
    );
    help_sec.append(Some(&gettext("About Companion")), Some("win.about"));
    menu.append_section(None, &help_sec);
    menu
}
