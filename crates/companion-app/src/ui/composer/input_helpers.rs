//! Pure input/clipboard helpers and unit-tested utilities for the chat composer.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::prelude::*;

use crate::i18n::tr_f;
use crate::storage::ModelRecord;
use crate::ui::widgets::spawn_blocking_ui;

use super::{ComposerModel, ComposerMsg, ComposerOutput};
use crate::ui::mention_picker::MentionPicker;

/// Parse a `text/uri-list` clipboard payload into existing local file paths.
pub(super) fn parse_uri_list_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let p = if l.starts_with("file://") {
                gio::File::for_uri(l).path()
            } else {
                Some(PathBuf::from(l))
            };
            p.filter(|path| path.is_file())
        })
        .collect()
}

/// Extract `n_ctx` from the active model's typed `default_params`, defaulting to `4096`.
pub(super) fn extract_model_n_ctx(models: &[ModelRecord], active_model_id: Option<&str>) -> u32 {
    active_model_id
        .and_then(|id| models.iter().find(|m| m.id == id))
        .map(|m| m.default_params.n_ctx)
        .unwrap_or(4096)
}

/// Compute clamped context usage percentage (`0..=100`).
pub(super) fn compute_context_usage_pct(used_tokens: u32, max_ctx: u32) -> u32 {
    ((used_tokens as f64 / max_ctx.max(1) as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u32
}

/// Attach Enter-to-send and mention-picker key handling to the composer `TextView`.
pub(super) fn attach_key_controller(
    text_view: &gtk::TextView,
    mention_picker: &MentionPicker,
    sender: &ComponentSender<ComposerModel>,
) {
    let key_ctrl = gtk::EventControllerKey::new();
    let sender_clone = sender.clone();
    let picker_key = mention_picker.clone();
    key_ctrl.connect_key_pressed(move |_, key, _, mods| {
        if let Some(prop) = picker_key.handle_key(key) {
            return prop;
        }
        if (key == gdk::Key::Return || key == gdk::Key::KP_Enter)
            && !mods.contains(gdk::ModifierType::SHIFT_MASK)
        {
            sender_clone.input(ComposerMsg::SendPressed);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    text_view.add_controller(key_ctrl);
}

/// Remove orphaned clipboard temp images left behind by previous runs.
pub fn cleanup_orphaned_clipboard_files(clipboard_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(clipboard_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_pasted_png = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("pasted-") && n.ends_with(".png"));
        if is_pasted_png && path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Handle image/file paste into the composer, or trim trailing blank lines on text paste.
pub(super) fn attach_clipboard_paste_handler(
    text_view: &gtk::TextView,
    clipboard_dir: PathBuf,
    rt_handle: tokio::runtime::Handle,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_paste = sender.clone();
    text_view.connect_paste_clipboard(move |tv| {
        let clipboard = tv.clipboard();
        let formats = clipboard.formats();

        if formats.contains_type(gdk::Texture::static_type()) {
            tv.stop_signal_emission_by_name("paste-clipboard");
            paste_texture_from_clipboard(&clipboard, &clipboard_dir, &rt_handle, &sender_paste);
            return;
        }
        if formats.contains_type(gdk::FileList::static_type()) {
            tv.stop_signal_emission_by_name("paste-clipboard");
            paste_file_list_from_clipboard(&clipboard, &sender_paste);
            return;
        }
        if formats.contains_type(gio::File::static_type()) {
            tv.stop_signal_emission_by_name("paste-clipboard");
            paste_single_file_from_clipboard(&clipboard, &sender_paste);
            return;
        }
        if formats.contain_mime_type("text/uri-list") {
            tv.stop_signal_emission_by_name("paste-clipboard");
            paste_uri_list_from_clipboard(tv, &clipboard, &sender_paste);
            return;
        }

        let tv_weak = tv.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(tv) = tv_weak.upgrade() {
                let buf = tv.buffer();
                let (start, end) = buf.bounds();
                let text = buf.text(&start, &end, false);
                let trimmed = text.trim_end_matches(['\n', '\r']);
                if trimmed.len() < text.len() {
                    buf.set_text(trimmed);
                }
                tv.queue_resize();
            }
        });
    });
}

fn paste_texture_from_clipboard(
    clipboard: &gdk::Clipboard,
    clipboard_dir: &std::path::Path,
    rt_handle: &tokio::runtime::Handle,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_clip = sender.clone();
    let dest_dir = clipboard_dir.to_path_buf();
    let rt_handle = rt_handle.clone();
    clipboard.read_texture_async(None::<&gio::Cancellable>, move |res| match res {
        Ok(Some(texture)) => {
            let png_bytes = texture.save_to_png_bytes();
            let dest_path = dest_dir.join(format!("pasted-{}.png", uuid::Uuid::new_v4()));
            let dest_dir = dest_dir.clone();
            let sender_ui = sender_clip.clone();
            spawn_blocking_ui(
                &rt_handle,
                move || {
                    std::fs::create_dir_all(&dest_dir)
                        .and_then(|_| std::fs::write(&dest_path, png_bytes.as_ref()))
                        .map(|_| dest_path)
                        .map_err(|e| e.to_string())
                },
                move |io_res| match io_res {
                    Ok(path) => sender_ui.input(ComposerMsg::FilesDropped(vec![path])),
                    Err(err) => {
                        let _ = sender_ui.output(ComposerOutput::ShowToast(tr_f(
                            "Failed to save pasted image: {err}",
                            &[("err", &err)],
                        )));
                    }
                },
            );
        }
        Ok(None) => {}
        Err(err) => {
            let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                "Failed to read image from clipboard: {err}",
                &[("err", &err.to_string())],
            )));
        }
    });
}

fn paste_file_list_from_clipboard(
    clipboard: &gdk::Clipboard,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_clip = sender.clone();
    clipboard.read_value_async(
        gdk::FileList::static_type(),
        glib::Priority::DEFAULT,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(val) => {
                if let Ok(file_list) = val.get::<gdk::FileList>() {
                    let paths: Vec<PathBuf> = file_list
                        .files()
                        .into_iter()
                        .filter_map(|f| f.path())
                        .collect();
                    if !paths.is_empty() {
                        sender_clip.input(ComposerMsg::FilesDropped(paths));
                    }
                }
            }
            Err(err) => {
                let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                    "Failed to read file from clipboard: {err}",
                    &[("err", &err.to_string())],
                )));
            }
        },
    );
}

