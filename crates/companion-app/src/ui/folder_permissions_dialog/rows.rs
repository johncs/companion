//! Access Manager row builders and file-manager launcher for granted folders and files.

use std::path::Path;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{gettext, tr_f};
use crate::ui::widgets::{clear_preferences_group, spawn_blocking_ui};

use super::FolderPermissionsState;
use super::view_model::GrantItem;

pub(super) fn populate_grant_group(
    group: &adw::PreferencesGroup,
    entries: &[GrantItem],
    is_file_group: bool,
    empty_title: &str,
    empty_subtitle: &str,
    state: &FolderPermissionsState,
) {
    clear_preferences_group(group);
    if entries.is_empty() {
        let empty = adw::ActionRow::builder()
            .title(glib::markup_escape_text(empty_title))
            .subtitle(glib::markup_escape_text(empty_subtitle))
            .build();
        empty.set_sensitive(false);
        group.add(&empty);
        return;
    }

    for item in entries {
        group.add(&build_grant_row(item, is_file_group, state));
    }
}

fn build_grant_row(
    item: &GrantItem,
    is_file_group: bool,
    state: &FolderPermissionsState,
) -> adw::ActionRow {
    let entry = &item.entry;
    let path_obj = Path::new(&entry.folder_path);
    let name = path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&entry.folder_path);
    let subtitle = if item.exists {
        entry.folder_path.clone()
    } else {
        tr_f("{path} (Missing)", &[("path", &entry.folder_path)])
    };

    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(name))
        .subtitle(glib::markup_escape_text(&subtitle))
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(if is_file_group {
        "text-x-generic-symbolic"
    } else {
        "folder-symbolic"
    }));

    let perm_dropdown = build_perm_dropdown(entry, state);
    let revoke_btn = build_revoke_button(&entry.id, state);

    row.add_suffix(&perm_dropdown);
    row.add_suffix(&build_open_path_button(
        &entry.folder_path,
        item.exists,
        item.is_file,
        &state.toast_overlay,
    ));
    row.add_suffix(&revoke_btn);
    row
}

fn build_perm_dropdown(
    entry: &crate::storage::ConversationFolder,
    state: &FolderPermissionsState,
) -> gtk::DropDown {
    let perm_model = gtk::StringList::new(&[&gettext("Read Only"), &gettext("Read & Write")]);
    let perm_dropdown = gtk::DropDown::builder()
        .model(&perm_model)
        .selected(if entry.permission_mode.allows_write() {
            1
        } else {
            0
        })
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Permission level"))
        .build();
    let (state, fid) = (state.clone(), entry.id.clone());
    let prev_sel = std::rc::Rc::new(std::cell::Cell::new(perm_dropdown.selected()));
    let reverting = std::rc::Rc::new(std::cell::Cell::new(false));
    perm_dropdown.connect_selected_notify(move |dd| {
        if reverting.get() {
            return;
        }
        apply_perm_selection(dd, &state, &fid, &prev_sel, &reverting);
    });
    perm_dropdown
}

fn apply_perm_selection(
    dd: &gtk::DropDown,
    state: &FolderPermissionsState,
    fid: &str,
    prev_sel: &std::rc::Rc<std::cell::Cell<u32>>,
    reverting: &std::rc::Rc<std::cell::Cell<bool>>,
) {
    let (new_sel, old_sel) = (dd.selected(), prev_sel.get());
    let mode = if new_sel == 1 {
        crate::core::types::FolderPermission::ReadWrite
    } else {
        crate::core::types::FolderPermission::Read
    };
    let (db, fid_bg, state_ui, dd_weak, prev_ref, rev_ref) = (
        state.db.clone(),
        fid.to_string(),
        state.clone(),
        dd.downgrade(),
        prev_sel.clone(),
        reverting.clone(),
    );
    spawn_blocking_ui(
        &state.rt_handle,
        move || {
            db.set_conversation_folder_permission(&fid_bg, mode)
                .map_err(|e| e.to_string())
        },
        move |res| {
            if let Err(err) = res {
                tracing::error!("Failed to update folder permission: {err}");
                if let Some(dd) = dd_weak.upgrade() {
                    rev_ref.set(true);
                    dd.set_selected(old_sel);
                    rev_ref.set(false);
                }
                state_ui.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Failed to update permission: {err}",
                    &[("err", &err)],
                )));
            } else {
                prev_ref.set(new_sel);
                (state_ui.on_changed)();
            }
        },
    );
}

fn build_revoke_button(folder_id: &str, state: &FolderPermissionsState) -> gtk::Button {
    let revoke_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Revoke access"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let state = state.clone();
    let fid = folder_id.to_string();
    revoke_btn.connect_clicked(move |_| {
        let db = state.db.clone();
        let fid_bg = fid.clone();
        let state_ui = state.clone();
        spawn_blocking_ui(
            &state.rt_handle,
            move || {
                db.remove_conversation_folder(&fid_bg)
                    .map_err(|e| e.to_string())
            },
            move |res| {
                if let Err(err) = res {
                    state_ui.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                        "Failed to revoke access: {err}",
                        &[("err", &err)],
                    )));
                } else {
                    (state_ui.on_changed)();
                    state_ui.trigger_refresh();
                }
            },
        );
    });
    revoke_btn
}

fn build_open_path_button(
    path: &str,
    exists: bool,
    is_file: bool,
    toast_overlay: &adw::ToastOverlay,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Open in File Manager"))
        .valign(gtk::Align::Center)
        .sensitive(exists)
        .css_classes(["flat", "circular"])
        .build();
    let target_path = path.to_string();
    let toast_overlay = toast_overlay.clone();
    btn.connect_clicked(move |b| {
        let toast_overlay = toast_overlay.clone();
        crate::ui::widgets::open_path_with_launcher(
            b,
            std::path::Path::new(&target_path),
            is_file,
            move |err| {
                toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Could not open folder: {err}",
                    &[("err", &err)],
                )));
            },
        );
    });
    btn
}
