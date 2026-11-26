//! Per-model inference configuration dialog and pure form-state logic.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use crate::core::ThinkingLevel;
use crate::format::markdown::escape_pango_markup;
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::models::gguf::GgufMetadataCache;
use crate::storage::{Database, ModelRecord};

use super::model_config_form_state::ModelConfigFormState;

/// Present the per-model settings [`adw::Dialog`].
pub fn present_model_config_dialog(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    rt_handle: tokio::runtime::Handle,
    gguf_cache: Arc<GgufMetadataCache>,
    model: ModelRecord,
    parent_dialog: adw::PreferencesDialog,
    on_saved: Rc<dyn Fn()>,
) {
    let crate::ui::dialog_scaffold::DialogScaffold {
        dialog,
        toolbar,
        primary_btn: save_btn,
    } = crate::ui::dialog_scaffold::build_dialog_scaffold(
        &gettext("Configure AI Model"),
        540,
        560,
        &gettext("Save"),
        false,
    );

    let (form, ctx_row, thinking_row, gpu_row, temp_row) =
        build_and_wire_dialog_body(&toolbar, &model, &gguf_cache, &rt_handle, &save_btn);
    wire_model_save_button(
        &save_btn,
        &dialog,
        &model,
        &form,
        (&ctx_row, &thinking_row, &gpu_row, &temp_row),
        (db, rt_handle, gguf_cache, parent_dialog, on_saved),
    );
    dialog.present(Some(parent));
}

fn build_and_wire_dialog_body(
    toolbar: &adw::ToolbarView,
    model: &ModelRecord,
    gguf_cache: &Arc<GgufMetadataCache>,
    rt_handle: &tokio::runtime::Handle,
    save_btn: &gtk::Button,
) -> (
    Rc<RefCell<ModelConfigFormState>>,
    adw::ComboRow,
    adw::ComboRow,
    adw::SwitchRow,
    adw::SpinRow,
) {
    let init_max_ctx = model.default_params.n_ctx.max(131072);
    let form = Rc::new(RefCell::new(ModelConfigFormState::from_model(
        model,
        init_max_ctx,
    )));
    let (reset_btn, info_group, (group, ctx_row, thinking_row, gpu_row, temp_row)) = (
        gtk::Button::with_label(&gettext("Reset to Default")),
        build_info_group(model),
        build_controls_group(&form.borrow(), init_max_ctx),
    );
    reset_btn.set_halign(gtk::Align::Start);
    spawn_gguf_max_ctx_probe(model, gguf_cache, rt_handle, &form, &ctx_row);
    wire_form_signals(
        &form,
        &ctx_row,
        &thinking_row,
        &gpu_row,
        &temp_row,
        save_btn,
        &reset_btn,
    );
    attach_dialog_scroller(toolbar, &info_group, &group, &reset_btn);
    (form, ctx_row, thinking_row, gpu_row, temp_row)
}

fn attach_dialog_scroller(
    toolbar: &adw::ToolbarView,
    info_group: &adw::PreferencesGroup,
    group: &adw::PreferencesGroup,
    reset_btn: &gtk::Button,
) {
    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.append(info_group);
    content_box.append(group);
    content_box.append(reset_btn);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();
    toolbar.set_content(Some(&scroller));
}

type ModelSaveDeps = (
    Database,
    tokio::runtime::Handle,
    Arc<GgufMetadataCache>,
    adw::PreferencesDialog,
    Rc<dyn Fn()>,
);

fn wire_model_save_button(
    save_btn: &gtk::Button,
    dialog: &adw::Dialog,
    model: &ModelRecord,
    form: &Rc<RefCell<ModelConfigFormState>>,
    rows: (
        &adw::ComboRow,
        &adw::ComboRow,
        &adw::SwitchRow,
        &adw::SpinRow,
    ),
    deps: ModelSaveDeps,
) {
    let (ctx_row, thinking_row, gpu_row, temp_row) = (
        rows.0.clone(),
        rows.1.clone(),
        rows.2.clone(),
        rows.3.clone(),
    );
    let (db, rt_handle, gguf_cache, parent_dialog, on_saved) = deps;
    let (d_save, model, form) = (dialog.downgrade(), model.clone(), form.clone());
    save_btn.connect_clicked(move |_| {
        let mut updated = model.clone();
        updated.default_params = form.borrow().to_model_params(
            ctx_row.selected(),
            thinking_row.selected(),
            gpu_row.is_active(),
            temp_row.value(),
            model.default_params.max_tokens,
        );
        save_model_config_async(
            &rt_handle,
            &db,
            &gguf_cache,
            updated,
            &parent_dialog,
            &on_saved,
            &d_save,
        );
    });
}

