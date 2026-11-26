//! Chat composer bar: attachments (drag-and-drop + picker), unified Chat Controls, and Send/Stop.

mod actions;
mod attachments;
mod input_helpers;
mod state_sync;
mod toolbar;
mod view_model;

use std::sync::Arc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;
use crate::core::types::SearchEngine;
use crate::i18n::{gettext, tr_f};
use crate::services::AppServices;
use crate::storage::{ConversationFolder, ModelRecord};
use crate::ui::mention_picker::{MentionEntry, MentionPicker};

pub use actions::{ComposerMsg, ComposerOutput};
use attachments::{
    AttachmentQueue, classify_dropped_paths, enqueue_classified_files, open_attach_file_dialog,
    rebuild_chips_box,
};
use input_helpers::attach_key_controller;
pub use input_helpers::cleanup_orphaned_clipboard_files;
use state_sync::build_composer_widgets;

pub struct ComposerModel {
    services: Arc<AppServices>,
    text_view: gtk::TextView,
    chips_box: gtk::Box,
    attach_btn: gtk::Button,
    folders_btn: gtk::Button,
    globe_btn: gtk::Button,
    controls_btn: gtk::Button,
    web_search_enabled: bool,
    web_search_global_enabled: bool,
    search_engine: SearchEngine,
    models: Vec<ModelRecord>,
    active_model_id: Option<String>,
    assistant_name: String,
    system_prompt: String,
    mention_picker: MentionPicker,
    thinking_level: ThinkingLevel,
    context_used_tokens: u32,
    attachments: AttachmentQueue,
    granted_folders: Vec<ConversationFolder>,
    granted_files: Vec<MentionEntry>,
    files_seq: u64,
    is_generating: bool,
}

#[relm4::component(pub)]
impl SimpleComponent for ComposerModel {
    type Init = (Arc<AppServices>, Option<String>);
    type Input = ComposerMsg;
    type Output = ComposerOutput;

    view! {
        adw::Clamp {
            set_maximum_size: 780,
            set_tightening_threshold: 560,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_margin_top: 6,
                set_margin_bottom: 14,
                set_margin_start: 16,
                set_margin_end: 16,

                #[name = "composer_card"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 4,
                    add_css_class: "composer-card",

                    #[local_ref]
                    chips_box -> gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 8,
                        set_halign: gtk::Align::Start,
                        set_vexpand: false,
                        set_margin_top: 10,
                        set_margin_start: 12,
                        set_margin_end: 12,
                        set_visible: false,
                    },

                    gtk::Overlay {
                        #[wrap(Some)]
                        set_child = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            set_vscrollbar_policy: gtk::PolicyType::Automatic,
                            set_propagate_natural_height: true,
                            set_min_content_height: 44,
                            set_max_content_height: 320,

                            #[local_ref]
                            text_view -> gtk::TextView {
                                add_css_class: "composer-input",
                                set_wrap_mode: gtk::WrapMode::WordChar,
                                set_top_margin: 16,
                                set_bottom_margin: 4,
                                set_left_margin: 16,
                                set_right_margin: 16,
                                set_accepts_tab: false,
                            }
                        },

                        #[name = "placeholder"]
                        add_overlay = &gtk::Label {
                            #[watch]
                            set_label: &tr_f("Message {name}…", &[("name", &model.assistant_name)]),
                            set_halign: gtk::Align::Start,
                            set_valign: gtk::Align::Start,
                            set_margin_top: 16,
                            set_margin_start: 16,
                            set_can_target: false,
                            add_css_class: "dim-label",
                        }
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 4,
                        set_margin_bottom: 8,
                        set_margin_start: 10,
                        set_margin_end: 10,

                        #[local_ref]
                        attach_btn -> gtk::Button {
                            set_icon_name: "mail-attachment-symbolic",
                            add_css_class: "flat",
                            add_css_class: "circular",
                            connect_clicked => ComposerMsg::AttachPressed,
                        },

                        #[local_ref]
                        folders_btn -> gtk::Button {
                            connect_clicked => ComposerMsg::OpenFolderPermissions,
                        },

                        #[local_ref]
                        globe_btn -> gtk::Button {
                            connect_clicked => ComposerMsg::ToggleWebSearch,
                        },

                        #[local_ref]
                        controls_btn -> gtk::Button {
                            add_css_class: "flat",
                            connect_clicked => ComposerMsg::OpenChatControls,
                        },

                        gtk::Box {
                            set_hexpand: true,
                        },

                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.is_generating {
                                "media-playback-stop-symbolic"
                            } else {
                                "go-up-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(&if model.is_generating {
                                gettext("Stop generation")
                            } else {
                                gettext("Send message (Enter)")
                            }),
                            #[watch]
                            set_css_classes: if model.is_generating {
                                &["circular", "destructive-action"]
                            } else {
                                &["circular", "suggested-action"]
                            },
                            connect_clicked[sender] => move |_| {
                                sender.input(ComposerMsg::SendPressed);
                            },
                        }
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
        let (text_view, chips_box, attach_btn, folders_btn, globe_btn, controls_btn) =
            build_composer_widgets();
        let mention_picker = MentionPicker::attach(&text_view);
        attach_key_controller(&text_view, &mention_picker, &sender);

        let clipboard_dir = services.paths.clipboard_dir.clone();
        let rt_handle = services.rt_handle.clone();
        let model = Self {
            services,
            text_view: text_view.clone(),
            chips_box: chips_box.clone(),
            attach_btn: attach_btn.clone(),
            folders_btn: folders_btn.clone(),
            globe_btn: globe_btn.clone(),
            controls_btn: controls_btn.clone(),
            web_search_enabled: false,
            web_search_global_enabled: false,
            search_engine: SearchEngine::default(),
            models: Vec::new(),
            active_model_id,
            assistant_name: "Companion".to_string(),
            system_prompt: default_system_prompt().to_string(),
            mention_picker: mention_picker.clone(),
            thinking_level: ThinkingLevel::Off,
            context_used_tokens: 0,
            attachments: AttachmentQueue::default(),
            granted_folders: Vec::new(),
            granted_files: Vec::new(),
            files_seq: 0,
            is_generating: false,
        };
        model.refresh_all_toolbar_buttons(&sender);

        let widgets = view_output!();
        state_sync::wire_composer_text_view(
            &text_view,
            &widgets.placeholder,
            mention_picker,
            clipboard_dir,
            rt_handle,
            &sender,
        );
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ComposerMsg::SendPressed
            | ComposerMsg::StopPressed
            | ComposerMsg::AttachPressed
            | ComposerMsg::FilesDropped(_)
            | ComposerMsg::PathsClassified { .. }
            | ComposerMsg::AttachmentLoaded { .. }
            | ComposerMsg::RemoveAttachment(_)
            | ComposerMsg::OpenFolderPermissions
            | ComposerMsg::SetGrantedFolders(_)
            | ComposerMsg::GrantedFilesLoaded { .. } => {
                self.handle_input_or_attachment_msg(msg, &sender)
            }
            _ => self.handle_settings_or_state_msg(msg, &sender),
        }
    }
}

