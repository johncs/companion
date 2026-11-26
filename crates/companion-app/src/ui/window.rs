//! Main adaptive GNOME application window (`AdwNavigationSplitView`).

use adw::prelude::*;
use relm4::prelude::*;

use crate::core::paths::AppPaths;
use crate::i18n::gettext;
use crate::models::download_manager::DownloadManager;
use crate::models::hardware::{HardwareInfo, probe_hardware};
use crate::storage::{Conversation, Database, ModelRecord};
use crate::ui::chat_view::{ChatViewModel, ChatViewMsg, ChatViewOutput};
use crate::ui::preferences::{SettingsPage, present_settings_dialog};
use crate::ui::sidebar::{SidebarModel, SidebarMsg, SidebarOutput};

pub struct MainWindow {
    db: Database,
    paths: AppPaths,
    download_manager: DownloadManager,
    sidebar: Controller<SidebarModel>,
    chat_view: Controller<ChatViewModel>,
    active_conversation: Option<Conversation>,
    conv_select_seq: u64,
    active_model: Option<ModelRecord>,
    hardware_info: Option<HardwareInfo>,
    toast_overlay: adw::ToastOverlay,
    split_view: adw::NavigationSplitView,
}

#[derive(Debug)]
pub enum MainWindowMsg {
    ConversationSelected(String),
    ConversationLoaded {
        request_seq: u64,
        conv: Option<Conversation>,
    },
    ConversationRenamed {
        id: String,
        title: String,
    },
    ConversationDeleted(String),
    ChatRunningChanged {
        id: String,
        is_running: bool,
    },
    ActiveModelChanged(Option<String>),
    PersonalizationChanged {
        assistant_name: String,
        user_name: Option<String>,
    },
    WebSearchGlobalChanged(bool),
    HardwareProbed(HardwareInfo),
    OpenModelManager,
    OpenPreferences,
    ShowToast(String),
    NewChat,
}

#[relm4::component(pub)]
impl SimpleComponent for MainWindow {
    type Init = (Database, AppPaths);
    type Input = MainWindowMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("Companion"),
            set_default_width: 1120,
            set_default_height: 740,
            set_width_request: 360,
            set_height_request: 480,