fn save_model_config_async(
    rt_handle: &tokio::runtime::Handle,
    db: &Database,
    gguf_cache: &Arc<GgufMetadataCache>,
    mut updated: ModelRecord,
    parent_dialog: &adw::PreferencesDialog,
    on_saved: &Rc<dyn Fn()>,
    d_save: &glib::WeakRef<adw::Dialog>,
) {
    let (db, cache, parent_dialog, on_saved, d_close) = (
        db.clone(),
        gguf_cache.clone(),
        parent_dialog.clone(),
        on_saved.clone(),
        d_save.clone(),
    );
    crate::ui::widgets::spawn_blocking_ui(
        rt_handle,
        move || -> Result<(), String> {
            updated.supports_thinking = updated
                .file_path
                .as_deref()
                .map(std::path::Path::new)
                .is_some_and(|p| cache.inspect(p).supports_thinking)
                || updated.supports_thinking;
            db.upsert_model(&updated).map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(()) => {
                parent_dialog.add_toast(adw::Toast::new(&gettext("Saved model settings")));
                on_saved();
                if let Some(d) = d_close.upgrade() {
                    d.close();
                }
            }
            Err(err) => {
                tracing::error!("Failed to save model settings: {err}");
                parent_dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to save: {err}",
                    &[("err", &err)],
                )));
            }
        },
    );
}

fn build_info_group(model: &ModelRecord) -> adw::PreferencesGroup {
    let info_group = adw::PreferencesGroup::new();
    let friendly = friendly_model_name(&model.display_name);
    let file_name = model
        .file_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name().and_then(|n| n.to_str()))
        .unwrap_or("—");
    for (title, value) in [
        (gettext("AI Model Name"), friendly.as_str()),
        (gettext("AI Model File"), file_name),
    ] {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(escape_pango_markup(value))
            .build();
        row.add_css_class("property");
        info_group.add(&row);
    }
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

    let thinking_row = build_default_thinking_row(form.init_thinking_idx);
    for w in [
        ctx_row.upcast_ref::<gtk::Widget>(),
        thinking_row.upcast_ref(),
        gpu_row.upcast_ref(),
        temp_row.upcast_ref(),
    ] {
        group.add(w);
    }
    (group, ctx_row, thinking_row, gpu_row, temp_row)
}

fn build_default_thinking_row(init_idx: u32) -> adw::ComboRow {
    let thinking_labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let thinking_refs: Vec<&str> = thinking_labels.iter().map(String::as_str).collect();
    let thinking_row = adw::ComboRow::builder()
        .title(gettext("Default Thinking Level"))
        .subtitle(gettext("Applied automatically when starting a new chat"))
        .model(&gtk::StringList::new(&thinking_refs))
        .build();
    thinking_row.set_selected(init_idx);
    thinking_row
}

fn spawn_gguf_max_ctx_probe(
    model: &ModelRecord,
    gguf_cache: &Arc<GgufMetadataCache>,
    rt_handle: &tokio::runtime::Handle,
    form: &Rc<RefCell<ModelConfigFormState>>,
    ctx_row: &adw::ComboRow,
) {
    let Some(model_path) = model.file_path.as_deref() else {
        return;
    };
    let path = std::path::PathBuf::from(model_path);
    let (cache, form_async, ctx_row_weak, model_async) = (
        gguf_cache.clone(),
        form.clone(),
        ctx_row.downgrade(),
        model.clone(),
    );
    crate::ui::widgets::spawn_blocking_ui(
        rt_handle,
        move || cache.inspect(&path).max_context,
        move |detected| {
            let Some(max_ctx) = detected else { return };
            let Some(row) = ctx_row_weak.upgrade() else {
                return;
            };
            let updated = ModelConfigFormState::from_model(&model_async, max_ctx);
            let labels: Vec<String> = updated
                .ctx_presets
                .iter()
                .map(|&v| crate::models::gguf::format_context_label(v))
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            row.set_model(Some(&gtk::StringList::new(&refs)));
            row.set_selected(updated.init_ctx_idx);
            row.set_subtitle(&tr_f(
                "Max supported by model: {max}",
                &[("max", &crate::models::gguf::format_short_context(max_ctx))],
            ));
            *form_async.borrow_mut() = updated;
        },
    );
}

fn wire_form_signals(
    form: &Rc<RefCell<ModelConfigFormState>>,
    ctx_row: &adw::ComboRow,
    thinking_row: &adw::ComboRow,
    gpu_row: &adw::SwitchRow,
    temp_row: &adw::SpinRow,
    save_btn: &gtk::Button,
    reset_btn: &gtk::Button,
) {
    let (f_c, c_c, th_c, g_c, te_c, s_c, r_c) = (
        form.clone(),
        ctx_row.clone(),
        thinking_row.clone(),
        gpu_row.clone(),
        temp_row.clone(),
        save_btn.clone(),
        reset_btn.clone(),
    );
    let update: Rc<dyn Fn()> = Rc::new(move || {
        let f = f_c.borrow();
        let (cc, ct, cg, cte) = (
            c_c.selected(),
            th_c.selected(),
            g_c.is_active(),
            (te_c.value() * 100.0).round() as i64,
        );
        s_c.set_sensitive(f.is_dirty(cc, ct, cg, cte));
        r_c.set_sensitive(!f.is_default(cc, ct, cg, cte));
    });
    update();
    let (u1, u2, u3) = (update.clone(), update.clone(), update.clone());
    ctx_row.connect_selected_notify(move |_| u1());
    thinking_row.connect_selected_notify(move |_| u2());
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
        ctx_r.set_selected(form_r.borrow().default_ctx_idx);
        thinking_r.set_selected(0);
        gpu_r.set_active(true);
        temp_r.set_value(1.0);
    });
}

#[cfg(test)]
mod tests {}