impl ComposerModel {
    fn handle_input_or_attachment_msg(&mut self, msg: ComposerMsg, sender: &ComponentSender<Self>) {
        match msg {
            ComposerMsg::SendPressed => self.handle_send_pressed(sender),
            ComposerMsg::StopPressed => drop(sender.output(ComposerOutput::StopRequested)),
            ComposerMsg::AttachPressed => open_attach_file_dialog(&self.text_view, sender),
            ComposerMsg::FilesDropped(paths) => {
                classify_dropped_paths(paths, &self.services.rt_handle, sender);
            }
            ComposerMsg::PathsClassified { dirs, files } => {
                for dir in dirs {
                    let _ = sender.output(ComposerOutput::GrantFolder(dir));
                }
                let has_mmproj = self.has_active_model_mmproj();
                enqueue_classified_files(
                    &mut self.attachments,
                    files,
                    has_mmproj,
                    &self.chips_box,
                    &self.services.rt_handle,
                    sender,
                );
            }
            ComposerMsg::AttachmentLoaded { temp_id, result } => {
                self.handle_attachment_loaded(temp_id, result, sender);
            }
            ComposerMsg::RemoveAttachment(temp_id) => {
                self.attachments.remove(&temp_id);
                rebuild_chips_box(&self.chips_box, &self.attachments, sender);
            }
            ComposerMsg::OpenFolderPermissions => {
                let _ = sender.output(ComposerOutput::OpenFolderPermissions);
            }
            ComposerMsg::SetGrantedFolders(folders) => {
                self.granted_folders = folders;
                self.refresh_folders_button();
                self.refresh_granted_files(sender);
            }
            ComposerMsg::GrantedFilesLoaded { seq, files } if seq == self.files_seq => {
                self.granted_files = files;
                self.sync_mention_files();
            }
            _ => {}
        }
    }

    fn handle_settings_or_state_msg(&mut self, msg: ComposerMsg, sender: &ComponentSender<Self>) {
        match msg {
            ComposerMsg::ToggleWebSearch if self.web_search_global_enabled => {
                self.web_search_enabled = !self.web_search_enabled;
                self.refresh_globe_button();
                let _ = sender.output(ComposerOutput::WebSearchToggled(self.web_search_enabled));
            }
            ComposerMsg::SetWebSearchEnabled(enabled) => {
                self.web_search_enabled = enabled;
                self.refresh_globe_button();
            }
            ComposerMsg::SetWebSearchGlobalEnabled(enabled) => {
                self.web_search_global_enabled = enabled;
                self.refresh_search_engine(sender);
                self.refresh_globe_button();
            }
            ComposerMsg::OpenChatControls => self.open_chat_controls(sender),
            ComposerMsg::ControlsApplied {
                thinking_level,
                system_prompt,
                context_length,
            } => {
                self.handle_controls_applied(thinking_level, system_prompt, context_length, sender);
            }
            ComposerMsg::OpenModelSettings => {
                drop(sender.output(ComposerOutput::OpenModelSettings));
            }
            ComposerMsg::InitDataLoaded { .. }
            | ComposerMsg::ModelsMetadataLoaded(_)
            | ComposerMsg::SearchEngineLoaded(_)
            | ComposerMsg::RefreshModels(_)
            | ComposerMsg::SetSystemPrompt(_)
            | ComposerMsg::SetThinkingLevel(_)
            | ComposerMsg::SetAssistantName(_)
            | ComposerMsg::SetContextUsed(_)
            | ComposerMsg::SetGenerating(_) => self.handle_state_sync_msg(msg, sender),
            _ => {}
        }
    }
}
