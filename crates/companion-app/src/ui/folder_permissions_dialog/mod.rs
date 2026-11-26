//! Access Manager dialog for viewing and managing per-chat directory & file permissions.

mod rows;
mod view_model;

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::agent::tools::find_covering_granted_folder;
use crate::i18n::{gettext, tr_f};
use crate::storage::Database;
use crate::ui::widgets::{dialog_content_box, spawn_blocking_ui};

use rows::populate_grant_group;
use view_model::partition_grants;

#[derive(Clone)]
struct FolderPermissionsState {
    db: Database,
    rt_handle: tokio::runtime::Handle,
    toast_overlay: adw::ToastOverlay,
    on_changed: Rc<dyn Fn()>,
    refresh_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

impl FolderPermissionsState {
    fn trigger_refresh(&self) {
        let _ = self.refresh_tx.send(());
    }
}

/// Present the Access Manager dialog for `conv_id`.
pub fn present_folder_permissions_dialog(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    rt_handle: tokio::runtime::Handle,
    conv_id: String,
    on_changed: impl Fn() + 'static,
) {
    let (dialog, toast_overlay, add_dir_btn, add_file_btn, dirs_group, files_group) =
        build_folder_permissions_chrome();

    let (refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let state = FolderPermissionsState {
        db,
        rt_handle,
        toast_overlay,
        on_changed: Rc::new(on_changed),
        refresh_tx,
    };

    let closed = Rc::new(std::cell::Cell::new(false));
    let (state_c, closed_c) = (state.clone(), closed.clone());
    dialog.connect_closed(move |_| {
        closed_c.set(true);
        state_c.trigger_refresh();
    });

    spawn_grant_refresh_loop(
        refresh_rx,
        closed,
        state.clone(),
        conv_id.clone(),
        dirs_group,
        files_group,
    );
    state.trigger_refresh();

    let grant_chosen_path = make_grant_chosen_handler(state, conv_id);
    wire_folder_picker(&add_dir_btn, &dialog, grant_chosen_path.clone());
    wire_file_picker(&add_file_btn, &dialog, grant_chosen_path);
    dialog.present(Some(parent));
}

fn build_folder_permissions_chrome() -> (
    adw::Dialog,
    adw::ToastOverlay,
    gtk::Button,
    gtk::Button,
    adw::PreferencesGroup,
    adw::PreferencesGroup,
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

    let (dirs_group, add_dir_btn) = build_grant_section_group(
        &gettext("Folders"),
        &gettext("Folders this chat can access"),
        &gettext("Add Folder"),
    );
    content_box.append(&dirs_group);

    let (files_group, add_file_btn) = build_grant_section_group(
        &gettext("Files"),
        &gettext("Individual files this chat can access"),
        &gettext("Add File"),
    );
    content_box.append(&files_group);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();
    toolbar.set_content(Some(&scroller));
    toast_overlay.set_child(Some(&toolbar));
    dialog.set_child(Some(&toast_overlay));
    (
        dialog,
        toast_overlay,
        add_dir_btn,
        add_file_btn,
        dirs_group,
        files_group,
    )
}

fn build_grant_section_group(
    title: &str,
    description: &str,
    add_tooltip: &str,
) -> (adw::PreferencesGroup, gtk::Button) {
    let add_btn = build_add_button(add_tooltip);
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .header_suffix(&add_btn)
        .build();
    (group, add_btn)
}

fn spawn_grant_refresh_loop(
    mut refresh_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    closed: Rc<std::cell::Cell<bool>>,
    state: FolderPermissionsState,
    conv_id: String,
    dirs_group: adw::PreferencesGroup,
    files_group: adw::PreferencesGroup,
) {
    let refresh_seq = Rc::new(std::cell::Cell::new(0u64));
    glib::MainContext::default().spawn_local(async move {
        while refresh_rx.recv().await.is_some() {
            if closed.get() {
                break;
            }
            let seq = refresh_seq.get().wrapping_add(1);
            refresh_seq.set(seq);
            run_grant_refresh_pass(
                &state,
                &conv_id,
                &dirs_group,
                &files_group,
                &closed,
                &refresh_seq,
                seq,
            );
        }
    });
}

fn run_grant_refresh_pass(
    state: &FolderPermissionsState,
    conv_id: &str,
    dirs_group: &adw::PreferencesGroup,
    files_group: &adw::PreferencesGroup,
    closed: &Rc<std::cell::Cell<bool>>,
    refresh_seq: &Rc<std::cell::Cell<u64>>,
    seq: u64,
) {
    let (db, cid, state_ui, dirs_ui, files_ui, closed_ui, seq_ref) = (
        state.db.clone(),
        conv_id.to_string(),
        state.clone(),
        dirs_group.clone(),
        files_group.clone(),
        closed.clone(),
        refresh_seq.clone(),
    );
    spawn_blocking_ui(
        &state.rt_handle,
        move || partition_grants(db.list_conversation_folders(&cid).unwrap_or_default()),
        move |(file_entries, dir_entries)| {
            if closed_ui.get() || seq_ref.get() != seq {
                return;
            }
            populate_grant_group(
                &dirs_ui,
                &dir_entries,
                false,
                &gettext("No folders granted"),
                &gettext("Click + to grant access to a folder"),
                &state_ui,
            );
            populate_grant_group(
                &files_ui,
                &file_entries,
                true,
                &gettext("No files granted"),
                &gettext("Click + to grant access to a file"),
                &state_ui,
            );
        },
    );
}

fn make_grant_chosen_handler(
    state: FolderPermissionsState,
    conv_id: String,
) -> Rc<dyn Fn(Result<gio::File, glib::Error>)> {
    Rc::new(move |res: Result<gio::File, glib::Error>| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let db = state.db.clone();
        let cid = conv_id.clone();
        let state_ui = state.clone();
        spawn_blocking_ui(
            &state.rt_handle,
            move || -> Result<Option<String>, String> {
                let existing = db.list_conversation_folders(&cid).unwrap_or_default();
                if let Some(parent_name) = find_covering_granted_folder(&existing, &path) {
                    return Ok(Some(parent_name));
                }
                let path_str = std::fs::canonicalize(&path)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                db.add_conversation_folder(&cid, &path_str)
                    .map(|_| None)
                    .map_err(|e| e.to_string())
            },
            move |res| match res {
                Ok(Some(parent_name)) => state_ui.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Already covered by granted folder \"{name}\"",
                    &[("name", &parent_name)],
                ))),
                Ok(None) => {
                    (state_ui.on_changed)();
                    state_ui.trigger_refresh();
                }
                Err(err) => state_ui.toast_overlay.add_toast(adw::Toast::new(&tr_f(
                    "Failed to grant access: {err}",
                    &[("err", &err)],
                ))),
            },
        );
    })
}

