//! Access Manager dialog for viewing and managing per-chat directory & file permissions.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::{gettext, tr_f};
use crate::storage::{ConversationFolder, Database};
use crate::ui::widgets::{clear_preferences_group, dialog_content_box};

#[derive(Clone)]
struct FolderPermissionsState {
    db: Database,
    toast_overlay: adw::ToastOverlay,
    on_changed: Rc<dyn Fn()>,
    refresh_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
}

/// Present the Access Manager dialog for `conv_id`.
pub fn present_folder_permissions_dialog(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    conv_id: String,
    on_changed: impl Fn() + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Files & Folders"))
        .content_width(640)
        .content_height(560)
        .build();

    let toast_overlay = adw::ToastOverlay::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let content_box = dialog_content_box();

    let add_dir_btn = build_add_button(&gettext("Add Folder"));
    let dirs_group = adw::PreferencesGroup::builder()
        .title(gettext("Folders"))
        .description(gettext("Folders this chat can access"))
        .header_suffix(&add_dir_btn)
        .build();
    content_box.append(&dirs_group);

    let add_file_btn = build_add_button(&gettext("Add File"));
    let files_group = adw::PreferencesGroup::builder()
        .title(gettext("Files"))
        .description(gettext("Individual files this chat can access"))
        .header_suffix(&add_file_btn)
        .build();
    content_box.append(&files_group);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();
    toolbar.set_content(Some(&scroller));
    toast_overlay.set_child(Some(&toolbar));
    dialog.set_child(Some(&toast_overlay));

    let state = FolderPermissionsState {
        db: db.clone(),
        toast_overlay: toast_overlay.clone(),
        on_changed: Rc::new(on_changed),
        refresh_slot: Rc::new(RefCell::new(None)),
    };

    let refresh_fn: Rc<dyn Fn()> = {
        let state = state.clone();
        let conv_id = conv_id.clone();
        let dirs_group = dirs_group.clone();
        let files_group = files_group.clone();
        Rc::new(move || {
            let entries = state.db.list_conversation_folders(&conv_id).unwrap_or_default();
            let (file_entries, dir_entries): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| {
                let p = Path::new(&e.folder_path);
                if p.exists() { p.is_file() } else { p.extension().is_some() }
            });
            populate_grant_group(
                &dirs_group,
                &dir_entries,
                false,
                &gettext("No folders granted"),
                &gettext("Click + to grant access to a folder"),
                &state,
            );
            populate_grant_group(
                &files_group,
                &file_entries,
                true,
                &gettext("No files granted"),
                &gettext("Click + to grant access to a file"),
                &state,
            );
        })
    };

    *state.refresh_slot.borrow_mut() = Some(refresh_fn.clone());
    refresh_fn();

    let grant_chosen_path = Rc::new({
        let state = state.clone();
        let conv_id = conv_id.clone();
        let refresh_fn = refresh_fn.clone();
        move |res: Result<gio::File, glib::Error>| {
            if let Ok(file) = res
                && let Some(path) = file.path()
            {
                let existing = state.db.list_conversation_folders(&conv_id).unwrap_or_default();
                if let Some(parent_name) = find_covering_granted_folder(&existing, &path) {
                    state.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                        "Already covered by granted folder \"{name}\"",
                        &[("name", &parent_name)],
                    )));
                    return;
                }
                let path_str = std::fs::canonicalize(&path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                if let Err(err) = state.db.add_conversation_folder(&conv_id, &path_str) {
                    state.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                        "Failed to grant access: {err}",
                        &[("err", &err.to_string())],
                    )));
                } else {
                    (state.on_changed)();
                    refresh_fn();
                }
            }
        }
    });

    {
        let dialog_ref = dialog.clone();
        let cb = grant_chosen_path.clone();
        add_dir_btn.connect_clicked(move |_| {
            let picker = gtk::FileDialog::builder()
                .title(gettext("Add Folder"))
                .modal(true)
                .build();
            let window = dialog_ref
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());
            let cb = cb.clone();
            picker.select_folder(window.as_ref(), None::<&gio::Cancellable>, move |res| cb(res));
        });
    }

    {
        let dialog_ref = dialog.clone();
        let cb = grant_chosen_path;
        add_file_btn.connect_clicked(move |_| {
            let picker = gtk::FileDialog::builder()
                .title(gettext("Add File"))
                .modal(true)
                .build();
            let window = dialog_ref
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());
            let cb = cb.clone();
            picker.open(window.as_ref(), None::<&gio::Cancellable>, move |res| cb(res));
        });
    }

    dialog.present(Some(parent));
}

fn build_add_button(tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(tooltip)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build()
}

fn populate_grant_group(
    group: &adw::PreferencesGroup,
    entries: &[ConversationFolder],
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

    for entry in entries {
        group.add(&build_grant_row(entry, is_file_group, state));
    }
}

