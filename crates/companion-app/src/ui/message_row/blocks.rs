//! Markdown block diffing and native GTK widget builders for chat message rows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::format::markdown::{MarkdownBlock, format_code_language};
use crate::i18n::gettext;
use crate::ui::message_dialogs::copy_text_with_feedback;
use crate::ui::widgets::clear_box;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum BlockUpdatePlan {
    Skip,
    UpdateLastText,
    UpdateLastCode,
    ReplaceTail(usize),
    RebuildAll,
}

pub(super) fn plan_block_update(
    prev: &[MarkdownBlock],
    prev_is_dark: bool,
    new_blocks: &[MarkdownBlock],
    is_dark: bool,
    is_streaming: bool,
) -> BlockUpdatePlan {
    let theme_matches = prev_is_dark == is_dark;
    if theme_matches && prev == new_blocks {
        return BlockUpdatePlan::Skip;
    }

    // Fast path during streaming: if all prior blocks match (and theme didn't flip), only update/replace the tail.
    let prefix_len = prev.len().saturating_sub(1);
    if theme_matches
        && !prev.is_empty()
        && new_blocks.len() >= prev.len()
        && prev[..prefix_len] == new_blocks[..prefix_len]
    {
        if new_blocks.len() == prev.len()
            && matches!(new_blocks.last(), Some(MarkdownBlock::Text(_)))
        {
            return BlockUpdatePlan::UpdateLastText;
        }
        if is_streaming
            && new_blocks.len() == prev.len()
            && let (
                Some(MarkdownBlock::Code {
                    language: prev_lang,
                    ..
                }),
                Some(MarkdownBlock::Code {
                    language: new_lang, ..
                }),
            ) = (prev.last(), new_blocks.last())
            && prev_lang == new_lang
        {
            return BlockUpdatePlan::UpdateLastCode;
        }
        return BlockUpdatePlan::ReplaceTail(prefix_len);
    }

    BlockUpdatePlan::RebuildAll
}

pub(super) fn update_blocks_box(
    blocks_box: &gtk::Box,
    rendered_cache: &Rc<RefCell<Vec<MarkdownBlock>>>,
    rendered_is_dark: &Rc<Cell<bool>>,
    new_blocks: &[MarkdownBlock],
    is_dark: bool,
    is_streaming: bool,
) {
    let plan = {
        let prev = rendered_cache.borrow();
        plan_block_update(
            &prev,
            rendered_is_dark.get(),
            new_blocks,
            is_dark,
            is_streaming,
        )
    };

    match plan {
        BlockUpdatePlan::Skip => (),
        BlockUpdatePlan::UpdateLastText => {
            if !try_update_last_text(blocks_box, new_blocks) {
                let prefix_len = rendered_cache.borrow().len().saturating_sub(1);
                replace_tail(blocks_box, rendered_cache, new_blocks, prefix_len, is_dark);
                return;
            }
            *rendered_cache.borrow_mut() = new_blocks.to_vec();
        }
        BlockUpdatePlan::UpdateLastCode => {
            if !try_update_last_code(blocks_box, new_blocks, is_dark) {
                let prefix_len = rendered_cache.borrow().len().saturating_sub(1);
                replace_tail(blocks_box, rendered_cache, new_blocks, prefix_len, is_dark);
                return;
            }
            *rendered_cache.borrow_mut() = new_blocks.to_vec();
        }
        BlockUpdatePlan::ReplaceTail(prefix_len) => {
            replace_tail(blocks_box, rendered_cache, new_blocks, prefix_len, is_dark);
        }
        BlockUpdatePlan::RebuildAll => {
            clear_box(blocks_box);
            for block in new_blocks {
                blocks_box.append(&build_block_widget(block, is_dark));
            }
            *rendered_cache.borrow_mut() = new_blocks.to_vec();
            rendered_is_dark.set(is_dark);
        }
    }
}

fn try_update_last_text(blocks_box: &gtk::Box, new_blocks: &[MarkdownBlock]) -> bool {
    if let (Some(MarkdownBlock::Text(new_markup)), Some(last_widget)) =
        (new_blocks.last(), blocks_box.last_child())
        && let Ok(label) = last_widget.downcast::<gtk::Label>()
    {
        apply_markup_to_label(&label, new_markup);
        return true;
    }
    false
}

fn try_update_last_code(
    blocks_box: &gtk::Box,
    new_blocks: &[MarkdownBlock],
    is_dark: bool,
) -> bool {
    if let (
        Some(MarkdownBlock::Code {
            light_pango_markup,
            dark_pango_markup,
            ..
        }),
        Some(last_widget),
    ) = (new_blocks.last(), blocks_box.last_child())
        && let Some(code_box) = last_widget.downcast_ref::<gtk::Box>()
        && let Some(scroller) = code_box.last_child().and_downcast::<gtk::ScrolledWindow>()
        && let Some(code_label) = scroller.child().and_downcast::<gtk::Label>()
    {
        code_label.set_markup(if is_dark {
            dark_pango_markup
        } else {
            light_pango_markup
        });
        return true;
    }
    false
}

fn replace_tail(
    blocks_box: &gtk::Box,
    rendered_cache: &Rc<RefCell<Vec<MarkdownBlock>>>,
    new_blocks: &[MarkdownBlock],
    prefix_len: usize,
    is_dark: bool,
) {
    if let Some(last_widget) = blocks_box.last_child() {
        blocks_box.remove(&last_widget);
    }
    for block in &new_blocks[prefix_len..] {
        blocks_box.append(&build_block_widget(block, is_dark));
    }
    *rendered_cache.borrow_mut() = new_blocks.to_vec();
}