            add_breakpoint = adw::Breakpoint::new(
                adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    720.0,
                    adw::LengthUnit::Sp,
                )
            ) {
                add_setter: (&split_view, "collapsed", Some(&true.to_value())),
            },

            #[local_ref]
            toast_overlay -> adw::ToastOverlay {
                #[local_ref]
                split_view -> adw::NavigationSplitView {
                    set_min_sidebar_width: 310.0,
                    set_max_sidebar_width: 310.0,
                    set_sidebar_width_fraction: 0.35,

                    #[wrap(Some)]
                    set_sidebar = &adw::NavigationPage {
                        set_title: "Companion",
                        #[wrap(Some)]
                        set_child = model.sidebar.widget(),
                    },

                    #[wrap(Some)]
                    set_content = &adw::NavigationPage {
                        set_title: &gettext("Chat"),

                        #[wrap(Some)]
                        set_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                #[wrap(Some)]
                                set_title_widget = &adw::WindowTitle {
                                    #[watch]
                                    set_title: model
                                        .active_conversation
                                        .as_ref()
                                        .map(|c| c.title.as_str())
                                        .unwrap_or(""),
                                },
                            },

                            #[wrap(Some)]
                            set_content = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,

                                adw::StatusPage {
                                    #[watch]
                                    set_visible: model.active_conversation.is_none(),
                                    set_vexpand: true,
                                    set_icon_name: Some(crate::ui::icons::MODEL),
                                    set_title: &gettext("Welcome to Companion"),
                                    set_description: Some(&gettext("Start a new chat to begin.")),

                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Horizontal,
                                        set_halign: gtk::Align::Center,

                                        gtk::Button {
                                            set_label: &gettext("New Chat"),
                                            add_css_class: "pill",
                                            add_css_class: "suggested-action",
                                            connect_clicked => MainWindowMsg::NewChat,
                                        },
                                    }
                                },

                                append = model.chat_view.widget(),
                            }
                        }
                    }
                }
            }
        }
    }

    fn init(
        (db, paths): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        crate::ui::load_css();

        let sidebar =
            SidebarModel::builder()
                .launch(db.clone())
                .forward(sender.input_sender(), |output| match output {
                    SidebarOutput::ConversationSelected(id) => {
                        MainWindowMsg::ConversationSelected(id)
                    }
                    SidebarOutput::ConversationRenamed { id, title } => {
                        MainWindowMsg::ConversationRenamed { id, title }
                    }
                    SidebarOutput::ConversationDeleted(id) => {
                        MainWindowMsg::ConversationDeleted(id)
                    }
                    SidebarOutput::OpenSettings => MainWindowMsg::OpenPreferences,
                    SidebarOutput::ShowToast(text) => MainWindowMsg::ShowToast(text),
                });

        let saved_model = db
            .get_active_model_id()
            .ok()
            .flatten()
            .and_then(|id| db.get_model(&id).ok().flatten());
        let active_model = saved_model.or_else(|| {
            db.list_models().ok().and_then(|mut m| {
                if m.is_empty() {
                    None
                } else {
                    Some(m.remove(0))
                }
            })
        });
        let initial_model_id = active_model.as_ref().map(|m| m.id.clone());

        // Warm GGUF metadata cache and syntect sets concurrently across CPU cores at startup.
        if let Ok(all_models) = db.list_models() {
            let paths: Vec<std::path::PathBuf> = all_models
                .into_iter()
                .filter_map(|m| m.file_path.map(std::path::PathBuf::from))
                .collect();
            crate::models::gguf::GgufMetadataCache::new()
                .warm(paths, crate::core::runtime().handle());
        }
        crate::core::runtime().spawn_blocking(crate::core::markdown::warm_syntax_highlighting);
        {
            let input_sender = sender.input_sender().clone();
            crate::core::runtime().spawn_blocking(move || {
                let _ = input_sender.send(MainWindowMsg::HardwareProbed(probe_hardware()));
            });
        }

        let chat_view = ChatViewModel::builder()
            .launch((db.clone(), initial_model_id))
            .forward(sender.input_sender(), |output| match output {
                ChatViewOutput::ShowToast(text) => MainWindowMsg::ShowToast(text),
                ChatViewOutput::ConversationTitleUpdated { id, title } => {
                    MainWindowMsg::ConversationRenamed { id, title }
                }
                ChatViewOutput::ChatRunningChanged {
                    conversation_id,
                    is_running,
                } => MainWindowMsg::ChatRunningChanged {
                    id: conversation_id,
                    is_running,
                },
                ChatViewOutput::OpenModelSettings => MainWindowMsg::OpenModelManager,
            });

        let toast_overlay = adw::ToastOverlay::new();
        let split_view = adw::NavigationSplitView::new();

        let download_manager = DownloadManager::new(
            db.clone(),
            paths.models_dir.clone(),
            crate::core::runtime().handle().clone(),
        );
        let model = Self {
            db,
            paths,
            download_manager,
            sidebar,
            chat_view,
            active_conversation: None,
            conv_select_seq: 0,
            active_model,
            hardware_info: None,
            toast_overlay: toast_overlay.clone(),
            split_view: split_view.clone(),
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            MainWindowMsg::ConversationSelected(conv_id) => {
                self.conv_select_seq = self.conv_select_seq.wrapping_add(1);
                let req_seq = self.conv_select_seq;
                let db = self.db.clone();
                let input_sender = sender.input_sender().clone();
                crate::core::runtime().spawn_blocking(move || {
                    let conv = db.get_conversation(&conv_id).ok().flatten();
                    let _ = input_sender.send(MainWindowMsg::ConversationLoaded {
                        request_seq: req_seq,
                        conv,
                    });
                });
            }
            MainWindowMsg::ConversationLoaded { request_seq, conv } => {
                if self.conv_select_seq != request_seq {
                    return;
                }
                self.active_conversation = conv;
                if let Some(conv) = &self.active_conversation {
                    self.chat_view.emit(ChatViewMsg::LoadConversation {
                        conversation_id: conv.id.clone(),
                        title: conv.title.clone(),
                    });
                } else {
                    self.chat_view.emit(ChatViewMsg::ClearConversation);
                }
                self.split_view.set_show_content(true);
            }
            MainWindowMsg::ConversationRenamed { id, title } => {
                if let Some(conv) = &mut self.active_conversation
                    && conv.id == id
                {
                    conv.title = title;
                }
                self.sidebar.emit(SidebarMsg::RefreshLists);
            }
            MainWindowMsg::ConversationDeleted(id) => {
                self.chat_view
                    .emit(ChatViewMsg::CancelConversation(id.clone()));
                if self.active_conversation.as_ref().map(|c| c.id.as_str()) == Some(&id) {
                    self.active_conversation = None;
                    self.chat_view.emit(ChatViewMsg::ClearConversation);
                }
            }
            MainWindowMsg::ActiveModelChanged(model_id) => {
                let _ = self.db.set_active_model_id(model_id.as_deref());
                self.active_model = model_id
                    .as_deref()
                    .and_then(|id| self.db.get_model(id).ok().flatten());
                self.chat_view.emit(ChatViewMsg::RefreshModels(model_id));
            }
            MainWindowMsg::PersonalizationChanged {
                assistant_name,
                user_name,
            } => {
                self.chat_view.emit(ChatViewMsg::PersonalizationChanged {
                    assistant_name,
                    user_name,
                });
            }
            MainWindowMsg::WebSearchGlobalChanged(enabled) => {
                self.chat_view
                    .emit(ChatViewMsg::RefreshWebSearchGlobal(enabled));
            }
            MainWindowMsg::HardwareProbed(hw) => {
                self.hardware_info = Some(hw);
            }
            MainWindowMsg::ChatRunningChanged { id, is_running } => {
                self.sidebar
                    .emit(SidebarMsg::SetConversationRunning { id, is_running });
            }
            MainWindowMsg::OpenModelManager => {
                self.open_settings(SettingsPage::Models, &sender);
            }
            MainWindowMsg::OpenPreferences => {
                self.open_settings(SettingsPage::General, &sender);
            }
            MainWindowMsg::ShowToast(text) => {
                self.toast_overlay.add_toast(adw::Toast::new(&text));
            }
            MainWindowMsg::NewChat => {
                self.sidebar.emit(SidebarMsg::NewChat);
            }
        }
    }
}