fn build_grant_row(
    entry: &ConversationFolder,
    is_file_group: bool,
    state: &FolderPermissionsState,
) -> adw::ActionRow {
    let path_obj = Path::new(&entry.folder_path);
    let name = path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&entry.folder_path);
    let subtitle = if path_obj.exists() {
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

    let perm_model = gtk::StringList::new(&[&gettext("Read Only"), &gettext("Read & Write")]);
    let perm_dropdown = gtk::DropDown::builder()
        .model(&perm_model)
        .selected(if entry.permission_mode.allows_write() { 1 } else { 0 })
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Permission level"))
        .build();
    {
        let state = state.clone();
        let fid = entry.id.clone();
        perm_dropdown.connect_selected_notify(move |dd| {
            let mode = if dd.selected() == 1 {
                crate::core::types::FolderPermission::ReadWrite
            } else {
                crate::core::types::FolderPermission::Read
            };
            if let Err(err) = state.db.set_conversation_folder_permission(&fid, mode) {
                state.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Failed to update permission: {err}",
                    &[("err", &err.to_string())],
                )));
            } else {
                (state.on_changed)();
            }
        });
    }

    let revoke_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Revoke access"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    {
        let state = state.clone();
        let fid = entry.id.clone();
        revoke_btn.connect_clicked(move |_| {
            if let Err(err) = state.db.remove_conversation_folder(&fid) {
                state.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Failed to revoke access: {err}",
                    &[("err", &err.to_string())],
                )));
            } else {
                (state.on_changed)();
                if let Some(rf) = state.refresh_slot.borrow().as_ref() {
                    rf();
                }
            }
        });
    }

    row.add_suffix(&perm_dropdown);
    row.add_suffix(&build_open_path_button(&entry.folder_path, &state.toast_overlay));
    row.add_suffix(&revoke_btn);
    row
}

fn build_open_path_button(path: &str, toast_overlay: &adw::ToastOverlay) -> gtk::Button {
    let is_file = Path::new(path).is_file();
    let btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Open in File Manager"))
        .valign(gtk::Align::Center)
        .sensitive(Path::new(path).exists())
        .css_classes(["flat", "circular"])
        .build();
    let target_path = path.to_string();
    let toast_overlay = toast_overlay.clone();
    btn.connect_clicked(move |b| {
        let file = gio::File::for_path(&target_path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        let window = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let toast_overlay = toast_overlay.clone();
        if is_file {
            launcher.open_containing_folder(
                window.as_ref(),
                None::<&gio::Cancellable>,
                move |res| {
                    if let Err(err) = res {
                        toast_overlay.add_toast(adw::Toast::new(&tr_f(
                            "Could not open folder: {err}",
                            &[("err", &err.to_string())],
                        )));
                    }
                },
            );
        } else {
            launcher.launch(window.as_ref(), None::<&gio::Cancellable>, move |res| {
                if let Err(err) = res {
                    toast_overlay.add_toast(adw::Toast::new(&tr_f(
                        "Could not open folder: {err}",
                        &[("err", &err.to_string())],
                    )));
                }
            });
        }
    });
    btn
}

/// If `candidate` is already inside (or equal to) a granted directory in `entries`, return that folder's display name.
fn find_covering_granted_folder(
    entries: &[ConversationFolder],
    candidate: &Path,
) -> Option<String> {
    let cand_canon = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    for entry in entries {
        let entry_path = Path::new(&entry.folder_path);
        if !entry_path.is_dir() {
            continue;
        }
        let dir_canon =
            std::fs::canonicalize(entry_path).unwrap_or_else(|_| entry_path.to_path_buf());
        if cand_canon.starts_with(&dir_canon) {
            let name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&entry.folder_path)
                .to_string();
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(path: &Path) -> ConversationFolder {
        ConversationFolder {
            id: "1".to_string(),
            conversation_id: "c1".to_string(),
            folder_path: path.to_string_lossy().to_string(),
            permission_mode: crate::core::types::FolderPermission::Read,
            created_at: 0,
        }
    }

    #[test]
    fn detects_file_or_subfolder_inside_granted_directory() {
        let root = std::env::temp_dir().join("companion_folder_cover_test");
        let _ = std::fs::remove_dir_all(&root);
        let granted_dir = root.join("Photos");
        let sub_dir = granted_dir.join("Raw");
        let sibling_dir = root.join("Other");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::create_dir_all(&sibling_dir).unwrap();
        let inside_file = granted_dir.join("PERSON.md");
        let outside_file = sibling_dir.join("notes.txt");
        std::fs::write(&inside_file, "hi").unwrap();
        std::fs::write(&outside_file, "bye").unwrap();

        let entries = vec![make_entry(&granted_dir)];
        assert_eq!(
            find_covering_granted_folder(&entries, &inside_file).as_deref(),
            Some("Photos")
        );
        assert_eq!(
            find_covering_granted_folder(&entries, &sub_dir).as_deref(),
            Some("Photos")
        );
        assert_eq!(find_covering_granted_folder(&entries, &outside_file), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
