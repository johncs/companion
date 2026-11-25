//! Sidebar component: live FTS5 search, chat list, and header actions.

use std::collections::HashSet;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::storage::Database;
use crate::ui::sidebar_rows::{SIDEBAR_ICON_SIZE, build_conversation_row, new_nav_list};
use crate::ui::widgets::clear_list_box;

#[derive(Debug)]
pub enum SidebarMsg {
    SearchChanged(String),
    ApplySearch,
    SearchResultsLoaded {
        request_seq: u64,
        result: Result<Vec<crate::storage::Conversation>, String>,
    },
    NewChat,
    SelectConversation(String),
    RenameConversation { id: String, title: String },
    DuplicateConversation(String),
    DeleteConversation(String),
    SetConversationRunning { id: String, is_running: bool },
    RefreshLists,
    OpenSettings,
}

#[derive(Debug)]
pub enum SidebarOutput {
    ConversationSelected(String),
    ConversationRenamed { id: String, title: String },
    ConversationDeleted(String),
    OpenSettings,
    ShowToast(String),
}

pub struct SidebarModel {
    db: Database,
    search_query: String,
    search_debounce: Option<glib::SourceId>,
    search_seq: u64,
    active_conversation_id: Option<String>,
    running_conversations: HashSet<String>,
    chats_list: gtk::ListBox,
    search_list: gtk::ListBox,
    search_section: gtk::Box,
}

