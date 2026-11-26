//! Chat composer bar: attachments (drag-and-drop + picker), unified Chat Controls, and Send/Stop.

mod attachments;
mod input_helpers;
mod toolbar;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, glib};
use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;
use crate::i18n::{gettext, tr_f};
use crate::storage::{AttachmentRecord, ConversationFolder, Database, ModelRecord};
use crate::ui::mention_picker::MentionPicker;

use attachments::{
    AttachmentQueue, enqueue_dropped_paths, open_attach_file_dialog, rebuild_chips_box,
};
use input_helpers::{attach_clipboard_paste_handler, attach_dynamic_height_scaler};

#[derive(Debug)]
pub enum ComposerMsg {
    SendPressed,
    StopPressed,
    AttachPressed,
    FilesDropped(Vec<PathBuf>),
    AttachmentLoaded {
        temp_id: String,
        result: Result<AttachmentRecord, String>,
    },
    RemoveAttachment(String),
    OpenFolderPermissions,
    ToggleWebSearch,
    SetWebSearchEnabled(bool),
    SetWebSearchGlobalEnabled(bool),
    SetGrantedFolders(Vec<ConversationFolder>),
    GrantedFilesLoaded(Vec<String>),
    OpenChatControls,
    ControlsApplied {
        thinking_level: ThinkingLevel,
        system_prompt: String,
        context_length: u32,
    },
    OpenModelSettings,
    RefreshModels(Option<String>),
    SetSystemPrompt(String),
    SetThinkingLevel(ThinkingLevel),
    SetAssistantName(String),
    SetContextUsed(u32),
    SetGenerating(bool),
}

#[derive(Debug)]
pub enum ComposerOutput {
    Send {
        text: String,
        thinking_level: ThinkingLevel,
        attachments: Vec<AttachmentRecord>,
    },
    StopRequested,
    OpenFolderPermissions,
    GrantFolder(PathBuf),
    WebSearchToggled(bool),
    SystemPromptChanged(String),
    ThinkingLevelChanged(ThinkingLevel),
    OpenModelSettings,
    ShowToast(String),
}

pub struct ComposerModel {
    db: Database,
    text_view: gtk::TextView,
    chips_box: gtk::Box,
    attach_btn: gtk::Button,
    folders_btn: gtk::Button,
    globe_btn: gtk::Button,
    controls_btn: gtk::Button,
    web_search_enabled: bool,
    web_search_global_enabled: bool,
    models: Vec<ModelRecord>,
    active_model_id: Option<String>,
    assistant_name: String,
    system_prompt: String,
    mention_picker: MentionPicker,
    thinking_level: ThinkingLevel,
    context_used_tokens: u32,
    attachments: AttachmentQueue,
    granted_folders: Vec<ConversationFolder>,
    granted_files: Vec<String>,
    is_generating: bool,
}

#[relm4::component(pub)]
impl SimpleComponent for ComposerModel {
    type Init = (Database, Option<String>);
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
        (db, active_model_id): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let text_view = gtk::TextView::new();
        let chips_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let attach_btn = gtk::Button::new();
        let folders_btn = gtk::Button::new();
        let globe_btn = gtk::Button::new();
        let controls_btn = gtk::Button::new();
        let mention_picker = MentionPicker::attach(&text_view);

