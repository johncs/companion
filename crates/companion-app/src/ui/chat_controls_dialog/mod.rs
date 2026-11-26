//! Unified Chat Controls dialog: Thinking Level, Context Window, Usage, and System Prompt.

mod form_state;

use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::markdown::escape_pango_markup;
use crate::core::prompt::default_system_prompt;
use crate::i18n::{gettext, tr_f};
use crate::models::gguf::{context_presets_up_to, format_context_label, format_short_context};
use crate::models::{detect_gguf_max_context, friendly_model_name};
use crate::storage::ModelRecord;

use form_state::ChatControlsFormState;

/// Configuration passed to [`present_chat_controls_dialog`].
pub struct ChatControlsConfig<'a, F, M> {
    pub active_model: Option<&'a ModelRecord>,
    pub assistant_name: &'a str,
    pub current_thinking: ThinkingLevel,
    pub current_ctx: u32,
    pub used_tokens: u32,
    pub current_prompt: &'a str,
    pub on_apply: F,
    pub on_manage_models: M,
}

struct SettingsSectionWidgets {
    group: adw::PreferencesGroup,
    manage_btn: gtk::Button,
    thinking_row: adw::ComboRow,
    ctx_row: adw::ComboRow,
    form: Rc<ChatControlsFormState>,
    rebuild_ctx: Rc<dyn Fn(u32, u32)>,
    update_usage: Rc<dyn Fn(u32)>,
}

/// Present the unified Chat Controls [`adw::Dialog`].
pub fn present_chat_controls_dialog<F, M>(
    parent: &impl IsA<gtk::Widget>,
    cfg: ChatControlsConfig<'_, F, M>,
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

    let section = build_settings_section(&cfg);
    let (notes_group, text_view) = build_notes_section(cfg.assistant_name, cfg.current_prompt);
    let reset_btn = gtk::Button::builder()
        .label(gettext("Reset to Default"))
        .halign(gtk::Align::Start)
        .build();

    wire_dirty_and_reset(&apply_btn, &reset_btn, &section, &text_view);

    let body = crate::ui::widgets::dialog_content_box();
    body.append(&section.group);
    body.append(&notes_group);
    body.append(&reset_btn);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .build();
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    let d_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| {
        d_cancel.close();
    });

    let d_manage = dialog.clone();
    let on_manage = cfg.on_manage_models;
    section.manage_btn.connect_clicked(move |_| {
        d_manage.close();
        on_manage();
    });

    let d_apply = dialog.clone();
    let buf = text_view.buffer();
    let on_apply = cfg.on_apply;
    let form = section.form.clone();
    let thinking_row = section.thinking_row.clone();
    let ctx_row = section.ctx_row.clone();
    apply_btn.connect_clicked(move |_| {
        let selected_thinking = ThinkingLevel::from_index(thinking_row.selected());
        let selected_ctx = form.selected_ctx(ctx_row.selected());
        let (start, end) = buf.bounds();
        let prompt_text = buf.text(&start, &end, false).trim().to_string();
        let final_prompt = if prompt_text.is_empty() {
            default_system_prompt().to_string()
        } else {
            prompt_text
        };
        on_apply(selected_thinking, selected_ctx, final_prompt);
        d_apply.close();
    });

    dialog.present(Some(parent));
}