#[relm4::component(pub)]
impl SimpleComponent for SidebarModel {
    type Init = Database;
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
                pack_end = &gtk::Button {
                    set_icon_name: "preferences-system-symbolic",
                    set_tooltip_text: Some(&gettext("Settings")),
                    connect_clicked => SidebarMsg::OpenSettings,
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
        db: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let chats_list = new_nav_list();
        let search_list = new_nav_list();
        let search_section = build_section_box(&gettext("Search Results"), &search_list);

        let widgets = view_output!();
        widgets.search_bar.connect_entry(&widgets.search_entry);
        widgets
            .search_bar
            .bind_property("search-mode-enabled", &widgets.search_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        {
            let entry = widgets.search_entry.clone();
            widgets
                .search_bar
                .connect_search_mode_enabled_notify(move |bar| {
                    if !bar.is_search_mode() && !entry.text().is_empty() {
                        entry.set_text("");
                    }
                });
        }

        let mut model = Self {
            db,
            search_query: String::new(),
            search_debounce: None,
            search_seq: 0,
            active_conversation_id: None,
            running_conversations: HashSet::new(),
            chats_list,
            search_list,
            search_section,
        };
        model.rebuild_lists(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            SidebarMsg::SearchChanged(query) => {
                self.search_query = query;
                if let Some(source_id) = self.search_debounce.take() {
                    source_id.remove();
                }
                if self.search_query.trim().is_empty() {
                    self.rebuild_lists(&sender);
                } else {
                    let sender_clone = sender.clone();
                    self.search_debounce = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(150),
                        move || {
                            sender_clone.input(SidebarMsg::ApplySearch);
                        },
                    ));
                }
            }
            SidebarMsg::ApplySearch => {
                self.search_debounce = None;
                self.rebuild_lists(&sender);
            }
            SidebarMsg::SearchResultsLoaded { request_seq, result } => {
                if self.search_seq != request_seq || self.search_query.trim().is_empty() {
                    return;
                }
                clear_list_box(&self.search_list);
                match result {
                    Ok(convs) if convs.is_empty() => {
                        self.search_list
                            .append(&build_sidebar_empty_row(&gettext("No matching conversations")));
                    }
                    Ok(convs) => {
                        for conv in convs {
                            let is_active = self.active_conversation_id.as_deref() == Some(&conv.id);
                            let is_running = self.running_conversations.contains(&conv.id);
                            self.search_list.append(&build_conversation_row(
                                &conv,
                                is_active,
                                is_running,
                                &sender,
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
            SidebarMsg::NewChat => {
                let title = gettext("New Chat");
                match self.db.create_conversation(&title) {
                    Ok(conv) => {
                        self.active_conversation_id = Some(conv.id.clone());
                        self.rebuild_lists(&sender);
                        let _ = sender.output(SidebarOutput::ConversationSelected(conv.id));
                    }
                    Err(err) => {
                        let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                            "Failed to create chat: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                }
            }
            SidebarMsg::SelectConversation(conv_id) => {
                self.active_conversation_id = Some(conv_id.clone());
                self.rebuild_lists(&sender);
                let _ = sender.output(SidebarOutput::ConversationSelected(conv_id));
            }
            SidebarMsg::RenameConversation { id, title } => {
                if let Err(err) = self.db.rename_conversation(&id, &title) {
                    let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                        "Failed to rename chat: {err}",
                        &[("err", &err.to_string())],
                    )));
                } else {
                    self.rebuild_lists(&sender);
                    let _ = sender.output(SidebarOutput::ConversationRenamed { id, title });
                }
            }
            SidebarMsg::DuplicateConversation(conv_id) => {
                match self.db.duplicate_conversation(&conv_id) {
                    Ok(Some(copy)) => {
                        self.active_conversation_id = Some(copy.id.clone());
                        self.rebuild_lists(&sender);
                        let _ = sender.output(SidebarOutput::ConversationSelected(copy.id));
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                            "Failed to duplicate chat: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                }
            }
            SidebarMsg::DeleteConversation(conv_id) => {
                if let Err(err) = self.db.delete_conversation(&conv_id) {
                    let _ = sender.output(SidebarOutput::ShowToast(tr_f(
                        "Failed to delete chat: {err}",
                        &[("err", &err.to_string())],
                    )));
                } else {
                    self.running_conversations.remove(&conv_id);
                    if self.active_conversation_id.as_deref() == Some(&conv_id) {
                        self.active_conversation_id = None;
                    }
                    self.rebuild_lists(&sender);
                    let _ = sender.output(SidebarOutput::ConversationDeleted(conv_id));
                }
            }
            SidebarMsg::SetConversationRunning { id, is_running } => {
                let changed = if is_running {
                    self.running_conversations.insert(id)
                } else {
                    self.running_conversations.remove(&id)
                };
                if changed {
                    self.rebuild_lists(&sender);
                }
            }
            SidebarMsg::RefreshLists => {
                self.rebuild_lists(&sender);
            }
            SidebarMsg::OpenSettings => {
                let _ = sender.output(SidebarOutput::OpenSettings);
            }
        }
    }
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

        self.search_seq = self.search_seq.wrapping_add(1);
        let req_seq = self.search_seq;
        let db = self.db.clone();
        let query_owned = query.to_string();
        let input_sender = sender.input_sender().clone();

        crate::core::runtime().spawn_blocking(move || {
            let result = db
                .search_conversations(&query_owned, 50)
                .map_err(|e| e.to_string());
            let _ = input_sender.send(SidebarMsg::SearchResultsLoaded {
                request_seq: req_seq,
                result,
            });
        });
    }

    fn populate_chats(&self, sender: &ComponentSender<Self>) {
        match self.db.list_conversations(100) {
            Ok(convs) => {
                for conv in convs {
                    let is_active = self.active_conversation_id.as_deref() == Some(&conv.id);
                    let is_running = self.running_conversations.contains(&conv.id);
                    self.chats_list.append(&build_conversation_row(
                        &conv,
                        is_active,
                        is_running,
                        sender,
                    ));
                }
            }
            Err(err) => {
                tracing::error!("Failed to list chats: {err}");
                let sender_clone = sender.clone();
                self.chats_list.append(&build_sidebar_error_row(
                    &gettext("Failed to load chats"),
                    move || sender_clone.input(SidebarMsg::RefreshLists),
                ));
            }
        }
    }
}

fn build_section_box(title: &str, body: &impl IsA<gtk::Widget>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(8)
        .margin_end(4)
        .build();
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading", "dim-label"])
        .build();
    header.append(&label);
    section.append(&header);
    section.append(body);
    section
}

fn build_sidebar_empty_row(text: &str) -> gtk::ListBoxRow {
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .margin_start(4)
        .css_classes(["dim-label", "caption"])
        .build();
    gtk::ListBoxRow::builder()
        .child(&lbl)
        .activatable(false)
        .selectable(false)
        .sensitive(false)
        .build()
}

fn build_sidebar_loading_row(text: &str) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(4)
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(SIDEBAR_ICON_SIZE)
        .height_request(SIDEBAR_ICON_SIZE)
        .valign(gtk::Align::Center)
        .build();
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    box_.append(&spinner);
    box_.append(&lbl);
    gtk::ListBoxRow::builder()
        .child(&box_)
        .activatable(false)
        .selectable(false)
        .build()
}

fn build_sidebar_error_row(text: &str, on_retry: impl Fn() + 'static) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(4)
        .build();
    let icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .pixel_size(SIDEBAR_ICON_SIZE)
        .css_classes(["warning"])
        .build();
    let lbl = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption"])
        .build();
    let retry_btn = gtk::Button::builder()
        .child(
            &gtk::Image::builder()
                .icon_name("view-refresh-symbolic")
                .pixel_size(SIDEBAR_ICON_SIZE)
                .build(),
        )
        .tooltip_text(gettext("Retry"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    retry_btn.connect_clicked(move |_| on_retry());
    box_.append(&icon);
    box_.append(&lbl);
    box_.append(&retry_btn);
    gtk::ListBoxRow::builder()
        .child(&box_)
        .activatable(false)
        .selectable(false)
        .build()
}
