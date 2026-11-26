//! Per-model inference configuration dialog and pure form-state logic.

use std::rc::Rc;

use adw::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::markdown::escape_pango_markup;
use crate::core::model_params::ModelParams;
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::storage::{Database, ModelRecord};

/// Pure, unit-testable form state for [`present_model_config_dialog`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelConfigFormState {
    pub ctx_presets: Vec<u32>,
    pub init_ctx_idx: u32,
    pub default_ctx_idx: u32,
    pub init_thinking_idx: u32,
    pub init_gpu_enabled: bool,
    pub init_temp_cents: i64,
}

impl ModelConfigFormState {
    pub fn from_model(model: &ModelRecord, max_ctx: u32) -> Self {
        let ctx_presets = crate::models::gguf::context_presets_up_to(max_ctx);
        let init_ctx = model.default_params.n_ctx;
        let init_ctx_idx = ctx_presets
            .iter()
            .rposition(|&v| v <= init_ctx.min(max_ctx))
            .unwrap_or(0) as u32;
        let default_ctx_idx = ctx_presets
            .iter()
            .rposition(|&v| v <= 4096_u32.min(max_ctx))
            .unwrap_or(0) as u32;
        let init_thinking = model.default_thinking_level();
        let init_thinking_idx = ThinkingLevel::ALL
            .iter()
            .position(|t| *t == init_thinking)
            .unwrap_or(0) as u32;
        let init_temp_cents = (f64::from(model.default_params.temperature) * 100.0).round() as i64;
        Self {
            ctx_presets,
            init_ctx_idx,
            default_ctx_idx,
            init_thinking_idx,
            init_gpu_enabled: model.default_params.n_gpu_layers > 0,
            init_temp_cents,
        }
    }

    pub fn is_dirty(&self, ctx_idx: u32, thinking_idx: u32, gpu: bool, temp_cents: i64) -> bool {
        ctx_idx != self.init_ctx_idx
            || thinking_idx != self.init_thinking_idx
            || gpu != self.init_gpu_enabled
            || temp_cents != self.init_temp_cents
    }

    pub fn is_default(&self, ctx_idx: u32, thinking_idx: u32, gpu: bool, temp_cents: i64) -> bool {
        ctx_idx == self.default_ctx_idx && thinking_idx == 0 && gpu && temp_cents == 100
    }

    pub fn to_model_params(
        &self,
        ctx_idx: u32,
        thinking_idx: u32,
        gpu: bool,
        temp_value: f64,
        existing_max_tokens: u32,
    ) -> ModelParams {
        ModelParams {
            n_ctx: self
                .ctx_presets
                .get(ctx_idx as usize)
                .copied()
                .unwrap_or(4096),
            n_gpu_layers: if gpu { 99 } else { 0 },
            max_tokens: existing_max_tokens,
            temperature: ((temp_value * 100.0).round() / 100.0) as f32,
            thinking_level: ThinkingLevel::from_index(thinking_idx),
        }
    }
}