fn build_block_widget(block: &MarkdownBlock, is_dark: bool) -> gtk::Widget {
    match block {
        MarkdownBlock::Text(markup) => build_markup_label(markup).upcast(),
        MarkdownBlock::Quote(markup) => {
            let quote_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .css_classes(["markdown-blockquote"])
                .build();
            let label = build_markup_label(markup);
            label.add_css_class("dim-label");
            quote_box.append(&label);
            quote_box.upcast()
        }
        MarkdownBlock::Rule => gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_top(6)
            .margin_bottom(6)
            .build()
            .upcast(),
        MarkdownBlock::Table { headers, rows } => build_markdown_table(headers, rows).upcast(),
        MarkdownBlock::Code {
            language,
            code,
            light_pango_markup,
            dark_pango_markup,
        } => {
            let markup = if is_dark {
                dark_pango_markup
            } else {
                light_pango_markup
            };
            build_code_block(language.as_deref(), code, markup).upcast()
        }
    }
}

fn apply_markup_to_label(label: &gtk::Label, markup: &str) {
    if let Ok((attrs, text, _)) = gtk::pango::parse_markup(markup, '\0') {
        attrs.insert(gtk::pango::AttrFloat::new_line_height(1.42));
        label.set_text(&text);
        label.set_attributes(Some(&attrs));
    } else {
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrFloat::new_line_height(1.42));
        label.set_text(markup);
        label.set_attributes(Some(&attrs));
    }
}

fn build_markup_label(markup: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .selectable(true)
        .build();
    apply_markup_to_label(&label, markup);
    label
}

fn build_markdown_table(headers: &[String], rows: &[Vec<String>]) -> gtk::ScrolledWindow {
    let grid = gtk::Grid::builder()
        .column_spacing(0)
        .row_spacing(0)
        .build();
    for (col_idx, header) in headers.iter().enumerate() {
        grid.attach(&build_table_cell(header, true), col_idx as i32, 0, 1, 1);
    }
    let row_offset = if headers.is_empty() { 0 } else { 1 };
    for (row_idx, row) in rows.iter().enumerate() {
        for (col_idx, cell_markup) in row.iter().enumerate() {
            grid.attach(
                &build_table_cell(cell_markup, false),
                col_idx as i32,
                (row_idx + row_offset) as i32,
                1,
                1,
            );
        }
    }
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["markdown-table-card"])
        .child(&grid)
        .build()
}

fn build_table_cell(markup: &str, is_header: bool) -> gtk::Box {
    let cell = build_markup_label(markup);
    let v_pad = if is_header { 8 } else { 6 };
    cell.set_margin_top(v_pad);
    cell.set_margin_bottom(v_pad);
    cell.set_margin_start(12);
    cell.set_margin_end(12);
    if is_header {
        cell.add_css_class("heading");
    }
    let wrapper = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes([if is_header {
            "markdown-table-header"
        } else {
            "markdown-table-cell"
        }])
        .build();
    wrapper.append(&cell);
    wrapper
}

fn build_code_block(language: Option<&str>, _raw_code: &str, pango_markup: &str) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["code-card"])
        .build();
    let (header, copy_btn) = build_code_header(language);
    container.append(&header);
    container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let code_label = gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .use_markup(true)
        .margin_top(10)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .css_classes(["monospace"])
        .build();
    code_label.set_markup(pango_markup);
    let label_ref = code_label.clone();
    copy_btn.connect_clicked(move |btn| {
        // Reading `label_ref.text()` reads the latest rendered code even after streaming updates.
        copy_text_with_feedback(btn, &label_ref.text());
    });
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&code_label)
        .build();
    container.append(&scroller);
    container
}

fn build_code_header(language: Option<&str>) -> (gtk::Box, gtk::Button) {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(14)
        .margin_end(8)
        .build();
    let lang_label = gtk::Label::builder()
        .label(format_code_language(language))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["caption-heading", "dim-label"])
        .build();
    let copy_btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(gettext("Copy code"))
        .css_classes(["flat", "circular"])
        .build();
    header.append(&lang_label);
    header.append(&copy_btn);
    (header, copy_btn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_block(lang: &str, code: &str) -> MarkdownBlock {
        MarkdownBlock::Code {
            language: Some(lang.to_string()),
            code: code.to_string(),
            light_pango_markup: code.to_string(),
            dark_pango_markup: code.to_string(),
        }
    }

    #[test]
    fn skips_matching_blocks_and_rebuilds_on_theme_flip() {
        let b = [
            MarkdownBlock::Text("hi".into()),
            code_block("rust", "fn main() {}"),
        ];
        assert_eq!(
            plan_block_update(&b, true, &b, true, false),
            BlockUpdatePlan::Skip
        );
        assert_eq!(
            plan_block_update(&b, true, &b, false, false),
            BlockUpdatePlan::RebuildAll
        );
        assert_eq!(
            plan_block_update(&b, false, &b, true, true),
            BlockUpdatePlan::RebuildAll
        );
    }

    #[test]
    fn fast_paths_streaming_text_and_code_updates() {
        let (pt, nt) = (
            [MarkdownBlock::Text("hel".into())],
            [MarkdownBlock::Text("hello".into())],
        );
        assert_eq!(
            plan_block_update(&pt, false, &nt, false, true),
            BlockUpdatePlan::UpdateLastText
        );
        let (pc, nc) = (
            [code_block("rust", "let x = 1;")],
            [code_block("rust", "let x = 1;\nlet y = 2;")],
        );
        assert_eq!(
            plan_block_update(&pc, true, &nc, true, true),
            BlockUpdatePlan::UpdateLastCode
        );
        assert_eq!(
            plan_block_update(&pc, true, &nc, true, false),
            BlockUpdatePlan::ReplaceTail(0)
        );
    }
}