        let key_ctrl = gtk::EventControllerKey::new();
        let sender_clone = sender.clone();
        let picker_key = mention_picker.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, mods| {
            if let Some(prop) = picker_key.handle_key(key) {
                return prop;
            }
            if (key == gdk::Key::Return || key == gdk::Key::KP_Enter)
                && !mods.contains(gdk::ModifierType::SHIFT_MASK)
            {
                sender_clone.input(ComposerMsg::SendPressed);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        text_view.add_controller(key_ctrl);

        let assistant_name = db
            .get_assistant_name()
            .unwrap_or_else(|_| "Companion".to_string());
        let web_search_global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
        let mut model = Self {
            db,
            text_view: text_view.clone(),
            chips_box: chips_box.clone(),
            attach_btn: attach_btn.clone(),
            folders_btn: folders_btn.clone(),
            globe_btn: globe_btn.clone(),
            controls_btn: controls_btn.clone(),
            web_search_enabled: false,
            web_search_global_enabled,
            models: Vec::new(),
            active_model_id,
            assistant_name,
            system_prompt: default_system_prompt().to_string(),
            mention_picker: mention_picker.clone(),
            thinking_level: ThinkingLevel::Off,
            context_used_tokens: 0,
            attachments: AttachmentQueue::default(),
            granted_folders: Vec::new(),
            granted_files: Vec::new(),
            is_generating: false,
        };
        model.refresh_folders_button();
        model.refresh_globe_button();
        model.refresh_controls_button();
        model.refresh_attach_button();

        let widgets = view_output!();

        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let sender_drop = sender.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                let paths: Vec<PathBuf> = file_list
                    .files()
                    .into_iter()
                    .filter_map(|f| f.path())
                    .collect();
                if !paths.is_empty() {
                    sender_drop.input(ComposerMsg::FilesDropped(paths));
                    return true;
                }
            }
            false
        });
        widgets.composer_card.add_controller(drop_target);

        let placeholder = widgets.placeholder.clone();
        let picker_buf = mention_picker;
        text_view.buffer().connect_changed(move |buf| {
            placeholder.set_visible(buf.char_count() == 0);
            picker_buf.update_from_buffer();
        });

        attach_clipboard_paste_handler(&text_view, &sender);
        attach_dynamic_height_scaler(&text_view);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ComposerMsg::SendPressed => {
                if self.is_generating {
                    sender.input(ComposerMsg::StopPressed);
                    return;
                }
                if self.attachments.has_loading() {
                    let _ = sender.output(ComposerOutput::ShowToast(gettext(
                        "Please wait for attached files to finish loading",
                    )));
                    return;
                }

                let buffer = self.text_view.buffer();
                let (start, end) = buffer.bounds();
                let text = buffer.text(&start, &end, false).trim().to_string();
                let ready_attachments = self.attachments.take_ready();

                if text.is_empty() && ready_attachments.is_empty() {
                    return;
                }

                buffer.set_text("");
                rebuild_chips_box(&self.chips_box, &self.attachments, &sender);

                let _ = sender.output(ComposerOutput::Send {
                    text,
                    thinking_level: self.thinking_level,
                    attachments: ready_attachments,
                });
            }
            ComposerMsg::StopPressed => {
                let _ = sender.output(ComposerOutput::StopRequested);
            }
            ComposerMsg::AttachPressed => {
                open_attach_file_dialog(&self.text_view, &sender);
            }
            ComposerMsg::FilesDropped(paths) => {
                let has_mmproj = self.has_active_model_mmproj();
                enqueue_dropped_paths(
                    &mut self.attachments,
                    paths,
                    has_mmproj,
                    &self.chips_box,
                    &sender,
                );
            }
            ComposerMsg::AttachmentLoaded { temp_id, result } => match result {
                Ok(record) => {
                    self.attachments.mark_loaded(&temp_id, record);
                    rebuild_chips_box(&self.chips_box, &self.attachments, &sender);
                }
                Err(err) => {
                    self.attachments.remove(&temp_id);
                    rebuild_chips_box(&self.chips_box, &self.attachments, &sender);
                    let _ = sender.output(ComposerOutput::ShowToast(err));
                }
            },
            ComposerMsg::RemoveAttachment(temp_id) => {
                self.attachments.remove(&temp_id);
                rebuild_chips_box(&self.chips_box, &self.attachments, &sender);
            }
            ComposerMsg::OpenFolderPermissions => {
                let _ = sender.output(ComposerOutput::OpenFolderPermissions);
            }
            ComposerMsg::ToggleWebSearch => {
                if !self.web_search_global_enabled {
                    return;
                }
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
                self.refresh_globe_button();
            }
            ComposerMsg::SetGrantedFolders(folders) => {
                self.granted_folders = folders;
                self.refresh_folders_button();
                self.refresh_granted_files(&sender);
            }
            ComposerMsg::GrantedFilesLoaded(files) => {
                self.granted_files = files;
                self.sync_mention_files();
            }
            ComposerMsg::OpenChatControls => self.open_chat_controls(&sender),
            ComposerMsg::ControlsApplied {
                thinking_level,
                system_prompt,
                context_length,
            } => {
                let prompt_changed = self.system_prompt != system_prompt;
                let thinking_changed = self.thinking_level != thinking_level;
                self.thinking_level = thinking_level;
                self.system_prompt = system_prompt.clone();

                if let Some(mid) = &self.active_model_id
                    && let Ok(Some(mut rec)) = self.db.get_model(mid)
                {
                    rec.default_params.n_ctx = context_length;
                    let _ = self.db.upsert_model(&rec);
                }

                self.refresh_controls_button();

                if prompt_changed {
                    let _ = sender.output(ComposerOutput::SystemPromptChanged(system_prompt));
                }
                if thinking_changed {
                    let _ = sender.output(ComposerOutput::ThinkingLevelChanged(thinking_level));
                }
            }
            ComposerMsg::OpenModelSettings => {
                let _ = sender.output(ComposerOutput::OpenModelSettings);
            }
            ComposerMsg::RefreshModels(active_id) => {
                self.active_model_id = active_id;
                self.refresh_controls_button();
                self.refresh_attach_button();
            }
            ComposerMsg::SetSystemPrompt(prompt) => self.system_prompt = prompt,
            ComposerMsg::SetThinkingLevel(level) => self.thinking_level = level,
            ComposerMsg::SetAssistantName(name) => {
                self.assistant_name = name;
                self.refresh_controls_button();
            }
            ComposerMsg::SetContextUsed(tokens) => {
                self.context_used_tokens = tokens;
                self.refresh_controls_button();
            }
            ComposerMsg::SetGenerating(generating) => self.is_generating = generating,
        }
    }
}
