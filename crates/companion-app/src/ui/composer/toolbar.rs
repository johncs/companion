//! Composer toolbar button refreshers, granted-file indexing, and Chat Controls dialog launcher.

use std::path::PathBuf;

use adw::prelude::*;
use relm4::prelude::*;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::ui::chat_controls_dialog::{ChatControlsConfig, present_chat_controls_dialog};
use crate::ui::icons;

use super::input_helpers::{compute_context_usage_pct, extract_model_n_ctx};
use super::{ComposerModel, ComposerMsg};

impl ComposerModel {
    pub(super) fn sync_mention_files(&self) {
        self.mention_picker.set_files(self.granted_files.clone());
    }

    pub(super) fn refresh_granted_files(&self, sender: &ComponentSender<Self>) {
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

    pub(super) fn has_active_model_mmproj(&self) -> bool {
        self.active_model_id
            .as_deref()
            .and_then(|id| self.models.iter().find(|m| m.id == id))
            .and_then(|m| m.mmproj_path.as_ref())
            .is_some()
    }

    pub(super) fn refresh_attach_button(&self) {
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

    pub(super) fn refresh_folders_button(&self) {
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
            let mode_label = if f.permission_mode.allows_write() {
                gettext("Read & Write")
            } else {
                gettext("Read-Only")
            };
            tip.push_str(&format!("\n• {} ({mode_label})", f.folder_path));
        }
        self.folders_btn.set_tooltip_text(Some(&tip));
    }

    pub(super) fn refresh_globe_button(&self) {
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

    pub(super) fn refresh_controls_button(&mut self) {
        self.models = self.db.list_models().unwrap_or_default();
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

    pub(super) fn open_chat_controls(&mut self, sender: &ComponentSender<Self>) {
        self.models = self.db.list_models().unwrap_or_default();
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
