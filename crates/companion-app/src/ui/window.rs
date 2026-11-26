//! Main adaptive GNOME application window (`AdwNavigationSplitView`).

use std::sync::Arc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::gettext;
use crate::models::hardware::{HardwareInfo, probe_hardware};
use crate::services::AppServices;
use crate::storage::Conversation;
use crate::storage::ModelRecord;
use crate::ui::chat_view::{ChatViewModel, ChatViewMsg, ChatViewOutput};
use crate::ui::preferences::SettingsPage;
use crate::ui::sidebar::{SidebarModel, SidebarMsg, SidebarOutput};

pub struct MainWindow {
    services: Arc<AppServices>,
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
    ModelContextLengthChanged(u32),
    PersonalizationChanged {
        assistant_name: String,
        user_name: Option<String>,
    },
    WebSearchGlobalChanged(bool),
    StartupDataLoaded {
        active_model: Option<ModelRecord>,
        color_scheme: Option<String>,
    },
    HardwareProbed(HardwareInfo),
    OpenModelManager,
    OpenPreferences,
    ShowToast(String),
    NewChat,
}

#[relm4::component(pub)]
impl SimpleComponent for MainWindow {
    type Init = Arc<AppServices>;
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
                                    set_title: model.active_conversation.as_ref().map(|c| c.title.as_str()).unwrap_or(""),
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
        services: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        crate::ui::load_css();
        let sidebar = launch_sidebar(&services, &sender);
        spawn_startup_sync(&services, &sender);
        let chat_view = launch_chat_view(&services, &sender);

        let toast_overlay = adw::ToastOverlay::new();
        let split_view = adw::NavigationSplitView::new();
        let model = Self {
            services,
            sidebar,
            chat_view,
            active_conversation: None,
            conv_select_seq: 0,
            active_model: None,
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
                self.on_conversation_selected(conv_id, &sender)
            }
            MainWindowMsg::ConversationLoaded { request_seq, conv } => {
                self.on_conversation_loaded(request_seq, conv)
            }
            MainWindowMsg::ConversationRenamed { id, title } => {
                self.on_conversation_renamed(&id, title)
            }
            MainWindowMsg::ConversationDeleted(id) => self.on_conversation_deleted(&id),
            MainWindowMsg::ActiveModelChanged(model_id) => self.on_active_model_changed(model_id),
            MainWindowMsg::ModelContextLengthChanged(n_ctx) => {
                self.on_model_ctx_changed(n_ctx, &sender)
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
            MainWindowMsg::StartupDataLoaded {
                active_model,
                color_scheme,
            } => {
                crate::ui::preferences::apply_color_scheme_setting(color_scheme.as_deref());
                let model_id = active_model.as_ref().map(|m| m.id.clone());
                self.active_model = active_model;
                self.chat_view.emit(ChatViewMsg::RefreshModels(model_id));
            }
            MainWindowMsg::HardwareProbed(hw) => self.hardware_info = Some(hw),
            MainWindowMsg::ChatRunningChanged { id, is_running } => {
                self.sidebar
                    .emit(SidebarMsg::SetConversationRunning { id, is_running });
            }
            MainWindowMsg::OpenModelManager => self.open_settings(SettingsPage::Models, &sender),
            MainWindowMsg::OpenPreferences => self.open_settings(SettingsPage::General, &sender),
            MainWindowMsg::ShowToast(text) => self.toast_overlay.add_toast(adw::Toast::new(&text)),
            MainWindowMsg::NewChat => self.sidebar.emit(SidebarMsg::NewChat),
        }
    }
}

impl MainWindow {
    fn on_conversation_selected(&mut self, conv_id: String, sender: &ComponentSender<Self>) {
        self.conv_select_seq = self.conv_select_seq.wrapping_add(1);
        let (req_seq, db, input_sender) = (
            self.conv_select_seq,
            self.services.db.clone(),
            sender.input_sender().clone(),
        );
        self.services.rt_handle.spawn_blocking(move || {
            let conv = db.get_conversation(&conv_id).ok().flatten();
            let _ = input_sender.send(MainWindowMsg::ConversationLoaded {
                request_seq: req_seq,
                conv,
            });
        });
    }

