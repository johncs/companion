//! Chat composer bar: attachments (drag-and-drop + picker), unified Chat Controls, and Send/Stop.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::prelude::*;

use crate::core::attachments::{AttachmentKind, classify_path, process_attachment_file};
use crate::core::{ThinkingLevel, runtime};
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::storage::conversations::default_system_prompt;
use crate::storage::{AttachmentRecord, ConversationFolder, Database, ModelRecord};
use crate::ui::chat_controls_dialog::present_chat_controls_dialog;
use crate::ui::icons;
use crate::ui::mention_picker::MentionPicker;
use crate::ui::widgets::{build_document_tile, build_image_thumbnail, present_image_lightbox};

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

#[derive(Clone)]
struct PendingChip {
    temp_id: String,
    file_name: String,
    is_loading: bool,
    record: Option<AttachmentRecord>,
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
    attachments: Vec<PendingChip>,
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
            attachments: Vec::new(),
            granted_folders: Vec::new(),
            granted_files: Vec::new(),
            is_generating: false,
        };
        model.refresh_folders_button();
        model.refresh_globe_button();
        model.refresh_controls_button();
        model.refresh_attach_button();

        let widgets = view_output!();

        // Drag-and-drop support for files onto the composer card.
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

        // Handle image/file paste into the composer, or trim trailing blank lines on text paste.
        let sender_paste = sender.clone();
        text_view.connect_paste_clipboard(move |tv| {
            let clipboard = tv.clipboard();
            let formats = clipboard.formats();
            if formats.contains_type(gdk::Texture::static_type()) {
                tv.stop_signal_emission_by_name("paste-clipboard");
                let sender_clip = sender_paste.clone();
                clipboard.read_texture_async(None::<&gio::Cancellable>, move |res| match res {
                    Ok(Some(texture)) => {
                        let png_bytes = texture.save_to_png_bytes();
                        let dest_dir = glib::user_data_dir().join("companion").join("clipboard");
                        let dest_path =
                            dest_dir.join(format!("pasted-{}.png", uuid::Uuid::new_v4()));
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        runtime().spawn_blocking(move || {
                            let io_res = std::fs::create_dir_all(&dest_dir)
                                .and_then(|_| std::fs::write(&dest_path, png_bytes.as_ref()))
                                .map(|_| dest_path)
                                .map_err(|e| e.to_string());
                            let _ = tx.send(io_res);
                        });
                        let sender_ui = sender_clip.clone();
                        glib::MainContext::default().spawn_local(async move {
                            match rx.await {
                                Ok(Ok(path)) => {
                                    sender_ui.input(ComposerMsg::FilesDropped(vec![path]));
                                }
                                Ok(Err(err)) => {
                                    let _ = sender_ui.output(ComposerOutput::ShowToast(tr_f(
                                        "Failed to save pasted image: {err}",
                                        &[("err", &err)],
                                    )));
                                }
                                Err(_) => {}
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                            "Failed to read image from clipboard: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                });
                return;
            }

            if formats.contains_type(gdk::FileList::static_type()) {
                tv.stop_signal_emission_by_name("paste-clipboard");
                let sender_clip = sender_paste.clone();
                clipboard.read_value_async(
                    gdk::FileList::static_type(),
                    glib::Priority::DEFAULT,
                    None::<&gio::Cancellable>,
                    move |res| match res {
                        Ok(val) => {
                            if let Ok(file_list) = val.get::<gdk::FileList>() {
                                let paths: Vec<PathBuf> = file_list
                                    .files()
                                    .into_iter()
                                    .filter_map(|f| f.path())
                                    .filter(|p| p.is_file())
                                    .collect();
                                if !paths.is_empty() {
                                    sender_clip.input(ComposerMsg::FilesDropped(paths));
                                }
                            }
                        }
                        Err(err) => {
                            let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                                "Failed to read file from clipboard: {err}",
                                &[("err", &err.to_string())],
                            )));
                        }
                    },
                );
                return;
            }

            if formats.contains_type(gio::File::static_type()) {
                tv.stop_signal_emission_by_name("paste-clipboard");
                let sender_clip = sender_paste.clone();
                clipboard.read_value_async(
                    gio::File::static_type(),
                    glib::Priority::DEFAULT,
                    None::<&gio::Cancellable>,
                    move |res| match res {
                        Ok(val) => {
                            if let Ok(file) = val.get::<gio::File>()
                                && let Some(path) = file.path()
                                && path.is_file()
                            {
                                sender_clip.input(ComposerMsg::FilesDropped(vec![path]));
                            }
                        }
                        Err(err) => {
                            let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                                "Failed to read file from clipboard: {err}",
                                &[("err", &err.to_string())],
                            )));
                        }
                    },
                );
                return;
            }

            if formats.contain_mime_type("text/uri-list") {
                tv.stop_signal_emission_by_name("paste-clipboard");
                let sender_clip = sender_paste.clone();
                let tv_weak = tv.downgrade();
                clipboard.read_text_async(None::<&gio::Cancellable>, move |res| match res {
                    Ok(Some(text)) => {
                        let paths: Vec<PathBuf> = text
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty() && !l.starts_with('#'))
                            .filter_map(|l| {
                                let p = if l.starts_with("file://") {
                                    gio::File::for_uri(l).path()
                                } else {
                                    Some(PathBuf::from(l))
                                };
                                p.filter(|path| path.is_file())
                            })
                            .collect();
                        if !paths.is_empty() {
                            sender_clip.input(ComposerMsg::FilesDropped(paths));
                        } else if let Some(tv) = tv_weak.upgrade() {
                            tv.buffer().insert_at_cursor(&text);
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                            "Failed to read clipboard: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                });
                return;
            }

            let tv_weak = tv.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(tv) = tv_weak.upgrade() {
                    let buf = tv.buffer();
                    let (start, end) = buf.bounds();
                    let text = buf.text(&start, &end, false);
                    let trimmed = text.trim_end_matches(['\n', '\r']);
                    if trimmed.len() < text.len() {
                        buf.set_text(trimmed);
                    }
                    tv.queue_resize();
                }
            });
        });

        // Dynamically scale max input height to 45% of window height.
        if let Some(scroller) = text_view
            .parent()
            .and_then(|p| p.downcast::<gtk::ScrolledWindow>().ok())
        {
            scroller.connect_map(move |s| {
                if let Some(win) = s.root().and_downcast::<gtk::Window>() {
                    let update_max_height = {
                        let s = s.clone();
                        let win = win.clone();
                        move || {
                            let h = win.height();
                            if h > 0 {
                                let cap = ((h as f64) * 0.45).round().max(160.0) as i32;
                                s.set_max_content_height(cap);
                            }
                        }
                    };
                    update_max_height();

                    let u1 = update_max_height.clone();
                    win.connect_default_height_notify(move |_| u1());
                    let u2 = update_max_height;
                    win.connect_maximized_notify(move |_| u2());
                }
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ComposerMsg::SendPressed => {
                if self.is_generating {
                    sender.input(ComposerMsg::StopPressed);
                    return;
                }
                if self.attachments.iter().any(|a| a.is_loading) {
                    let _ = sender.output(ComposerOutput::ShowToast(gettext(
                        "Please wait for attached files to finish loading",
                    )));
                    return;
                }

                let buffer = self.text_view.buffer();
                let (start, end) = buffer.bounds();
                let text = buffer.text(&start, &end, false).trim().to_string();
                let ready_attachments: Vec<AttachmentRecord> = self
                    .attachments
                    .iter()
                    .filter_map(|a| a.record.clone())
                    .collect();

                if text.is_empty() && ready_attachments.is_empty() {
                    return;
                }

                buffer.set_text("");
                self.attachments.clear();
                self.rebuild_chips(&sender);

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
                let file_dialog = gtk::FileDialog::builder()
                    .title(gettext("Attach Files"))
                    .modal(true)
                    .build();
                let window = self
                    .text_view
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let sender_clone = sender.clone();

                file_dialog.open_multiple(
                    window.as_ref(),
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok(list) = res {
                            let mut paths = Vec::new();
                            for idx in 0..list.n_items() {
                                if let Some(file) = list.item(idx).and_downcast::<gio::File>()
                                    && let Some(p) = file.path()
                                {
                                    paths.push(p);
                                }
                            }
                            if !paths.is_empty() {
                                sender_clone.input(ComposerMsg::FilesDropped(paths));
                            }
                        }
                    },
                );
            }
            ComposerMsg::FilesDropped(paths) => {
                for path in paths {
                    if path.is_dir() {
                        let canon = std::fs::canonicalize(&path).unwrap_or(path);
                        let _ = sender.output(ComposerOutput::GrantFolder(canon));
                        continue;
                    }
                    if classify_path(&path) == AttachmentKind::Image && !self.has_active_model_mmproj() {
                        let _ = sender.output(ComposerOutput::ShowToast(gettext(
                            "Active AI model has no vision projector (mmproj) configured in Settings → AI Models",
                        )));
                        continue;
                    }

                    let temp_id = uuid::Uuid::new_v4().to_string();
                    let file_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    self.attachments.push(PendingChip {
                        temp_id: temp_id.clone(),
                        file_name,
                        is_loading: true,
                        record: None,
                    });
                    self.rebuild_chips(&sender);

                    let sender_clone = sender.clone();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    runtime().spawn_blocking(move || {
                        let res = process_attachment_file(&path).map_err(|e| e.to_string());
                        let _ = tx.send(res);
                    });
                    glib::MainContext::default().spawn_local(async move {
                        if let Ok(result) = rx.await {
                            sender_clone.input(ComposerMsg::AttachmentLoaded { temp_id, result });
                        }
                    });
                }
            }
            ComposerMsg::AttachmentLoaded { temp_id, result } => match result {
                Ok(record) => {
                    if let Some(chip) = self.attachments.iter_mut().find(|c| c.temp_id == temp_id) {
                        chip.is_loading = false;
                        chip.record = Some(record);
                    }
                    self.rebuild_chips(&sender);
                }
                Err(err) => {
                    self.attachments.retain(|c| c.temp_id != temp_id);
                    self.rebuild_chips(&sender);
                    let _ = sender.output(ComposerOutput::ShowToast(err));
                }
            },
            ComposerMsg::RemoveAttachment(temp_id) => {
                self.attachments.retain(|c| c.temp_id != temp_id);
                self.rebuild_chips(&sender);
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
            ComposerMsg::OpenChatControls => {
                self.models = self.db.list_models().unwrap_or_default();
                let current_ctx = self
                    .active_model_id
                    .as_deref()
                    .and_then(|id| self.models.iter().find(|m| m.id == id))
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(&m.default_params_json).ok())
                    .and_then(|v| v.get("n_ctx").and_then(|n| n.as_u64()))
                    .unwrap_or(4096) as u32;
                let active_model = self
                    .active_model_id
                    .as_deref()
                    .and_then(|id| self.models.iter().find(|m| m.id == id));
                let sender_apply = sender.clone();
                let sender_manage = sender.clone();
                present_chat_controls_dialog(
                    &self.controls_btn,
                    active_model,
                    &self.assistant_name,
                    self.thinking_level,
                    current_ctx,
                    self.context_used_tokens,
                    &self.system_prompt,
                    move |thinking_level, context_length, system_prompt| {
                        sender_apply.input(ComposerMsg::ControlsApplied {
                            thinking_level,
                            system_prompt,
                            context_length,
                        });
                    },
                    move || {
                        sender_manage.input(ComposerMsg::OpenModelSettings);
                    },
                );
            }
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
                    let mut val: serde_json::Value =
                        serde_json::from_str(&rec.default_params_json)
                            .unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert("n_ctx".to_string(), serde_json::json!(context_length));
                        rec.default_params_json = val.to_string();
                        let _ = self.db.upsert_model(&rec);
                    }
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
            ComposerMsg::SetSystemPrompt(prompt) => {
                self.system_prompt = prompt;
            }
            ComposerMsg::SetThinkingLevel(level) => {
                self.thinking_level = level;
            }
            ComposerMsg::SetAssistantName(name) => {
                self.assistant_name = name;
                self.refresh_controls_button();
            }
            ComposerMsg::SetContextUsed(tokens) => {
                self.context_used_tokens = tokens;
                self.refresh_controls_button();
            }
            ComposerMsg::SetGenerating(generating) => {
                self.is_generating = generating;
            }
        }
    }
}

