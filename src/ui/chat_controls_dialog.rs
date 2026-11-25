//! Unified Chat Controls dialog: Thinking Level, Context Window, Usage, and System Prompt.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;

use crate::core::ThinkingLevel;
use crate::i18n::{gettext, tr_f};
use crate::models::gguf::{context_presets_up_to, format_context_label, format_short_context};
use crate::models::{detect_gguf_max_context, friendly_model_name};
use crate::storage::ModelRecord;
use crate::storage::conversations::default_system_prompt;

/// Present the unified Chat Controls [`adw::Dialog`].
#[allow(clippy::too_many_arguments)]
pub fn present_chat_controls_dialog<F, M>(
    parent: &impl IsA<gtk::Widget>,
    active_model: Option<&ModelRecord>,
    assistant_name: &str,
    current_thinking: ThinkingLevel,
    current_ctx: u32,
    used_tokens: u32,
    current_prompt: &str,
    on_apply: F,
    on_manage_models: M,
) where
    F: Fn(ThinkingLevel, u32, String) + 'static,
    M: Fn() + 'static,
{
    let dialog = adw::Dialog::builder()
        .title(gettext("Chat Controls"))
        .content_width(520)
        .content_height(560)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel_btn = gtk::Button::with_label(&gettext("Cancel"));
    let apply_btn = gtk::Button::with_label(&gettext("Apply"));
    apply_btn.add_css_class("suggested-action");
    apply_btn.set_sensitive(false);
    header.pack_start(&cancel_btn);
    header.pack_end(&apply_btn);
    toolbar.add_top_bar(&header);

    let body = crate::ui::widgets::dialog_content_box();

    let settings_group = adw::PreferencesGroup::new();

    let active_model_name = active_model
        .map(|m| friendly_model_name(&m.display_name))
        .unwrap_or_else(|| gettext("No active AI model"));
    let manage_btn = gtk::Button::builder()
        .label(&gettext("Manage"))
        .tooltip_text(gettext("Manage AI Models"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let model_row = adw::ActionRow::builder()
        .title(gettext("Active AI Model"))
        .subtitle(gtk::glib::markup_escape_text(&active_model_name))
        .build();
    model_row.add_suffix(&manage_btn);

    let thinking_labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let thinking_refs: Vec<&str> = thinking_labels.iter().map(String::as_str).collect();
    let thinking_row = adw::ComboRow::builder()
        .title(gettext("Thinking Level"))
        .subtitle(gettext("Reasoning effort for models that support thinking"))
        .model(&gtk::StringList::new(&thinking_refs))
        .build();
    if let Some(idx) = ThinkingLevel::ALL.iter().position(|t| *t == current_thinking) {
        thinking_row.set_selected(idx as u32);
    }

    let init_max_ctx = active_model
        .and_then(|m| m.file_path.as_deref())
        .and_then(|p| detect_gguf_max_context(Path::new(p)))
        .unwrap_or(131072);

    let ctx_presets: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let ctx_row = adw::ComboRow::builder()
        .title(gettext("Context Window"))
        .build();

    let usage_bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .build();
    usage_bar.set_size_request(110, -1);
    let usage_row = adw::ActionRow::builder()
        .title(gettext("Context Usage"))
        .build();
    usage_row.add_suffix(&usage_bar);

    let update_usage: Rc<dyn Fn(u32)> = {
        let usage_row = usage_row.clone();
        let usage_bar = usage_bar.clone();
        Rc::new(move |max_ctx: u32| {
            let fraction = (used_tokens as f64 / max_ctx.max(1) as f64).clamp(0.0, 1.0);
            let pct = (fraction * 100.0).round() as u32;
            usage_bar.set_fraction(fraction);
            usage_row.set_subtitle(&tr_f(
                "{used} / {max} tokens ({pct}%)",
                &[
                    ("used", &used_tokens.to_string()),
                    ("max", &max_ctx.to_string()),
                    ("pct", &pct.to_string()),
                ],
            ));
        })
    };

    let rebuild_ctx_dropdown: Rc<dyn Fn(u32, u32)> = {
        let ctx_row = ctx_row.clone();
        let ctx_presets = ctx_presets.clone();
        let update_usage = update_usage.clone();
        Rc::new(move |max_ctx: u32, target_ctx: u32| {
            let presets = context_presets_up_to(max_ctx);
            let clamped = target_ctx.min(max_ctx);
            let selected_idx = presets
                .iter()
                .rposition(|&v| v <= clamped)
                .unwrap_or(0);
            let selected_val = presets.get(selected_idx).copied().unwrap_or(clamped);

            let labels: Vec<String> = presets.iter().map(|&v| format_context_label(v)).collect();
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            *ctx_presets.borrow_mut() = presets;
            ctx_row.set_model(Some(&gtk::StringList::new(&label_refs)));
            ctx_row.set_selected(selected_idx as u32);
            ctx_row.set_subtitle(&tr_f(
                "Max supported by model: {max}",
                &[("max", &format_short_context(max_ctx))],
            ));
            update_usage(selected_val);
        })
    };

    rebuild_ctx_dropdown(init_max_ctx, current_ctx);

    let init_thinking_idx = thinking_row.selected();
    let init_ctx_val = ctx_presets
        .borrow()
        .get(ctx_row.selected() as usize)
        .copied()
        .unwrap_or(4096);
    let init_prompt = current_prompt.trim().to_string();

    let text_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .vexpand(true)
        .css_classes(["composer-input"])
        .build();
    text_view.buffer().set_text(current_prompt);

    let sync_apply_sensitivity: Rc<dyn Fn()> = {
        let apply_btn = apply_btn.clone();
        let thinking_row = thinking_row.clone();
        let ctx_row = ctx_row.clone();
        let ctx_presets = ctx_presets.clone();
        let buf = text_view.buffer();
        Rc::new(move || {
            let cur_ctx = ctx_presets
                .borrow()
                .get(ctx_row.selected() as usize)
                .copied()
                .unwrap_or(4096);
            let (s, e) = buf.bounds();
            let cur_prompt = buf.text(&s, &e, false).trim().to_string();
            let dirty = thinking_row.selected() != init_thinking_idx
                || cur_ctx != init_ctx_val
                || cur_prompt != init_prompt;
            apply_btn.set_sensitive(dirty);
        })
    };

    {
        let sync = sync_apply_sensitivity.clone();
        thinking_row.connect_selected_notify(move |_| {
            sync();
        });
    }

    {
        let ctx_presets = ctx_presets.clone();
        let update_usage = update_usage.clone();
        let sync = sync_apply_sensitivity.clone();
        ctx_row.connect_selected_notify(move |row| {
            if let Some(&val) = ctx_presets.borrow().get(row.selected() as usize) {
                update_usage(val);
            }
            sync();
        });
    }

    {
        let sync = sync_apply_sensitivity;
        text_view.buffer().connect_changed(move |_| {
            sync();
        });
    }

    settings_group.add(&model_row);
    settings_group.add(&thinking_row);
    settings_group.add(&ctx_row);
    settings_group.add(&usage_row);

    let notes_group = adw::PreferencesGroup::builder()
        .title(gettext("Chat Notes"))
        .description(tr_f(
            "Add notes, instructions, or context for {name} to remember in this conversation only. Use {assistant_name} and {user_name} to insert your Personalization names.",
            &[("name", assistant_name)],
        ))
        .build();

    let reset_btn = gtk::Button::builder()
        .label(&gettext("Reset to Default"))
        .halign(gtk::Align::Start)
        .build();

    let prompt_frame = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(150)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();
    notes_group.add(&prompt_frame);

    let default_thinking_idx = active_model
        .map(|m| m.default_thinking_level())
        .and_then(|lvl| ThinkingLevel::ALL.iter().position(|t| *t == lvl))
        .unwrap_or(0) as u32;

    {
        let buf = text_view.buffer();
        let thinking_row_reset = thinking_row.clone();
        let rebuild_ctx_reset = rebuild_ctx_dropdown;
        reset_btn.connect_clicked(move |_| {
            buf.set_text(default_system_prompt());
            thinking_row_reset.set_selected(default_thinking_idx);
            rebuild_ctx_reset(init_max_ctx, 4096);
        });
    }

    body.append(&settings_group);
    body.append(&notes_group);
    body.append(&reset_btn);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .build();
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            d.close();
        });
    }
    {
        let d = dialog.clone();
        manage_btn.connect_clicked(move |_| {
            d.close();
            on_manage_models();
        });
    }
    {
        let d = dialog.clone();
        let buf = text_view.buffer();
        apply_btn.connect_clicked(move |_| {
            let selected_thinking = ThinkingLevel::from_index(thinking_row.selected());
            let selected_ctx = ctx_presets
                .borrow()
                .get(ctx_row.selected() as usize)
                .copied()
                .unwrap_or(4096);
            let (start, end) = buf.bounds();
            let prompt_text = buf.text(&start, &end, false).trim().to_string();
            let final_prompt = if prompt_text.is_empty() {
                default_system_prompt().to_string()
            } else {
                prompt_text
            };
            on_apply(selected_thinking, selected_ctx, final_prompt);
            d.close();
        });
    }

    dialog.present(Some(parent));
}
