//! Hugging Face repository expander rows and individual GGUF file rows.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::runtime;
use crate::i18n::{gettext, tr_f};
use crate::models::hardware::assess_model_fit;
use crate::models::huggingface::{HfGgufFile, HfRepoSummary, list_repo_gguf_files};
use crate::models::{format_bytes, infer_quantization};

use super::HfBrowserContext;
use super::download_controls::DownloadControls;
use super::download_tracker::{RowDownloadUi, start_hf_download};

pub(super) fn build_hf_repo_row(
    repo: &HfRepoSummary,
    ctx: &HfBrowserContext,
) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder()
        .title(&repo.id)
        .subtitle(tr_f(
            "{downloads} downloads · {likes} likes",
            &[
                ("downloads", &format_compact_count(repo.downloads)),
                ("likes", &format_compact_count(repo.likes)),
            ],
        ))
        .build();

    let placeholder = adw::ActionRow::builder()
        .title(gettext("Expand to view downloadable files…"))
        .sensitive(false)
        .build();
    expander.add_row(&placeholder);

    let loaded = Rc::new(RefCell::new(false));
    let repo_id = repo.id.clone();
    let ctx = ctx.clone();

    expander.connect_expanded_notify(move |exp| {
        if !exp.is_expanded() || *loaded.borrow() {
            return;
        }
        *loaded.borrow_mut() = true;
        placeholder.set_title(&gettext("Loading available files…"));

        let (tx, rx) = tokio::sync::oneshot::channel();
        let rid = repo_id.clone();
        runtime().spawn(async move {
            let _ = tx.send(list_repo_gguf_files(&rid).await);
        });

        let exp = exp.clone();
        let placeholder = placeholder.clone();
        let rid = repo_id.clone();
        let ctx = ctx.clone();
        let loaded_flag = loaded.clone();

        glib::MainContext::default().spawn_local(async move {
            match rx.await {
                Ok(Ok(files)) if files.is_empty() => {
                    placeholder.set_title(&gettext("No .gguf files in repository"));
                }
                Ok(Ok(files)) => {
                    exp.remove(&placeholder);
                    populate_repo_files(&exp, &rid, files, &ctx);
                }
                Ok(Err(err)) => {
                    *loaded_flag.borrow_mut() = false;
                    placeholder.set_title(&tr_f(
                        "Failed to load files (re-expand to retry): {err}",
                        &[("err", &err.to_string())],
                    ));
                }
                Err(_) => {
                    *loaded_flag.borrow_mut() = false;
                }
            }
        });
    });

    expander
}

fn populate_repo_files(
    exp: &adw::ExpanderRow,
    repo_id: &str,
    files: Vec<HfGgufFile>,
    ctx: &HfBrowserContext,
) {
    let mut mmproj_file: Option<HfGgufFile> = None;
    let mut model_files: Vec<HfGgufFile> = Vec::with_capacity(files.len());
    for file in files {
        if file.filename.to_ascii_lowercase().contains("mmproj") {
            if mmproj_file.is_none() {
                mmproj_file = Some(file);
            }
        } else {
            model_files.push(file);
        }
    }

    if model_files.is_empty() {
        if let Some(mm) = mmproj_file {
            exp.add_row(&build_hf_file_row(repo_id, &mm, None, ctx));
        }
        return;
    }

    for file in model_files {
        exp.add_row(&build_hf_file_row(
            repo_id,
            &file,
            mmproj_file.as_ref(),
            ctx,
        ));
    }
}

fn build_hf_file_row(
    repo_id: &str,
    file: &HfGgufFile,
    mmproj_file: Option<&HfGgufFile>,
    ctx: &HfBrowserContext,
) -> adw::ActionRow {
    let quant = infer_quantization(&file.filename).unwrap_or_else(|| "GGUF".to_string());
    let fit = assess_model_fit(file.size_bytes, ctx.hw.total_ram_bytes);

    let mut subtitle_parts = vec![format_bytes(file.size_bytes), quant, fit.label()];
    if mmproj_file.is_some() {
        subtitle_parts.push(gettext("+ Vision"));
    }

    let row = adw::ActionRow::builder()
        .title(&file.filename)
        .subtitle(subtitle_parts.join(" · "))
        .build();

    let controls = DownloadControls::new();
    let download_btn = gtk::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text(gettext("Download AI model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();

    let on_pause_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_cancel_handler: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let on_pause = on_pause_handler.clone();
        controls.pause_btn.connect_clicked(move |_| {
            if let Some(cb) = on_pause.borrow().clone() {
                cb();
            }
        });
    }
    {
        let on_cancel = on_cancel_handler.clone();
        controls.cancel_btn.connect_clicked(move |_| {
            if let Some(cb) = on_cancel.borrow().clone() {
                cb();
            }
        });
    }

    {
        let repo_id = repo_id.to_string();
        let filename = file.filename.clone();
        let mmproj_name = mmproj_file.map(|m| m.filename.clone());
        let ctx = ctx.clone();
        let controls = controls.clone();
        let on_pause = on_pause_handler.clone();
        let on_cancel = on_cancel_handler.clone();

        download_btn.connect_clicked(move |btn| {
            start_hf_download(
                &ctx,
                &repo_id,
                &filename,
                mmproj_name.as_deref(),
                RowDownloadUi {
                    btn: btn.clone(),
                    controls: controls.clone(),
                    on_pause: on_pause.clone(),
                    on_cancel: on_cancel.clone(),
                },
            );
        });
    }

    row.add_suffix(&controls.container);
    row.add_suffix(&download_btn);
    row
}

fn format_compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m < 10.0 {
            let s = format!("{m:.1}");
            format!("{}M", s.strip_suffix(".0").unwrap_or(&s))
        } else {
            format!("{}M", m.round() as u64)
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            let s = format!("{k:.1}");
            format!("{}K", s.strip_suffix(".0").unwrap_or(&s))
        } else {
            format!("{}K", k.round() as u64)
        }
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_compact_download_and_like_counts() {
        assert_eq!(format_compact_count(842), "842");
        assert_eq!(format_compact_count(1_000), "1K");
        assert_eq!(format_compact_count(1_500), "1.5K");
        assert_eq!(format_compact_count(12_400), "12K");
        assert_eq!(format_compact_count(5_000_000), "5M");
        assert_eq!(format_compact_count(1_200_000), "1.2M");
    }
}