impl ComposerModel {
    fn sync_mention_files(&self) {
        self.mention_picker.set_files(self.granted_files.clone());
    }

    fn refresh_granted_files(&self, sender: &ComponentSender<Self>) {
        if self.granted_folders.is_empty() {
            sender.input(ComposerMsg::GrantedFilesLoaded(Vec::new()));
            return;
        }
        let folders: Vec<String> = self
            .granted_folders
            .iter()
            .map(|f| f.folder_path.clone())
            .collect();
        let input_sender = sender.input_sender().clone();
        runtime().spawn_blocking(move || {
            let mut entries = Vec::new();
            for folder in folders {
                let root = PathBuf::from(&folder);
                if root.is_file() {
                    entries.push(folder);
                } else if root.is_dir() {
                    entries.push(folder.clone());
                    if let Ok(files) = crate::agent::tools::list_granted_files(&root, ".", 200) {
                        for rel in files {
                            entries.push(root.join(rel).to_string_lossy().to_string());
                        }
                    }
                }
            }
            let _ = input_sender.send(ComposerMsg::GrantedFilesLoaded(entries));
        });
    }

    fn has_active_model_mmproj(&self) -> bool {
        self.active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .and_then(|m| m.mmproj_path.as_ref())
            .is_some()
    }

    fn refresh_attach_button(&self) {
        if self.active_model_id.is_none() {
            self.attach_btn.set_sensitive(false);
            self.attach_btn.set_tooltip_text(Some(&gettext(
                "Select an AI model in Settings to attach files",
            )));
            return;
        }
        self.attach_btn.set_sensitive(true);
        let tip = if self.has_active_model_mmproj() {
            gettext("Attach files (images, text, PDF)")
        } else {
            gettext("Attach text or PDF files (active model has no vision projector for images)")
        };
        self.attach_btn.set_tooltip_text(Some(&tip));
    }