/// Present the per-model settings [`adw::Dialog`].
pub fn present_model_config_dialog(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    model: ModelRecord,
    parent_dialog: adw::PreferencesDialog,
    on_saved: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Configure AI Model"))
        .content_width(540)
        .content_height(560)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel_btn = gtk::Button::with_label(&gettext("Cancel"));
    let save_btn = gtk::Button::with_label(&gettext("Save"));
    save_btn.add_css_class("suggested-action");
    save_btn.set_sensitive(false);
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);
    toolbar.add_top_bar(&header);

    let max_ctx = model
        .file_path
        .as_deref()
        .and_then(|p| crate::models::detect_gguf_max_context(std::path::Path::new(p)))
        .unwrap_or(131072);
    let form = Rc::new(ModelConfigFormState::from_model(&model, max_ctx));

    let info_group = build_info_group(&model);
    let (group, ctx_row, thinking_row, gpu_row, temp_row) = build_controls_group(&form, max_ctx);
    let reset_btn = gtk::Button::builder()
        .label(gettext("Reset to Default"))
        .halign(gtk::Align::Start)
        .build();

    wire_form_signals(
        &form,
        &ctx_row,
        &thinking_row,
        &gpu_row,
        &temp_row,
        &save_btn,
        &reset_btn,
    );

    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.append(&info_group);
    content_box.append(&group);
    content_box.append(&reset_btn);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    let d_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| {
        d_cancel.close();
    });

    let d_save = dialog.clone();
    save_btn.connect_clicked(move |_| {
        let mut updated = model.clone();
        updated.default_params = form.to_model_params(
            ctx_row.selected(),
            thinking_row.selected(),
            gpu_row.is_active(),
            temp_row.value(),
            model.default_params.max_tokens,
        );
        let db = db.clone();
        let parent_dialog = parent_dialog.clone();
        let on_saved = on_saved.clone();
        crate::ui::widgets::spawn_blocking_ui(
            move || -> Result<(), String> {
                updated.supports_thinking = updated
                    .file_path
                    .as_deref()
                    .map(std::path::Path::new)
                    .is_some_and(crate::models::detect_gguf_supports_thinking)
                    || updated.supports_thinking;
                db.upsert_model(&updated).map_err(|e| e.to_string())
            },
            move |res| match res {
                Ok(()) => {
                    parent_dialog.add_toast(adw::Toast::new(&gettext("Saved model settings")));
                    on_saved();
                }
                Err(err) => {
                    parent_dialog.add_toast(adw::Toast::new(&tr_f(
                        "Failed to save: {err}",
                        &[("err", &err)],
                    )));
                }
            },
        );
        d_save.close();
    });

    dialog.present(Some(parent));
}

fn build_info_group(model: &ModelRecord) -> adw::PreferencesGroup {
    let info_group = adw::PreferencesGroup::new();
    let friendly = friendly_model_name(&model.display_name);
    let name_row = adw::ActionRow::builder()
        .title(gettext("AI Model Name"))
        .subtitle(escape_pango_markup(&friendly))
        .build();
    name_row.add_css_class("property");

    let file_name = model
        .file_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name().and_then(|n| n.to_str()))
        .unwrap_or("—");
    let file_row = adw::ActionRow::builder()
        .title(gettext("AI Model File"))
        .subtitle(escape_pango_markup(file_name))
        .build();
    file_row.add_css_class("property");
    info_group.add(&name_row);
    info_group.add(&file_row);
    info_group
}

fn build_controls_group(
    form: &ModelConfigFormState,
    max_ctx: u32,
) -> (
    adw::PreferencesGroup,
    adw::ComboRow,
    adw::ComboRow,
    adw::SwitchRow,
    adw::SpinRow,
) {
    let group = adw::PreferencesGroup::new();
    let ctx_labels: Vec<String> = form
        .ctx_presets
        .iter()
        .map(|&v| crate::models::gguf::format_context_label(v))
        .collect();
    let ctx_refs: Vec<&str> = ctx_labels.iter().map(String::as_str).collect();
    let ctx_row = adw::ComboRow::builder()
        .title(gettext("Default Context Window"))
        .subtitle(tr_f(
            "Max supported by model: {max}",
            &[("max", &crate::models::gguf::format_short_context(max_ctx))],
        ))
        .model(&gtk::StringList::new(&ctx_refs))
        .build();
    ctx_row.set_selected(form.init_ctx_idx);

    let gpu_row = adw::SwitchRow::builder()
        .title(gettext("Use GPU Acceleration"))
        .subtitle(gettext("Run model on GPU via Vulkan"))
        .active(form.init_gpu_enabled)
        .build();

    let temp_row = adw::SpinRow::with_range(0.0, 2.0, 0.05);
    temp_row.set_title(&gettext("Temperature"));
    temp_row.set_digits(2);
    temp_row.set_value(form.init_temp_cents as f64 / 100.0);

    let thinking_labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let thinking_refs: Vec<&str> = thinking_labels.iter().map(String::as_str).collect();
    let thinking_row = adw::ComboRow::builder()
        .title(gettext("Default Thinking Level"))
        .subtitle(gettext("Applied automatically when starting a new chat"))
        .model(&gtk::StringList::new(&thinking_refs))
        .build();
    thinking_row.set_selected(form.init_thinking_idx);

    group.add(&ctx_row);
    group.add(&thinking_row);
    group.add(&gpu_row);
    group.add(&temp_row);
    (group, ctx_row, thinking_row, gpu_row, temp_row)
}