impl MainWindow {
    fn open_settings(&self, initial_page: SettingsPage, sender: &ComponentSender<Self>) {
        let launcher = SettingsLauncher {
            toast_overlay: self.toast_overlay.clone(),
            db: self.db.clone(),
            models_dir: self.paths.models_dir.clone(),
            download_manager: self.download_manager.clone(),
            active_model_id: self.active_model.as_ref().map(|m| m.id.clone()),
            sender: sender.clone(),
        };
        if let Some(hw) = self.hardware_info.clone() {
            launcher.present(initial_page, hw);
            return;
        }
        crate::ui::widgets::spawn_blocking_ui(probe_hardware, move |hw| {
            launcher
                .sender
                .input(MainWindowMsg::HardwareProbed(hw.clone()));
            launcher.present(initial_page, hw);
        });
    }
}

struct SettingsLauncher {
    toast_overlay: adw::ToastOverlay,
    db: Database,
    models_dir: std::path::PathBuf,
    download_manager: DownloadManager,
    active_model_id: Option<String>,
    sender: ComponentSender<MainWindow>,
}

impl SettingsLauncher {
    fn present(&self, initial_page: SettingsPage, hw: HardwareInfo) {
        let sender_model = self.sender.clone();
        let sender_name = self.sender.clone();
        let sender_web = self.sender.clone();
        present_settings_dialog(
            &self.toast_overlay,
            crate::ui::preferences::SettingsDialogConfig {
                db: self.db.clone(),
                models_dir: self.models_dir.clone(),
                download_manager: self.download_manager.clone(),
                hardware_info: hw,
                active_model_id: self.active_model_id.clone(),
                initial_page,
                on_active_changed: move |new_id| {
                    sender_model.input(MainWindowMsg::ActiveModelChanged(new_id));
                },
                on_personalization_changed: move |new_name, new_user| {
                    sender_name.input(MainWindowMsg::PersonalizationChanged {
                        assistant_name: new_name,
                        user_name: new_user,
                    });
                },
                on_web_search_global_changed: move |enabled| {
                    sender_web.input(MainWindowMsg::WebSearchGlobalChanged(enabled));
                },
            },
        );
    }
}