fn wire_folder_picker(
    btn: &gtk::Button,
    dialog: &adw::Dialog,
    cb: Rc<dyn Fn(Result<gio::File, glib::Error>)>,
) {
    let dialog_ref = dialog.downgrade();
    btn.connect_clicked(move |_| {
        let picker = gtk::FileDialog::builder()
            .title(gettext("Add Folder"))
            .modal(true)
            .build();
        let window = dialog_ref
            .upgrade()
            .and_then(|d| d.root())
            .and_then(|r| r.downcast::<gtk::Window>().ok());
        let cb = cb.clone();
        picker.select_folder(window.as_ref(), None::<&gio::Cancellable>, move |res| {
            cb(res)
        });
    });
}

fn wire_file_picker(
    btn: &gtk::Button,
    dialog: &adw::Dialog,
    cb: Rc<dyn Fn(Result<gio::File, glib::Error>)>,
) {
    let dialog_ref = dialog.downgrade();
    btn.connect_clicked(move |_| {
        let picker = gtk::FileDialog::builder()
            .title(gettext("Add File"))
            .modal(true)
            .build();
        let window = dialog_ref
            .upgrade()
            .and_then(|d| d.root())
            .and_then(|r| r.downcast::<gtk::Window>().ok());
        let cb = cb.clone();
        picker.open(window.as_ref(), None::<&gio::Cancellable>, move |res| {
            cb(res)
        });
    });
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