fn wire_form_signals(
    form: &Rc<ModelConfigFormState>,
    ctx_row: &adw::ComboRow,
    thinking_row: &adw::ComboRow,
    gpu_row: &adw::SwitchRow,
    temp_row: &adw::SpinRow,
    save_btn: &gtk::Button,
    reset_btn: &gtk::Button,
) {
    let update: Rc<dyn Fn()> = {
        let (form, ctx_row, thinking_row, gpu_row, temp_row, save_btn, reset_btn) = (
            form.clone(),
            ctx_row.clone(),
            thinking_row.clone(),
            gpu_row.clone(),
            temp_row.clone(),
            save_btn.clone(),
            reset_btn.clone(),
        );
        Rc::new(move || {
            let cur_ctx = ctx_row.selected();
            let cur_thinking = thinking_row.selected();
            let cur_gpu = gpu_row.is_active();
            let cur_temp = (temp_row.value() * 100.0).round() as i64;
            save_btn.set_sensitive(form.is_dirty(cur_ctx, cur_thinking, cur_gpu, cur_temp));
            reset_btn.set_sensitive(!form.is_default(cur_ctx, cur_thinking, cur_gpu, cur_temp));
        })
    };
    update();

    let u1 = update.clone();
    ctx_row.connect_selected_notify(move |_| u1());
    let u2 = update.clone();
    thinking_row.connect_selected_notify(move |_| u2());
    let u3 = update.clone();
    gpu_row.connect_active_notify(move |_| u3());
    temp_row.connect_value_notify(move |_| update());

    let (form_r, ctx_r, thinking_r, gpu_r, temp_r) = (
        form.clone(),
        ctx_row.clone(),
        thinking_row.clone(),
        gpu_row.clone(),
        temp_row.clone(),
    );
    reset_btn.connect_clicked(move |_| {
        ctx_r.set_selected(form_r.default_ctx_idx);
        thinking_r.set_selected(0);
        gpu_r.set_active(true);
        temp_r.set_value(1.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_model(params: ModelParams) -> ModelRecord {
        ModelRecord {
            id: "m1".into(),
            display_name: "Test".into(),
            provider_type: "llama_cpp".into(),
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: true,
            default_params: params,
            avg_tokens_per_sec: None,
        }
    }

    #[test]
    fn form_state_tracks_dirty_default_and_save_params() {
        let model = dummy_model(ModelParams::default());
        let state = ModelConfigFormState::from_model(&model, 32768);

        assert!(!state.is_dirty(state.init_ctx_idx, 0, true, 100));
        assert!(state.is_default(state.default_ctx_idx, 0, true, 100));

        assert!(state.is_dirty(state.init_ctx_idx + 1, 0, true, 100));
        assert!(!state.is_default(state.default_ctx_idx, 1, false, 70));

        let saved = state.to_model_params(state.default_ctx_idx, 1, false, 0.7, 2048);
        assert_eq!(saved.n_ctx, 4096);
        assert_eq!(saved.n_gpu_layers, 0);
        assert_eq!(saved.max_tokens, 2048);
        assert!((saved.temperature - 0.7).abs() < 1e-4);
        assert_eq!(saved.thinking_level, ThinkingLevel::from_index(1));
    }
}
