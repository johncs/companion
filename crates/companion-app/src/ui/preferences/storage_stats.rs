//! Hardware info, file-manager launch helpers, and parallel storage size computation.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::gio;

use crate::i18n::{gettext, tr_f};
use crate::models::format_bytes;
use crate::models::hardware::HardwareInfo;
use crate::ui::widgets::spawn_blocking_ui;

pub(super) fn build_hardware_group(hw: &HardwareInfo) -> adw::PreferencesGroup {
    let hw_group = adw::PreferencesGroup::builder()
        .title(gettext("System Hardware"))
        .build();
    let ram_str = if hw.total_ram_bytes > 0 {
        tr_f(
            "{total} total ({avail} available)",
            &[
                ("total", &format_bytes(hw.total_ram_bytes)),
                ("avail", &format_bytes(hw.available_ram_bytes)),
            ],
        )
    } else {
        gettext("Unknown")
    };
    let gpu_str = if hw.has_dri_gpu {
        gettext("GPU/DRI detected (Vulkan)")
    } else {
        gettext("CPU mode")
    };
    let hw_row = adw::ActionRow::builder()
        .title(gettext("Memory &amp; Acceleration"))
        .subtitle(format!("{ram_str} · {gpu_str}"))
        .build();
    hw_row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    hw_group.add(&hw_row);
    hw_group
}

pub(super) fn build_open_folder_button(
    dialog: &adw::PreferencesDialog,
    path: PathBuf,
    is_file: bool,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Open in File Manager"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    let dialog = dialog.clone();
    btn.connect_clicked(move |_| {
        open_path_in_file_manager(&dialog, &path, is_file);
    });
    btn
}

fn open_path_in_file_manager(dialog: &adw::PreferencesDialog, path: &Path, is_file: bool) {
    let dialog = dialog.clone();
    let path = path.to_path_buf();
    gtk::glib::MainContext::default().spawn_local(async move {
        if !is_file {
            let dir_path = path.clone();
            let dir_res = gio::spawn_blocking(move || std::fs::create_dir_all(&dir_path)).await;
            if let Ok(Err(err)) = dir_res {
                dialog.add_toast(adw::Toast::new(&tr_f(
                    "Failed to open folder: {err}",
                    &[("err", &err.to_string())],
                )));
                return;
            }
        }

        let dialog_toast = dialog.clone();
        crate::ui::widgets::open_path_with_launcher(&dialog, &path, is_file, move |err| {
            tracing::warn!("Failed to open path in file manager: {err}");
            dialog_toast.add_toast(adw::Toast::new(&tr_f(
                "Failed to open folder: {err}",
                &[("err", &err)],
            )));
        });
    });
}

pub(super) fn build_size_label() -> gtk::Label {
    gtk::Label::builder()
        .label(gettext("Calculating…"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "numeric"])
        .build()
}

pub(super) fn spawn_storage_size_refresh(
    rt_handle: &tokio::runtime::Handle,
    total_row: adw::ActionRow,
    db_size_label: gtk::Label,
    models_size_label: gtk::Label,
    db_path: PathBuf,
    models_dir: PathBuf,
) {
    spawn_blocking_ui(
        rt_handle,
        move || {
            let db_bytes = crate::storage::sqlite_storage_bytes(&db_path);
            let models_bytes = crate::models::inventory::compute_dir_size_parallel(&models_dir);
            (db_bytes, models_bytes)
        },
        move |(db_bytes, models_bytes)| {
            db_size_label.set_text(&format_bytes(db_bytes));
            models_size_label.set_text(&format_bytes(models_bytes));
            let total = db_bytes.saturating_add(models_bytes);
            total_row.set_subtitle(&format_bytes(total));
        },
    );
}
