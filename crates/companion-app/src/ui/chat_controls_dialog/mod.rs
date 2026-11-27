//! Unified Chat Controls dialog: Thinking Level, Context Window, Usage, and System Prompt.

mod form_state;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::prompt::default_system_prompt;
use crate::format::markdown::escape_pango_markup;
use crate::i18n::{gettext, tr_f};
use crate::models::friendly_model_name;
use crate::models::gguf::{
    GgufMetadataCache, context_presets_up_to, format_context_label, format_short_context,
};
use crate::storage::ModelRecord;
use crate::ui::widgets::spawn_blocking_ui;

use form_state::ChatControlsFormState;

/// Configuration passed to [`present_chat_controls_dialog`].
pub struct ChatControlsConfig<'a, F, M> {
    pub active_model: Option<&'a ModelRecord>,
    pub gguf_cache: Arc<GgufMetadataCache>,
    pub rt_handle: tokio::runtime::Handle,
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
    let crate::ui::dialog_scaffold::DialogScaffold {
        dialog,
        toolbar,
        primary_btn: apply_btn,
    } = crate::ui::dialog_scaffold::build_dialog_scaffold(
        &gettext("Chat Controls"),
        520,
        560,
        &gettext("Apply"),
        false,
    );

    let section = build_settings_section(&cfg);
    spawn_gguf_ctx_probe(&cfg, &section);
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

    wire_manage_and_apply(
        &dialog,
        &apply_btn,
        &section,
        &text_view,
        cfg.on_manage_models,
        cfg.on_apply,
    );
    dialog.present(Some(parent));
}

fn spawn_gguf_ctx_probe<F, M>(
    cfg: &ChatControlsConfig<'_, F, M>,
    section: &SettingsSectionWidgets,
) {
    let Some(model_path) = cfg.active_model.and_then(|m| m.file_path.as_deref()) else {
        return;
    };
    let path = PathBuf::from(model_path);
    let (cache, rebuild_ctx, form, ctx_row) = (
        cfg.gguf_cache.clone(),
        section.rebuild_ctx.clone(),
        section.form.clone(),
        section.ctx_row.downgrade(),
    );
    spawn_blocking_ui(
        &cfg.rt_handle,
        move || cache.inspect(&path).max_context,
        move |detected| {
            if let Some(max_ctx) = detected
                && let Some(row) = ctx_row.upgrade()
            {
                rebuild_ctx(max_ctx, form.selected_ctx(row.selected()));
            }
        },
    );
}

fn wire_manage_and_apply<F, M>(
    dialog: &adw::Dialog,
    apply_btn: &gtk::Button,
    section: &SettingsSectionWidgets,
    text_view: &gtk::TextView,
    on_manage: M,
    on_apply: F,
) where
    F: Fn(ThinkingLevel, u32, String) + 'static,
    M: Fn() + 'static,
{
    let d_manage = dialog.downgrade();
    section.manage_btn.connect_clicked(move |_| {
        if let Some(d) = d_manage.upgrade() {
            d.close();
        }
        on_manage();
    });
    let (d_apply, buf, form, thinking_row, ctx_row) = (
        dialog.downgrade(),
        text_view.buffer(),
        section.form.clone(),
        section.thinking_row.clone(),
        section.ctx_row.clone(),
    );
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
        if let Some(d) = d_apply.upgrade() {
            d.close();
        }
    });
}

