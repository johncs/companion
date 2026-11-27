//! Hugging Face repository expander rows and individual GGUF file rows.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{gettext, tr_f};
use crate::models::hardware::assess_model_fit;
use crate::models::huggingface::{HfGgufFile, HfRepoSummary};
use crate::models::{format_bytes, infer_quantization};

use super::HfBrowserContext;
use super::download_controls::DownloadControls;
use super::download_tracker::{RowDownloadUi, start_hf_download};

pub(super) fn build_hf_repo_row(repo: &HfRepoSummary, ctx: &HfBrowserContext) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder()
        .title(crate::format::markdown::escape_pango_markup(&repo.id))
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
    let (repo_id, ctx) = (repo.id.clone(), ctx.clone());
    expander.connect_expanded_notify(move |exp| {
        if !exp.is_expanded() || *loaded.borrow() {
            return;
        }
        *loaded.borrow_mut() = true;
        placeholder.set_title(&gettext("Loading available files…"));
        fetch_and_populate_repo_files(exp, &placeholder, &repo_id, &ctx, &loaded);
    });
    expander
}

fn fetch_and_populate_repo_files(
    exp: &adw::ExpanderRow,
    placeholder: &adw::ActionRow,
    repo_id: &str,
    ctx: &HfBrowserContext,
    loaded: &Rc<RefCell<bool>>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (rid, hf_client) = (repo_id.to_string(), ctx.services.hf_client.clone());
    ctx.services.rt_handle.spawn(async move {
        let _ = tx.send(hf_client.list_repo_gguf_files(&rid).await);
    });
    let (exp, placeholder, rid, ctx, loaded_flag) = (
        exp.clone(),
        placeholder.clone(),
        repo_id.to_string(),
        ctx.clone(),
        loaded.clone(),
    );
    glib::MainContext::default().spawn_local(async move {
        match rx.await {
            Ok(Ok(files)) if files.is_empty() => {
                placeholder.set_title(&gettext("No .gguf files in repository"))
            }
            Ok(Ok(files)) => {
                exp.remove(&placeholder);
                populate_repo_files(&exp, &rid, files, &ctx);
            }
            Ok(Err(err)) => {
                *loaded_flag.borrow_mut() = false;
                placeholder.set_title(&crate::format::markdown::escape_pango_markup(&tr_f(
                    "Failed to load files: {err}",
                    &[("err", &err.to_string())],
                )));
                attach_repo_retry_button(&exp, &placeholder, &rid, &ctx, &loaded_flag);
            }
            Err(_) => *loaded_flag.borrow_mut() = false,
        }
    });
}

fn attach_repo_retry_button(
    exp: &adw::ExpanderRow,
    placeholder: &adw::ActionRow,
    repo_id: &str,
    ctx: &HfBrowserContext,
    loaded: &Rc<RefCell<bool>>,
) {
    let retry_btn = gtk::Button::builder()
        .label(gettext("Retry"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let (exp, ph, rid, ctx, loaded) = (
        exp.clone(),
        placeholder.clone(),
        repo_id.to_string(),
        ctx.clone(),
        loaded.clone(),
    );
    let btn_ref = retry_btn.clone();
    retry_btn.connect_clicked(move |_| {
        ph.remove(&btn_ref);
        ph.set_title(&gettext("Loading files…"));
        *loaded.borrow_mut() = true;
        fetch_and_populate_repo_files(&exp, &ph, &rid, &ctx, &loaded);
    });
    placeholder.add_suffix(&retry_btn);
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
    let subtitle = build_hf_file_subtitle(file, mmproj_file, ctx.hw.total_ram_bytes);
    let row = adw::ActionRow::builder()
        .title(crate::format::markdown::escape_pango_markup(&file.filename))
        .subtitle(crate::format::markdown::escape_pango_markup(&subtitle))
        .build();

    let controls = DownloadControls::new();
    let download_btn = gtk::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text(gettext("Download AI model"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let (rid, fname, mp_name, ctx, ctrl_c) = (
        repo_id.to_string(),
        file.filename.clone(),
        mmproj_file.map(|m| m.filename.clone()),
        ctx.clone(),
        controls.clone(),
    );
    download_btn.connect_clicked(move |btn| {
        start_hf_download(
            &ctx,
            &rid,
            &fname,
            mp_name.as_deref(),
            RowDownloadUi {
                btn: btn.clone(),
                controls: ctrl_c.clone(),
            },
        );
    });
    row.add_suffix(&controls.container);
    row.add_suffix(&download_btn);
    row
}

fn build_hf_file_subtitle(
    file: &HfGgufFile,
    mmproj_file: Option<&HfGgufFile>,
    total_ram_bytes: u64,
) -> String {
    let quant = infer_quantization(&file.filename).unwrap_or_else(|| "GGUF".to_string());
    let fit = assess_model_fit(file.size_bytes, total_ram_bytes);
    let mut parts = vec![format_bytes(file.size_bytes), quant, fit.label()];
    if mmproj_file.is_some() {
        parts.push(gettext("+ Vision"));
    }
    parts.join(" · ")
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
