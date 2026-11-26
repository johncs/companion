//! Composer toolbar button refreshers, granted-file indexing, and Chat Controls dialog launcher.

use std::path::PathBuf;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::ui::chat_controls_dialog::{ChatControlsConfig, present_chat_controls_dialog};
use crate::ui::icons;
use crate::ui::mention_picker::MentionEntry;

use super::input_helpers::{compute_context_usage_pct, extract_model_n_ctx};
use super::{ComposerModel, ComposerMsg};

impl ComposerModel {
    pub(super) fn spawn_init_data_load(&self, sender: &ComponentSender<Self>) {
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let assistant_name = db
                .get_assistant_name()
                .unwrap_or_else(|_| "Companion".to_string());
            let web_search_global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
            let search_engine = db.get_search_engine().unwrap_or_default();
            let models = db.list_models().unwrap_or_default();
            let _ = input_sender.send(ComposerMsg::InitDataLoaded {
                assistant_name,
                web_search_global_enabled,
                search_engine,
                models,
            });
        });
    }

    pub(super) fn sync_mention_files(&self) {
        self.mention_picker.set_files(self.granted_files.clone());
    }

    pub(super) fn refresh_granted_files(&mut self, sender: &ComponentSender<Self>) {
        self.files_seq = self.files_seq.wrapping_add(1);
        let seq = self.files_seq;
        if self.granted_folders.is_empty() {
            sender.input(ComposerMsg::GrantedFilesLoaded {
                seq,
                files: Vec::new(),
            });
            return;
        }
        let folders: Vec<String> = self
            .granted_folders
            .iter()
            .map(|f| f.folder_path.clone())
            .collect();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let mut entries = Vec::new();
            for folder in folders {
                let root = PathBuf::from(&folder);
                if root.is_file() {
                    entries.push(MentionEntry {
                        path: folder,
                        is_dir: false,
                    });
                } else if root.is_dir() {
                    entries.push(MentionEntry {
                        path: folder.clone(),
                        is_dir: true,
                    });
                    if let Ok(files) = crate::agent::tools::list_granted_files(&root, ".", 200) {
                        for rel in files {
                            let full = root.join(rel);
                            entries.push(MentionEntry {
                                is_dir: full.is_dir(),
                                path: full.to_string_lossy().to_string(),
                            });
                        }
                    }
                }
            }
            let _ = input_sender.send(ComposerMsg::GrantedFilesLoaded {
                seq,
                files: entries,
            });
        });
    }

    pub(super) fn has_active_model_mmproj(&self) -> bool {
        self.active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .and_then(|m| m.mmproj_path.as_ref())
            .is_some()
    }

    pub(super) fn refresh_attach_button(&self) {
        let (sensitive, tip) = super::view_model::ComposerViewModel::attach_button_state(
            self.active_model_id.is_some(),
            self.has_active_model_mmproj(),
        );
        self.attach_btn.set_sensitive(sensitive);
        self.attach_btn.set_tooltip_text(Some(&tip));
    }

    pub(super) fn refresh_folders_button(&self) {
        self.folders_btn
            .set_child(Some(&icons::symbolic_image("folder-symbolic", 16)));
        let (accent, tip) =
            super::view_model::ComposerViewModel::folders_button_state(&self.granted_folders);
        if accent {
            self.folders_btn
                .set_css_classes(&["flat", "circular", "accent"]);
        } else {
            self.folders_btn.set_css_classes(&["flat", "circular"]);
        }
        self.folders_btn.set_tooltip_text(Some(&tip));
    }

    pub(super) fn refresh_globe_button(&self) {
        self.globe_btn
            .set_child(Some(&icons::symbolic_image(icons::GLOBE, 16)));
        let (sensitive, accent, tip) = super::view_model::ComposerViewModel::globe_button_state(
            self.web_search_global_enabled,
            self.web_search_enabled,
            self.search_engine,
        );
        self.globe_btn.set_sensitive(sensitive);
        if accent {
            self.globe_btn
                .set_css_classes(&["flat", "circular", "accent"]);
        } else {
            self.globe_btn.set_css_classes(&["flat", "circular"]);
        }
        self.globe_btn.set_tooltip_text(Some(&tip));
    }

    pub(super) fn refresh_models_cache(&self, sender: &ComponentSender<Self>) {
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let models = db.list_models().unwrap_or_default();
            let _ = input_sender.send(ComposerMsg::ModelsMetadataLoaded(models));
        });
    }

    pub(super) fn refresh_search_engine(&self, sender: &ComponentSender<Self>) {
        let db = self.services.db.clone();
        let input_sender = sender.input_sender().clone();
        self.services.rt_handle.spawn_blocking(move || {
            let engine = db.get_search_engine().unwrap_or_default();
            let _ = input_sender.send(ComposerMsg::SearchEngineLoaded(engine));
        });
    }

    pub(super) fn refresh_controls_button(&self) {
        let active_name = self
            .active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .map(|m| friendly_model_name(&m.display_name))
            .unwrap_or_else(|| gettext("Select AI Model"));

        let active_ctx = extract_model_n_ctx(&self.models, self.active_model_id.as_deref());
        let pct = compute_context_usage_pct(self.context_used_tokens, active_ctx);

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

    pub(super) fn open_chat_controls(&self, sender: &ComponentSender<Self>) {
        let current_ctx = extract_model_n_ctx(&self.models, self.active_model_id.as_deref());
        let active_model = self
            .active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id));
        let sender_apply = sender.clone();
        let sender_manage = sender.clone();
        present_chat_controls_dialog(
            &self.controls_btn,
            ChatControlsConfig {
                active_model,
                gguf_cache: self.services.gguf_cache.clone(),
                rt_handle: self.services.rt_handle.clone(),
                assistant_name: &self.assistant_name,
                current_thinking: self.thinking_level,
                current_ctx,
                used_tokens: self.context_used_tokens,
                current_prompt: &self.system_prompt,
                on_apply: move |thinking_level, context_length, system_prompt| {
                    sender_apply.input(ComposerMsg::ControlsApplied {
                        thinking_level,
                        system_prompt,
                        context_length,
                    });
                },
                on_manage_models: move || {
                    sender_manage.input(ComposerMsg::OpenModelSettings);
                },
            },
        );
    }
}