fn build_settings_section<F, M>(cfg: &ChatControlsConfig<'_, F, M>) -> SettingsSectionWidgets {
    let settings_group = adw::PreferencesGroup::new();
    let (model_row, manage_btn) = build_active_model_row(cfg.active_model);
    let (thinking_row, init_thinking_idx, default_thinking_idx) =
        build_thinking_row(cfg.active_model, cfg.current_thinking);

    let init_max_ctx = cfg.current_ctx.max(131072);
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
    let (usage_row, update_usage) = build_usage_row(cfg.used_tokens);
    let rebuild_ctx = make_rebuild_ctx_fn(&ctx_row, &form, &update_usage);
    rebuild_ctx(init_max_ctx, cfg.current_ctx);

    for w in [
        model_row.upcast_ref::<gtk::Widget>(),
        thinking_row.upcast_ref(),
        ctx_row.upcast_ref(),
        usage_row.upcast_ref(),
    ] {
        settings_group.add(w);
    }
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

fn build_active_model_row(active_model: Option<&ModelRecord>) -> (adw::ActionRow, gtk::Button) {
    let name = active_model
        .map(|m| friendly_model_name(&m.display_name))
        .unwrap_or_else(|| gettext("No active AI model"));
    let manage_btn = gtk::Button::builder()
        .label(gettext("Manage"))
        .tooltip_text(gettext("Manage AI Models"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let row = adw::ActionRow::builder()
        .title(gettext("Active AI Model"))
        .subtitle(escape_pango_markup(&name))
        .build();
    row.add_suffix(&manage_btn);
    (row, manage_btn)
}

fn build_thinking_row(
    active_model: Option<&ModelRecord>,
    current_thinking: ThinkingLevel,
) -> (adw::ComboRow, u32, u32) {
    let labels: Vec<String> = ThinkingLevel::ALL.iter().map(|t| t.label()).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(gettext("Thinking Level"))
        .subtitle(gettext("Reasoning effort for models that support thinking"))
        .model(&gtk::StringList::new(&refs))
        .build();
    let init_idx = ThinkingLevel::ALL
        .iter()
        .position(|t| *t == current_thinking)
        .unwrap_or(0) as u32;
    row.set_selected(init_idx);
    let default_idx = active_model
        .map(|m| m.default_thinking_level())
        .and_then(|lvl| ThinkingLevel::ALL.iter().position(|t| *t == lvl))
        .unwrap_or(0) as u32;
    (row, init_idx, default_idx)
}

fn build_usage_row(used_tokens: u32) -> (adw::ActionRow, Rc<dyn Fn(u32)>) {
    let usage_bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .build();
    usage_bar.set_size_request(110, -1);
    let usage_row = adw::ActionRow::builder()
        .title(gettext("Context Usage"))
        .build();
    usage_row.add_suffix(&usage_bar);
    let ur = usage_row.clone();
    let update_usage: Rc<dyn Fn(u32)> = Rc::new(move |max_ctx| {
        let (fraction, label) = ChatControlsFormState::format_usage(used_tokens, max_ctx);
        usage_bar.set_fraction(fraction);
        ur.set_subtitle(&label);
    });
    (usage_row, update_usage)
}

fn make_rebuild_ctx_fn(
    ctx_row: &adw::ComboRow,
    form: &Rc<ChatControlsFormState>,
    update_usage: &Rc<dyn Fn(u32)>,
) -> Rc<dyn Fn(u32, u32)> {
    let (ctx_row, form, update_usage) = (ctx_row.clone(), form.clone(), update_usage.clone());
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
        .build();
    text_view.buffer().set_text(current_prompt);

    let escaped_name = escape_pango_markup(assistant_name);
    let notes_group = adw::PreferencesGroup::builder()
        .title(gettext("Chat Notes"))
        .description(tr_f(
            "Add notes, instructions, or context for {name} to remember in this conversation only. Use {assistant_name} and {user_name} to insert your Personalization names.",
            &[("name", &escaped_name)],
        ))
        .build();
    let prompt_frame = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .height_request(150)
        .css_classes(["card"])
        .build();
    prompt_frame.append(&text_view);
    notes_group.add(&prompt_frame);
    (notes_group, text_view)
}

fn wire_dirty_and_reset(
    apply_btn: &gtk::Button,
    reset_btn: &gtk::Button,
    section: &SettingsSectionWidgets,
    text_view: &gtk::TextView,
) {
    let (a_c, r_c, th_c, ctx_c, f_c, buf_c) = (
        apply_btn.clone(),
        reset_btn.clone(),
        section.thinking_row.clone(),
        section.ctx_row.clone(),
        section.form.clone(),
        text_view.buffer(),
    );
    let sync_apply: Rc<dyn Fn()> = Rc::new(move || {
        let (s, e) = buf_c.bounds();
        let cur_prompt = buf_c.text(&s, &e, false);
        a_c.set_sensitive(f_c.is_dirty(th_c.selected(), ctx_c.selected(), &cur_prompt));
        r_c.set_sensitive(!f_c.is_default(
            th_c.selected(),
            ctx_c.selected(),
            &cur_prompt,
            default_system_prompt(),
        ));
    });
    sync_apply();

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
    wire_reset_button(reset_btn, section, text_view);
}

fn wire_reset_button(
    reset_btn: &gtk::Button,
    section: &SettingsSectionWidgets,
    text_view: &gtk::TextView,
) {
    let (buf, tr, rc, init_max_ctx, default_thinking_idx) = (
        text_view.buffer(),
        section.thinking_row.clone(),
        section.rebuild_ctx.clone(),
        section.form.init_max_ctx,
        section.form.default_thinking_idx,
    );
    reset_btn.connect_clicked(move |_| {
        buf.set_text(default_system_prompt());
        tr.set_selected(default_thinking_idx);
        rc(init_max_ctx, 4096);
    });
}
