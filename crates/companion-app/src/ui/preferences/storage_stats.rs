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

fn open_path_in_file_manager(dialog: &adw::PreferencesDialog, path: &PathBuf, is_file: bool) {
    if !is_file
        && !path.exists()
        && let Err(err) = std::fs::create_dir_all(path)
    {
        dialog.add_toast(adw::Toast::new(&tr_f(
            "Failed to open folder: {err}",
            &[("err", &err.to_string())],
        )));
        return;
    }

    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    let window = dialog.root().and_downcast::<gtk::Window>();
    let dialog_toast = dialog.clone();

    let handle_result = move |res: Result<(), gtk::glib::Error>| {
        if let Err(err) = res {
            tracing::warn!("Failed to open path in file manager: {err}");
            dialog_toast.add_toast(adw::Toast::new(&tr_f(
                "Failed to open folder: {err}",
                &[("err", &err.to_string())],
            )));
        }
    };

    if is_file {
        launcher.open_containing_folder(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    } else {
        launcher.launch(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    }
}

pub(super) fn build_size_label() -> gtk::Label {
    gtk::Label::builder()
        .label(gettext("Calculating…"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "numeric"])
        .build()
}

pub(super) fn spawn_storage_size_refresh(
    total_row: adw::ActionRow,
    db_size_label: gtk::Label,
    models_size_label: gtk::Label,
    db_path: PathBuf,
    models_dir: PathBuf,
) {
    spawn_blocking_ui(
        move || {
            std::thread::scope(|s| {
                let db_handle = s.spawn(|| compute_sqlite_storage_bytes(&db_path));
                let models_bytes = compute_dir_size_parallel(&models_dir);
                let db_bytes = db_handle.join().unwrap_or(0);
                (db_bytes, models_bytes)
            })
        },
        move |(db_bytes, models_bytes)| {
            db_size_label.set_text(&format_bytes(db_bytes));
            models_size_label.set_text(&format_bytes(models_bytes));
            let total = db_bytes.saturating_add(models_bytes);
            total_row.set_subtitle(&format_bytes(total));
        },
    );
}

fn compute_sqlite_storage_bytes(db_path: &Path) -> u64 {
    let base = db_path.as_os_str();
    let mut wal_os = base.to_os_string();
    wal_os.push("-wal");
    let mut shm_os = base.to_os_string();
    shm_os.push("-shm");

    [db_path, Path::new(&wal_os), Path::new(&shm_os)]
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn compute_dir_size_parallel(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    if entries.is_empty() {
        return 0;
    }

    let has_subdirs = entries
        .iter()
        .any(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false));
    let min_parallel = if has_subdirs { 1 } else { 16 };
    crate::core::parallel_chunks(&entries, min_parallel, sum_entries_iterative)
        .into_iter()
        .sum()
}

fn visit_dir_entry(entry: &std::fs::DirEntry, dir_stack: &mut Vec<PathBuf>) -> u64 {
    let Ok(ft) = entry.file_type() else {
        return 0;
    };
    if ft.is_file() {
        entry.metadata().map(|m| m.len()).unwrap_or(0)
    } else if ft.is_dir() {
        dir_stack.push(entry.path());
        0
    } else {
        0
    }
}

fn sum_entries_iterative(entries: &[std::fs::DirEntry]) -> u64 {
    let mut total = 0u64;
    let mut dir_stack: Vec<PathBuf> = Vec::new();

    for entry in entries {
        total = total.saturating_add(visit_dir_entry(entry, &mut dir_stack));
    }

    while let Some(dir) = dir_stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            total = total.saturating_add(visit_dir_entry(&entry, &mut dir_stack));
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_storage_sums_db_and_wal_shm_sidecars() {
        let dir = std::env::temp_dir().join(format!("companion_pref_db_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("companion.db");
        std::fs::write(&db_path, vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("companion.db-wal"), vec![2u8; 250]).unwrap();
        std::fs::write(dir.join("companion.db-shm"), vec![3u8; 50]).unwrap();

        assert_eq!(compute_sqlite_storage_bytes(&db_path), 400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_dir_size_handles_nested_and_flat_dirs() {
        let dir = std::env::temp_dir().join(format!("companion_pref_dir_{}", uuid::Uuid::new_v4()));
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        for i in 0..20 {
            std::fs::write(dir.join(format!("file_{i}.bin")), vec![0u8; 10]).unwrap();
        }
        std::fs::write(sub.join("deep.bin"), vec![0u8; 55]).unwrap();

        assert_eq!(compute_dir_size_parallel(&dir), 255);
        assert_eq!(compute_dir_size_parallel(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