    fn refresh_folders_button(&self) {
        self.folders_btn
            .set_child(Some(&icons::symbolic_image("folder-symbolic", 16)));
        if self.granted_folders.is_empty() {
            self.folders_btn.set_css_classes(&["flat", "circular"]);
            self.folders_btn
                .set_tooltip_text(Some(&gettext("Files & Folders (none granted)")));
            return;
        }

        self.folders_btn
            .set_css_classes(&["flat", "circular", "accent"]);

        let mut tip = tr_f(
            "Files & Folders ({count} granted)",
            &[("count", &self.granted_folders.len().to_string())],
        );
        for f in &self.granted_folders {
            let mode_label = if f.permission_mode == "read_write" {
                gettext("Read & Write")
            } else {
                gettext("Read-Only")
            };
            tip.push_str(&format!("\n• {} ({mode_label})", f.folder_path));
        }
        self.folders_btn.set_tooltip_text(Some(&tip));
    }

    fn refresh_globe_button(&self) {
        self.globe_btn
            .set_child(Some(&icons::symbolic_image(icons::GLOBE, 16)));
        if !self.web_search_global_enabled {
            self.globe_btn.set_sensitive(false);
            self.globe_btn.set_css_classes(&["flat", "circular"]);
            self.globe_btn
                .set_tooltip_text(Some(&gettext("Web Search is disabled in Settings")));
            return;
        }
        self.globe_btn.set_sensitive(true);
        if self.web_search_enabled {
            self.globe_btn
                .set_css_classes(&["flat", "circular", "accent"]);
            let engine = self.db.get_search_engine().unwrap_or_default().label();
            self.globe_btn.set_tooltip_text(Some(&tr_f(
                "Web Search (On · {engine})",
                &[("engine", &engine)],
            )));
        } else {
            self.globe_btn.set_css_classes(&["flat", "circular"]);
            self.globe_btn
                .set_tooltip_text(Some(&gettext("Web Search (Off)")));
        }
    }