    fn on_conversation_loaded(&mut self, request_seq: u64, conv: Option<Conversation>) {
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

    fn on_conversation_renamed(&mut self, id: &str, title: String) {
        if let Some(conv) = &mut self.active_conversation
            && conv.id == id
        {
            conv.title = title;
        }
        self.sidebar.emit(SidebarMsg::RefreshLists);
    }

    fn on_conversation_deleted(&mut self, id: &str) {
        self.chat_view
            .emit(ChatViewMsg::CancelConversation(id.to_string()));
        if self.active_conversation.as_ref().map(|c| c.id.as_str()) == Some(id) {
            self.active_conversation = None;
            self.chat_view.emit(ChatViewMsg::ClearConversation);
        }
    }

    fn on_active_model_changed(&mut self, model_id: Option<String>) {
        if let Err(err) = self.services.db.set_active_model_id(model_id.as_deref()) {
            tracing::error!("Failed to save active model: {err}");
            self.toast_overlay
                .add_toast(adw::Toast::new(&crate::i18n::tr_f(
                    "Failed to save active model: {err}",
                    &[("err", &err.to_string())],
                )));
        }
        self.active_model = model_id
            .as_deref()
            .and_then(|id| self.services.db.get_model(id).ok().flatten());
        self.chat_view.emit(ChatViewMsg::RefreshModels(model_id));
    }

    fn on_model_ctx_changed(&mut self, n_ctx: u32, sender: &ComponentSender<Self>) {
        let Some(ref mut model) = self.active_model else {
            return;
        };
        model.default_params.n_ctx = n_ctx;
        let (rec, db, input_sender) = (
            model.clone(),
            self.services.db.clone(),
            sender.input_sender().clone(),
        );
        self.services.rt_handle.spawn_blocking(move || {
            if let Err(err) = db.upsert_model(&rec) {
                tracing::error!("Failed to persist model context length: {err}");
                let _ = input_sender.send(MainWindowMsg::ShowToast(crate::i18n::tr_f(
                    "Failed to save context length: {err}",
                    &[("err", &err.to_string())],
                )));
            }
        });
    }

    fn open_settings(&self, page: SettingsPage, sender: &ComponentSender<Self>) {
        super::window_settings::open_settings_dialog(
            &self.toast_overlay,
            &self.services,
            self.hardware_info.as_ref(),
            self.active_model.as_ref().map(|m| m.id.clone()),
            page,
            sender,
        );
    }
}

fn launch_sidebar(
    services: &Arc<AppServices>,
    sender: &ComponentSender<MainWindow>,
) -> Controller<SidebarModel> {
    SidebarModel::builder()
        .launch(services.clone())
        .forward(sender.input_sender(), |output| match output {
            SidebarOutput::ConversationSelected(id) => MainWindowMsg::ConversationSelected(id),
            SidebarOutput::ConversationRenamed { id, title } => {
                MainWindowMsg::ConversationRenamed { id, title }
            }
            SidebarOutput::ConversationDeleted(id) => MainWindowMsg::ConversationDeleted(id),
            SidebarOutput::OpenSettings => MainWindowMsg::OpenPreferences,
            SidebarOutput::ShowToast(text) => MainWindowMsg::ShowToast(text),
        })
}

fn spawn_startup_sync(services: &Arc<AppServices>, sender: &ComponentSender<MainWindow>) {
    let real_name = gtk::glib::real_name().to_string_lossy().trim().to_string();
    let (services_bg, startup_sender) = (services.clone(), sender.input_sender().clone());
    services.rt_handle.spawn_blocking(move || {
        let (active_model, color_scheme) = services_bg.run_startup_sync(&real_name);
        let _ = startup_sender.send(MainWindowMsg::StartupDataLoaded {
            active_model,
            color_scheme,
        });
        let _ = startup_sender.send(MainWindowMsg::HardwareProbed(probe_hardware()));
    });
}

fn launch_chat_view(
    services: &Arc<AppServices>,
    sender: &ComponentSender<MainWindow>,
) -> Controller<ChatViewModel> {
    ChatViewModel::builder()
        .launch((services.clone(), None))
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
            ChatViewOutput::ModelContextLengthChanged(n_ctx) => {
                MainWindowMsg::ModelContextLengthChanged(n_ctx)
            }
            ChatViewOutput::OpenModelSettings => MainWindowMsg::OpenModelManager,
        })
}
