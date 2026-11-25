//! Per-model inference configuration dialog.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::core::ThinkingLevel;
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::storage::{Database, ModelRecord};

/// Present the per-model settings [`adw::Dialog`].
pub fn present_model_config_dialog(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    model: ModelRecord,
    parent_dialog: adw::PreferencesDialog,
    refresh_weak: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
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

    let parsed: serde_json::Value =
        serde_json::from_str(&model.default_params_json).unwrap_or_default();
    let init_ctx = parsed.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(4096) as u32;
    let init_gpu_enabled = parsed
        .get("n_gpu_layers")
        .and_then(|v| v.as_u64())
        .unwrap_or(99)
        > 0;
    let init_temp = parsed
        .get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let init_thinking = model.default_thinking_level();
    let init_thinking_idx = ThinkingLevel::ALL
        .iter()
        .position(|t| *t == init_thinking)
        .unwrap_or(0) as u32;

    let info_group = adw::PreferencesGroup::new();
    let friendly = friendly_model_name(&model.display_name);
    let name_row = adw::ActionRow::builder()
        .title(gettext("AI Model Name"))
        .subtitle(gtk::glib::markup_escape_text(&friendly))
        .build();
    name_row.add_css_class("property");

    let file_name = model
        .file_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name().and_then(|n| n.to_str()))
        .unwrap_or("—");
    let file_row = adw::ActionRow::builder()
        .title(gettext("AI Model File"))
        .subtitle(gtk::glib::markup_escape_text(file_name))
        .build();
    file_row.add_css_class("property");
    info_group.add(&name_row);
    info_group.add(&file_row);

    let group = adw::PreferencesGroup::new();

    let max_ctx = model
        .file_path
        .as_deref()
        .and_then(|p| crate::models::detect_gguf_max_context(std::path::Path::new(p)))
        .unwrap_or(131072);
    let ctx_presets = crate::models::gguf::context_presets_up_to(max_ctx);
    let ctx_labels: Vec<String> = ctx_presets
        .iter()
        .map(|&v| crate::models::gguf::format_context_label(v))
        .collect();
    let ctx_label_refs: Vec<&str> = ctx_labels.iter().map(String::as_str).collect();
    let ctx_row = adw::ComboRow::builder()
        .title(gettext("Default Context Window"))
        .subtitle(tr_f(
            "Max supported by model: {max}",
            &[("max", &crate::models::gguf::format_short_context(max_ctx))],
        ))
        .model(&gtk::StringList::new(&ctx_label_refs))
        .build();
    let init_idx = ctx_presets
        .iter()
        .rposition(|&v| v <= init_ctx.min(max_ctx))
        .unwrap_or(0);
    ctx_row.set_selected(init_idx as u32);

    let gpu_row = adw::SwitchRow::builder()
        .title(gettext("Use GPU Acceleration"))
        .subtitle(gettext("Run model on GPU via Vulkan"))
        .active(init_gpu_enabled)
        .build();

    let temp_row = adw::SpinRow::with_range(0.0, 2.0, 0.05);
    temp_row.set_title(&gettext("Temperature"));
    temp_row.set_digits(2);
    temp_row.set_value(init_temp);

    let thinking_labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let thinking_refs: Vec<&str> = thinking_labels.iter().map(String::as_str).collect();
    let thinking_row = adw::ComboRow::builder()
        .title(gettext("Default Thinking Level"))
        .subtitle(gettext("Applied automatically when starting a new chat"))
        .model(&gtk::StringList::new(&thinking_refs))
        .build();
    thinking_row.set_selected(init_thinking_idx);

    let detected_thinking = model
        .file_path
        .as_deref()
        .map(std::path::Path::new)
        .is_some_and(crate::models::detect_gguf_supports_thinking)
        || model.supports_thinking;

    group.add(&ctx_row);
    group.add(&thinking_row);
    group.add(&gpu_row);
    group.add(&temp_row);

    let default_ctx_idx = ctx_presets
        .iter()
        .rposition(|&v| v <= 4096_u32.min(max_ctx))
        .unwrap_or(0) as u32;
    let init_temp_cents = (init_temp * 100.0).round() as i64;

    let reset_btn = gtk::Button::builder()
        .label(&gettext("Reset to Default"))
        .halign(gtk::Align::Start)
        .build();

    let update_button_states: Rc<dyn Fn()> = {
        let ctx_row = ctx_row.clone();
        let thinking_row = thinking_row.clone();
        let gpu_row = gpu_row.clone();
        let temp_row = temp_row.clone();
        let save_btn = save_btn.clone();
        let reset_btn = reset_btn.clone();
        Rc::new(move || {
            let cur_ctx = ctx_row.selected();
            let cur_thinking = thinking_row.selected();
            let cur_gpu = gpu_row.is_active();
            let cur_temp = (temp_row.value() * 100.0).round() as i64;

            let is_dirty = cur_ctx != init_idx as u32
                || cur_thinking != init_thinking_idx
                || cur_gpu != init_gpu_enabled
                || cur_temp != init_temp_cents;
            let is_default = cur_ctx == default_ctx_idx
                && cur_thinking == 0
                && cur_gpu
                && cur_temp == 100;

            save_btn.set_sensitive(is_dirty);
            reset_btn.set_sensitive(!is_default);
        })
    };
    update_button_states();

    {
        let update = update_button_states.clone();
        ctx_row.connect_selected_notify(move |_| update());
    }
    {
        let update = update_button_states.clone();
        thinking_row.connect_selected_notify(move |_| update());
    }
    {
        let update = update_button_states.clone();
        gpu_row.connect_active_notify(move |_| update());
    }
    {
        let update = update_button_states;
        temp_row.connect_value_notify(move |_| update());
    }
    {
        let ctx_row = ctx_row.clone();
        let thinking_row = thinking_row.clone();
        let gpu_row = gpu_row.clone();
        let temp_row = temp_row.clone();
        reset_btn.connect_clicked(move |_| {
            ctx_row.set_selected(default_ctx_idx);
            thinking_row.set_selected(0);
            gpu_row.set_active(true);
            temp_row.set_value(1.0);
        });
    }

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

    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            d.close();
        });
    }
    {
        let d = dialog.clone();
        save_btn.connect_clicked(move |_| {
            let selected_ctx = ctx_presets
                .get(ctx_row.selected() as usize)
                .copied()
                .unwrap_or(4096);
            let n_gpu_layers: u32 = if gpu_row.is_active() { 99 } else { 0 };
            let params_json = serde_json::json!({
                "n_ctx": selected_ctx,
                "n_gpu_layers": n_gpu_layers,
                "temperature": (temp_row.value() * 100.0).round() / 100.0,
                "thinking_level": ThinkingLevel::from_index(thinking_row.selected()).as_str(),
            })
            .to_string();

            let mut updated = model.clone();
            updated.supports_thinking = detected_thinking;
            updated.default_params_json = params_json;

            if let Err(err) = db.upsert_model(&updated) {
                parent_dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to save: {err}",
                    &[("err", &err.to_string())],
                )));
            } else {
                parent_dialog.add_toast(adw::Toast::new(&gettext("Saved model settings")));
                if let Some(f) = refresh_weak.borrow().as_ref() {
                    f();
                }
            }
            d.close();
        });
    }

    dialog.present(Some(parent));
}