fn build_settings_section<F, M>(cfg: &ChatControlsConfig<'_, F, M>) -> SettingsSectionWidgets {
    let settings_group = adw::PreferencesGroup::new();
    let active_model_name = cfg
        .active_model
        .map(|m| friendly_model_name(&m.display_name))
        .unwrap_or_else(|| gettext("No active AI model"));
    let manage_btn = gtk::Button::builder()
        .label(gettext("Manage"))
        .tooltip_text(gettext("Manage AI Models"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let model_row = adw::ActionRow::builder()
        .title(gettext("Active AI Model"))
        .subtitle(escape_pango_markup(&active_model_name))
        .build();
    model_row.add_suffix(&manage_btn);

    let thinking_labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let thinking_refs: Vec<&str> = thinking_labels.iter().map(String::as_str).collect();
    let thinking_row = adw::ComboRow::builder()
        .title(gettext("Thinking Level"))
        .subtitle(gettext("Reasoning effort for models that support thinking"))
        .model(&gtk::StringList::new(&thinking_refs))
        .build();
    let init_thinking_idx = ThinkingLevel::ALL
        .iter()
        .position(|t| *t == cfg.current_thinking)
        .unwrap_or(0) as u32;
    thinking_row.set_selected(init_thinking_idx);

    let init_max_ctx = cfg
        .active_model
        .and_then(|m| m.file_path.as_deref())
        .and_then(|p| detect_gguf_max_context(Path::new(p)))
        .unwrap_or(131072);
    let default_thinking_idx = cfg
        .active_model
        .map(|m| m.default_thinking_level())
        .and_then(|lvl| ThinkingLevel::ALL.iter().position(|t| *t == lvl))
        .unwrap_or(0) as u32;
    let init_presets = context_presets_up_to(init_max_ctx);
    let init_ctx_val = init_presets
        .iter()
        .rfind(|&&v| v <= cfg.current_ctx.min(init_max_ctx))
        .copied()
        .unwrap_or(cfg.current_ctx.min(init_max_ctx));

    let form = Rc::new(ChatControlsFormState::new(
        init_thinking_idx,
        init_ctx_val,
        cfg.current_prompt,
        init_max_ctx,
        default_thinking_idx,
        init_presets,
    ));

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

    let used_tokens = cfg.used_tokens;
    let update_usage: Rc<dyn Fn(u32)> = {
        let usage_row = usage_row.clone();
        let usage_bar = usage_bar.clone();
        Rc::new(move |max_ctx: u32| {
            let (fraction, label) = ChatControlsFormState::format_usage(used_tokens, max_ctx);
            usage_bar.set_fraction(fraction);
            usage_row.set_subtitle(&label);
        })
    };

    let rebuild_ctx: Rc<dyn Fn(u32, u32)> = {
        let ctx_row = ctx_row.clone();
        let form = form.clone();
        let update_usage = update_usage.clone();
        Rc::new(move |max_ctx: u32, target_ctx: u32| {
            let presets = context_presets_up_to(max_ctx);
            let clamped = target_ctx.min(max_ctx);
            let selected_idx = presets.iter().rposition(|&v| v <= clamped).unwrap_or(0);
            let selected_val = presets.get(selected_idx).copied().unwrap_or(clamped);
            let labels: Vec<String> = presets.iter().map(|&v| format_context_label(v)).collect();
            let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            *form.ctx_presets.borrow_mut() = presets;
            ctx_row.set_model(Some(&gtk::StringList::new(&label_refs)));
            ctx_row.set_selected(selected_idx as u32);
            ctx_row.set_subtitle(&tr_f(
                "Max supported by model: {max}",
                &[("max", &format_short_context(max_ctx))],
            ));
            update_usage(selected_val);
        })
    };
    rebuild_ctx(init_max_ctx, cfg.current_ctx);

    settings_group.add(&model_row);
    settings_group.add(&thinking_row);
    settings_group.add(&ctx_row);
    settings_group.add(&usage_row);

    SettingsSectionWidgets {
        group: settings_group,
        manage_btn,
        thinking_row,
        ctx_row,
        form,
        rebuild_ctx,
        update_usage,
    }
}

fn build_notes_section(
    assistant_name: &str,
    current_prompt: &str,
) -> (adw::PreferencesGroup, gtk::TextView) {
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

    let notes_group = adw::PreferencesGroup::builder()
        .title(gettext("Chat Notes"))
        .description(tr_f(
            "Add notes, instructions, or context for {name} to remember in this conversation only. Use {assistant_name} and {user_name} to insert your Personalization names.",
            &[("name", assistant_name)],
        ))
        .build();
    let prompt_frame = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(150)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();
    notes_group.add(&prompt_frame);
    (notes_group, text_view)
}

fn wire_dirty_and_reset(
    apply_btn: &gtk::Button,
    reset_btn: &gtk::Button,
    section: &SettingsSectionWidgets,
    text_view: &gtk::TextView,
) {
    let sync_apply: Rc<dyn Fn()> = {
        let (apply_btn, thinking_row, ctx_row, form, buf) = (
            apply_btn.clone(),
            section.thinking_row.clone(),
            section.ctx_row.clone(),
            section.form.clone(),
            text_view.buffer(),
        );
        Rc::new(move || {
            let (s, e) = buf.bounds();
            let cur_prompt = buf.text(&s, &e, false);
            apply_btn.set_sensitive(form.is_dirty(
                thinking_row.selected(),
                ctx_row.selected(),
                &cur_prompt,
            ));
        })
    };

    let s1 = sync_apply.clone();
    section.thinking_row.connect_selected_notify(move |_| s1());

    let (form, uu, s2) = (
        section.form.clone(),
        section.update_usage.clone(),
        sync_apply.clone(),
    );
    section.ctx_row.connect_selected_notify(move |row| {
        uu(form.selected_ctx(row.selected()));
        s2();
    });

    text_view.buffer().connect_changed(move |_| sync_apply());

    let buf = text_view.buffer();
    let tr = section.thinking_row.clone();
    let rc = section.rebuild_ctx.clone();
    let init_max_ctx = section.form.init_max_ctx;
    let default_thinking_idx = section.form.default_thinking_idx;
    reset_btn.connect_clicked(move |_| {
        buf.set_text(default_system_prompt());
        tr.set_selected(default_thinking_idx);
        rc(init_max_ctx, 4096);
    });
}