fn paste_single_file_from_clipboard(
    clipboard: &gdk::Clipboard,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_clip = sender.clone();
    clipboard.read_value_async(
        gio::File::static_type(),
        glib::Priority::DEFAULT,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(val) => {
                if let Ok(file) = val.get::<gio::File>()
                    && let Some(path) = file.path()
                {
                    sender_clip.input(ComposerMsg::FilesDropped(vec![path]));
                }
            }
            Err(err) => {
                let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                    "Failed to read file from clipboard: {err}",
                    &[("err", &err.to_string())],
                )));
            }
        },
    );
}

fn paste_uri_list_from_clipboard(
    tv: &gtk::TextView,
    clipboard: &gdk::Clipboard,
    sender: &ComponentSender<ComposerModel>,
) {
    let sender_clip = sender.clone();
    let tv_weak = tv.downgrade();
    clipboard.read_text_async(None::<&gio::Cancellable>, move |res| match res {
        Ok(Some(text)) => {
            let paths = parse_uri_list_paths(&text);
            if !paths.is_empty() {
                sender_clip.input(ComposerMsg::FilesDropped(paths));
            } else if let Some(tv) = tv_weak.upgrade() {
                tv.buffer().insert_at_cursor(&text);
            }
        }
        Ok(None) => {}
        Err(err) => {
            let _ = sender_clip.output(ComposerOutput::ShowToast(tr_f(
                "Failed to read clipboard: {err}",
                &[("err", &err.to_string())],
            )));
        }
    });
}

/// Dynamically scale the composer's scrolled input max height to 45% of the window height.
pub(super) fn attach_dynamic_height_scaler(text_view: &gtk::TextView) {
    if let Some(scroller) = text_view
        .parent()
        .and_then(|p| p.downcast::<gtk::ScrolledWindow>().ok())
    {
        // Track one-time handler attachment so remapping never stacks duplicate `gtk::Window` signals.
        let attached = std::cell::Cell::new(false);
        scroller.connect_map(move |s| {
            if attached.get() {
                if let Some(win) = s.root().and_downcast::<gtk::Window>() {
                    let h = win.height();
                    if h > 0 {
                        s.set_max_content_height(((h as f64) * 0.45).round().max(160.0) as i32);
                    }
                }
                return;
            }
            if let Some(win) = s.root().and_downcast::<gtk::Window>() {
                attached.set(true);
                let update_max_height = {
                    let s = s.clone();
                    let win = win.clone();
                    move || {
                        let h = win.height();
                        if h > 0 {
                            let cap = ((h as f64) * 0.45).round().max(160.0) as i32;
                            s.set_max_content_height(cap);
                        }
                    }
                };
                update_max_height();
                let u1 = update_max_height.clone();
                win.connect_default_height_notify(move |_| u1());
                let u2 = update_max_height;
                win.connect_maximized_notify(move |_| u2());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_usage_pct_clamps_and_avoids_div_zero() {
        assert_eq!(compute_context_usage_pct(0, 0), 0);
        assert_eq!(compute_context_usage_pct(2048, 4096), 50);
        assert_eq!(compute_context_usage_pct(9000, 4096), 100);
    }

    #[test]
    fn parse_uri_list_filters_comments_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("companion_uri_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.txt");
        std::fs::write(&file, "hi").unwrap();

        let input = format!("# comment\n{}\n/nonexistent/path.txt\n", file.display());
        let parsed = parse_uri_list_paths(&input);
        assert_eq!(parsed, vec![file]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_model_n_ctx_reads_typed_params_and_falls_back() {
        use crate::core::model_params::ModelParams;

        let models = vec![ModelRecord {
            id: "m1".into(),
            display_name: "M1".into(),
            provider_type: crate::core::types::ModelProviderType::LlamaCpp,
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params: ModelParams {
                n_ctx: 16384,
                ..ModelParams::default()
            },
            avg_tokens_per_sec: None,
        }];

        assert_eq!(extract_model_n_ctx(&models, Some("m1")), 16384);
        assert_eq!(extract_model_n_ctx(&models, None), 4096);
    }

    #[test]
    fn cleanup_orphaned_clipboard_files_removes_only_pasted_pngs() {
        let dir = std::env::temp_dir().join(format!("companion_clip_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pasted = dir.join("pasted-123.png");
        let keep = dir.join("notes.txt");
        std::fs::write(&pasted, b"png").unwrap();
        std::fs::write(&keep, b"keep").unwrap();

        cleanup_orphaned_clipboard_files(&dir);

        assert!(!pasted.exists());
        assert!(keep.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
