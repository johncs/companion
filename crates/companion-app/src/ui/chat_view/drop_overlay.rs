//! Drag-and-drop overlay and folder-drop grant background task for [`ChatViewModel`].

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio};
use relm4::prelude::*;

use crate::core::types::FolderPermission;
use crate::i18n::tr_f;
use crate::ui::composer::ComposerMsg;
use crate::ui::widgets::spawn_blocking_ui;

use super::{ChatViewModel, ChatViewMsg, ChatViewOutput};

pub(super) fn grant_folder_drop_in_bg(
    db: &crate::storage::Database,
    rt_handle: &tokio::runtime::Handle,
    conv_id: String,
    folder_str: String,
    folder_name: String,
    allow_write: bool,
    sender: ComponentSender<ChatViewModel>,
) {
    let db = db.clone();
    spawn_blocking_ui(
        rt_handle,
        move || -> Result<(), String> {
            let entry = db
                .add_conversation_folder(&conv_id, &folder_str)
                .map_err(|e| e.to_string())?;
            if allow_write {
                db.set_conversation_folder_permission(&entry.id, FolderPermission::ReadWrite)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        },
        move |res| match res {
            Ok(()) => {
                sender.input(ChatViewMsg::RefreshGrantedFolders);
                let msg = if allow_write {
                    tr_f(
                        "Granted read & write access to {folder}",
                        &[("folder", &folder_name)],
                    )
                } else {
                    tr_f(
                        "Granted read-only access to {folder}",
                        &[("folder", &folder_name)],
                    )
                };
                let _ = sender.output(ChatViewOutput::ShowToast(msg));
            }
            Err(err) => {
                let _ = sender.output(ChatViewOutput::ShowToast(tr_f(
                    "Failed to grant folder: {err}",
                    &[("err", &err)],
                )));
            }
        },
    );
}

pub(super) fn attach_drop_overlay(
    root_overlay: &gtk::Overlay,
    drop_overlay: &gtk::Box,
    composer_sender: &relm4::Sender<ComposerMsg>,
) {
    let drop_target = gtk::DropTarget::builder()
        .actions(gdk::DragAction::COPY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

    let overlay_enter = drop_overlay.clone();
    drop_target.connect_enter(move |_, _, _| {
        overlay_enter.set_visible(true);
        gdk::DragAction::COPY
    });

    let overlay_leave = drop_overlay.clone();
    drop_target.connect_leave(move |_| {
        overlay_leave.set_visible(false);
    });

    let (overlay_drop, composer_sender) = (drop_overlay.clone(), composer_sender.clone());
    drop_target.connect_drop(move |_, value, _, _| {
        overlay_drop.set_visible(false);
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(file_list) = value.get::<gdk::FileList>() {
            paths.extend(file_list.files().into_iter().filter_map(|f| f.path()));
        } else if let Ok(file) = value.get::<gio::File>()
            && let Some(p) = file.path()
        {
            paths.push(p);
        }
        if !paths.is_empty() {
            let _ = composer_sender.send(ComposerMsg::FilesDropped(paths));
            true
        } else {
            false
        }
    });
    root_overlay.add_controller(drop_target);
}
