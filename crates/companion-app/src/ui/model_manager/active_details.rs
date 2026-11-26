//! Active model detail rows syncing and installed model list row construction.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{gettext, tr_f};
use crate::models::format_bytes;
use crate::storage::{Database, ModelRecord};
use crate::ui::widgets::present_destructive_confirm;

use super::view_model::ActiveModelViewModel;
use crate::models::inventory::{InstalledModelInfo, delete_managed_model_files};

#[derive(Clone)]
pub(super) struct ActiveDetailRows {
    pub(super) config_btn: gtk::Button,
    pub(super) details_expander: adw::ExpanderRow,
    pub(super) file_row: adw::ActionRow,
    pub(super) size_row: adw::ActionRow,
    pub(super) quant_row: adw::ActionRow,
    pub(super) ctx_row: adw::ActionRow,
    pub(super) speed_row: adw::ActionRow,
    pub(super) text_row: adw::ActionRow,
    pub(super) thinking_row: adw::ActionRow,
    pub(super) vision_row: adw::ActionRow,
    pub(super) vision_action_btn: gtk::Button,
    pub(super) audio_row: adw::ActionRow,
}

pub(super) fn build_property_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_css_class("property");
    row
}

pub(super) fn sync_active_model_details(
    info: Option<&InstalledModelInfo>,
    rows: &ActiveDetailRows,
) {
    let Some(info) = info else {
        rows.config_btn.set_visible(false);
        rows.details_expander.set_visible(false);
        return;
    };

    let vm = ActiveModelViewModel::from_info(info);
    rows.file_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.file_sub));
    rows.file_row.set_tooltip_text(vm.file_tooltip.as_deref());
    rows.size_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.size_sub));
    rows.quant_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.quant_sub));
    rows.ctx_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.ctx_sub));
    rows.speed_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.speed_sub));
    rows.text_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.text_sub));
    rows.thinking_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.thinking_sub));
    rows.vision_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.vision_sub));
    if let Some(label) = vm.vision_action_label {
        rows.vision_action_btn.set_label(&label);
        rows.vision_action_btn.set_visible(true);
    } else {
        rows.vision_action_btn.set_visible(false);
    }
    rows.audio_row
        .set_subtitle(&gtk::glib::markup_escape_text(&vm.audio_sub));

    rows.config_btn.set_visible(true);
    rows.details_expander.set_visible(true);
}

/// Dependencies needed to render and delete an installed model row.
#[derive(Clone)]
pub(super) struct InstalledRowCtx {
    pub(super) db: Database,
    pub(super) installed_group: adw::PreferencesGroup,
    pub(super) active_id_state: Rc<RefCell<Option<String>>>,
    pub(super) on_active_changed: Rc<dyn Fn(Option<String>)>,
    pub(super) dialog: adw::PreferencesDialog,
    pub(super) on_refresh: Rc<dyn Fn()>,
    pub(super) models_dir: PathBuf,
    pub(super) rt_handle: tokio::runtime::Handle,
}

pub(super) fn build_installed_model_row(
    model: &ModelRecord,
    file_size: Option<u64>,
    is_active: bool,
    ctx: &InstalledRowCtx,
) -> (adw::ActionRow, gtk::Label) {
    let size_str = file_size
        .map(format_bytes)
        .unwrap_or_else(|| gettext("Missing file"));

    let row = adw::ActionRow::builder()
        .title(crate::format::markdown::escape_pango_markup(
            &model.display_name,
        ))
        .subtitle(crate::format::markdown::escape_pango_markup(&size_str))
        .build();

    let active_badge = gtk::Label::builder()
        .label(gettext("Active"))
        .valign(gtk::Align::Center)
        .margin_end(8)
        .css_classes(["dim-label", "caption-heading"])
        .visible(is_active)
        .build();
    row.add_suffix(&active_badge);

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    wire_delete_model_button(&delete_btn, model, ctx);
    row.add_suffix(&delete_btn);
    (row, active_badge)
}

fn wire_delete_model_button(delete_btn: &gtk::Button, model: &ModelRecord, ctx: &InstalledRowCtx) {
    let (
        db,
        installed_group,
        active_id_state,
        on_active_changed,
        dialog,
        on_refresh,
        model_clone,
        models_dir,
        rt_handle,
    ) = (
        ctx.db.clone(),
        ctx.installed_group.clone(),
        ctx.active_id_state.clone(),
        ctx.on_active_changed.clone(),
        ctx.dialog.clone(),
        ctx.on_refresh.clone(),
        model.clone(),
        ctx.models_dir.clone(),
        ctx.rt_handle.clone(),
    );
    delete_btn.connect_clicked(move |_| {
        let is_managed = model_clone.file_path.as_deref().is_some_and(|p| Path::new(p).starts_with(&models_dir));
        let body = if is_managed {
            tr_f("Delete \"{name}\" and remove its downloaded files from disk?", &[("name", &model_clone.display_name)])
        } else {
            tr_f("Remove \"{name}\" from Companion? (Your imported .gguf file on disk will not be deleted.)", &[("name", &model_clone.display_name)])
        };
        let (db, active_id_state, on_active_changed, dialog, on_refresh, model_inner, models_dir_inner, rt_handle_inner) = (
            db.clone(), active_id_state.clone(), on_active_changed.clone(), dialog.clone(),
            on_refresh.clone(), model_clone.clone(), models_dir.clone(), rt_handle.clone(),
        );
        present_destructive_confirm(&installed_group, &gettext("Remove AI Model?"), &body, &gettext("Remove"), move || {
            execute_model_deletion(ModelDeleteReq {
                db: db.clone(),
                active_id_state: active_id_state.clone(),
                on_active_changed: on_active_changed.clone(),
                dialog: dialog.clone(),
                on_refresh: on_refresh.clone(),
                model: model_inner.clone(),
                models_dir: models_dir_inner.clone(),
                rt_handle: rt_handle_inner.clone(),
            });
        });
    });
}

struct ModelDeleteReq {
    db: Database,
    active_id_state: Rc<RefCell<Option<String>>>,
    on_active_changed: Rc<dyn Fn(Option<String>)>,
    dialog: adw::PreferencesDialog,
    on_refresh: Rc<dyn Fn()>,
    model: ModelRecord,
    models_dir: std::path::PathBuf,
    rt_handle: tokio::runtime::Handle,
}

fn execute_model_deletion(req: ModelDeleteReq) {
    let ModelDeleteReq {
        db,
        active_id_state,
        on_active_changed,
        dialog,
        on_refresh,
        model: model_inner,
        models_dir: models_dir_inner,
        rt_handle,
    } = req;
    let model_id = model_inner.id.clone();
    let display_name = model_inner.display_name.clone();
    crate::ui::widgets::spawn_blocking_ui(
        &rt_handle,
        move || -> Result<(), String> {
            let others: Vec<ModelRecord> = db
                .list_models()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.id != model_inner.id)
                .collect();
            delete_managed_model_files(&models_dir_inner, &model_inner, &others);
            db.delete_model(&model_inner.id).map_err(|e| e.to_string())
        },
        move |res| match res {
            Ok(()) => {
                if active_id_state.borrow().as_deref() == Some(&model_id) {
                    *active_id_state.borrow_mut() = None;
                    on_active_changed(None);
                }
                dialog.add_toast(adw::Toast::new(&tr_f(
                    "Removed \"{name}\"",
                    &[("name", &display_name)],
                )));
                on_refresh();
            }
            Err(err) => {
                tracing::error!("Failed to delete model {model_id}: {err}");
                dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to remove model: {err}",
                    &[("err", &err)],
                )));
            }
        },
    );
}