    fn refresh_controls_button(&mut self) {
        self.models = self.db.list_models().unwrap_or_default();
        let active_name = self
            .active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .map(|m| friendly_model_name(&m.display_name))
            .unwrap_or_else(|| gettext("Select AI Model"));

        let active_ctx = self
            .active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m.default_params_json).ok())
            .and_then(|v| v.get("n_ctx").and_then(|n| n.as_u64()))
            .unwrap_or(4096) as u32;
        let pct = ((self.context_used_tokens as f64 / active_ctx.max(1) as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u32;

        let btn_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        btn_content.append(&icons::symbolic_image(icons::PROMPT, 16));
        btn_content.append(&gtk::Label::new(Some(&self.assistant_name)));
        self.controls_btn.set_child(Some(&btn_content));
        self.controls_btn.set_tooltip_text(Some(&tr_f(
            "Chat Controls · {model} · Thinking: {thinking} · Context: {used}/{max} tokens ({pct}%)",
            &[
                ("model", &active_name),
                ("thinking", &self.thinking_level.label()),
                ("used", &self.context_used_tokens.to_string()),
                ("max", &active_ctx.to_string()),
                ("pct", &pct.to_string()),
            ],
        )));
    }

    fn rebuild_chips(&self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.chips_box.first_child() {
            self.chips_box.remove(&child);
        }

        if self.attachments.is_empty() {
            self.chips_box.set_visible(false);
            return;
        }
        self.chips_box.set_visible(true);

        for chip in &self.attachments {
            if chip.is_loading {
                let pill = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .tooltip_text(&chip.file_name)
                    .css_classes(["attachment-chip"])
                    .build();
                let spinner = gtk::Spinner::new();
                spinner.start();
                pill.append(&spinner);
                pill.append(
                    &gtk::Label::builder()
                        .label(&chip.file_name)
                        .css_classes(["caption"])
                        .ellipsize(gtk::pango::EllipsizeMode::Middle)
                        .max_width_chars(20)
                        .build(),
                );
                self.chips_box.append(&pill);
                continue;
            }

            if let Some(rec) = &chip.record {
                let overlay = gtk::Overlay::new();
                overlay.set_halign(gtk::Align::Start);
                overlay.set_valign(gtk::Align::Center);
                overlay.set_vexpand(false);
                if rec.mime_type.starts_with("image/") {
                    let thumb = build_image_thumbnail(&rec.file_path, 84);
                    let thumb_btn = gtk::Button::builder()
                        .child(&thumb)
                        .halign(gtk::Align::Start)
                        .valign(gtk::Align::Center)
                        .tooltip_text(&rec.file_name)
                        .css_classes(["flat", "thumb-btn"])
                        .build();
                    let fname = rec.file_name.clone();
                    let fpath = rec.file_path.clone();
                    let chips_box_ref = self.chips_box.clone();
                    thumb_btn.connect_clicked(move |_| {
                        present_image_lightbox(&chips_box_ref, &fname, &fpath);
                    });
                    overlay.set_child(Some(&thumb_btn));
                } else {
                    let doc_tile = build_document_tile(&rec.file_name, &rec.mime_type, 84);
                    overlay.set_child(Some(&doc_tile));
                }

                let remove_btn = gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .tooltip_text(&gettext("Remove attachment"))
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .css_classes(["thumb-remove-btn"])
                    .build();
                let tid = chip.temp_id.clone();
                let sender_clone = sender.clone();
                remove_btn.connect_clicked(move |_| {
                    sender_clone.input(ComposerMsg::RemoveAttachment(tid.clone()));
                });
                overlay.add_overlay(&remove_btn);

                self.chips_box.append(&overlay);
            }
        }
    }
}
