//! `AdwDialog` modal showing message metadata, thinking trace, attachments, and tool calls.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::ThinkingLevel;
use crate::core::types::ToolCallStatus;
use crate::i18n::{gettext, tr_f};
use crate::ui::message_object::{MessageEntry, MessageObject};

use super::copy_text_with_feedback;

struct InfoWidgets {
    date_val: gtk::Label,
    model_val: gtk::Label,
    device_val: gtk::Label,
    thinking_level_val: gtk::Label,
    time_val: gtk::Label,
    speed_val: gtk::Label,
    tokens_val: gtk::Label,
    attachments_group: adw::PreferencesGroup,
    tools_group: adw::PreferencesGroup,
    thinking_section: gtk::Box,
    thinking_buf: gtk::TextBuffer,
}

/// Present a GNOME `AdwDialog` showing message info, thinking trace, attachments, and tool calls.
pub fn present_message_info_dialog(parent: &impl IsA<gtk::Widget>, msg_obj: MessageObject) {
    let is_user = msg_obj.with_entry(|e| e.role == "user");
    let dialog = adw::Dialog::builder()
        .title(gettext("Message Info"))
        .content_width(560)
        .content_height(560)
        .build();

    let (stats_group, mut widgets) = build_stats_group(is_user);
    let (thinking_section, thinking_buf) = build_thinking_section();
    widgets.thinking_section = thinking_section;
    widgets.thinking_buf = thinking_buf;

    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.set_vexpand(true);
    content_box.append(&stats_group);
    content_box.append(&widgets.attachments_group);
    content_box.append(&widgets.tools_group);
    content_box.append(&widgets.thinking_section);

    let last_len = Rc::new(std::cell::Cell::new(0usize));
    let rendered_tools = Rc::new(std::cell::Cell::new(0usize));
    let rendered_atts = Rc::new(std::cell::Cell::new(false));
    let sync_from_entry = move |entry: &MessageEntry| {
        sync_entry_to_widgets(entry, is_user, &widgets, &last_len, &rendered_tools, &rendered_atts);
    };

    msg_obj.with_entry(&sync_from_entry);

    let outer_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&outer_scroller));
    dialog.set_child(Some(&toolbar));

    let obj_watch = msg_obj.clone();
    let handler_id = Rc::new(RefCell::new(Some(msg_obj.connect_local(
        "updated",
        false,
        move |_| {
            obj_watch.with_entry(&sync_from_entry);
            None
        },
    ))));
    dialog.connect_closed(move |_| {
        if let Some(id) = handler_id.borrow_mut().take() {
            msg_obj.disconnect(id);
        }
    });

    dialog.present(Some(parent));
}

fn make_val_label(numeric: bool) -> gtk::Label {
    let classes: &[&str] = if numeric {
        &["dim-label", "numeric"]
    } else {
        &["dim-label"]
    };
    gtk::Label::builder()
        .css_classes(classes)
        .valign(gtk::Align::Center)
        .build()
}

fn build_stats_group(is_user: bool) -> (adw::PreferencesGroup, InfoWidgets) {
    let date_val = make_val_label(true);
    let model_val = make_val_label(false);
    model_val.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let device_val = make_val_label(false);
    let thinking_level_val = make_val_label(false);
    let time_val = make_val_label(true);
    let speed_val = make_val_label(true);
    let tokens_val = make_val_label(true);

    let stats_group = adw::PreferencesGroup::new();
    for (title, val, assistant_only) in [
        (gettext("Date &amp; Time"), &date_val, false),
        (gettext("AI Model"), &model_val, true),
        (gettext("Compute Device"), &device_val, true),
        (gettext("Thinking Level"), &thinking_level_val, true),
        (gettext("Completion Time"), &time_val, true),
        (gettext("Generation Speed"), &speed_val, true),
        (gettext("Total Tokens"), &tokens_val, true),
    ] {
        if !assistant_only || !is_user {
            let row = adw::ActionRow::builder().title(title).build();
            row.add_suffix(val);
            stats_group.add(&row);
        }
    }

    let widgets = InfoWidgets {
        date_val,
        model_val,
        device_val,
        thinking_level_val,
        time_val,
        speed_val,
        tokens_val,
        attachments_group: adw::PreferencesGroup::builder()
            .title(gettext("Attachments"))
            .visible(false)
            .build(),
        tools_group: adw::PreferencesGroup::builder()
            .title(gettext("Tool Calls"))
            .visible(false)
            .build(),
        thinking_section: gtk::Box::new(gtk::Orientation::Vertical, 0),
        thinking_buf: gtk::TextBuffer::new(None),
    };
    (stats_group, widgets)
}

fn build_thinking_section() -> (gtk::Box, gtk::TextBuffer) {
    let thinking_header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .build();
    let thinking_title = gtk::Label::builder()
        .label(gettext("Thinking"))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["heading"])
        .build();
    let copy_btn = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(gettext("Copy thinking"))
        .css_classes(["flat", "circular"])
        .build();
    thinking_header.append(&thinking_title);
    thinking_header.append(&copy_btn);

    let text_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .pixels_above_lines(2)
        .pixels_below_lines(2)
        .pixels_inside_wrap(3)
        .top_margin(14)
        .bottom_margin(14)
        .left_margin(16)
        .right_margin(16)
        .css_classes(["composer-input"])
        .build();
    let buf = text_view.buffer();
    let buf_copy = buf.clone();
    copy_btn.connect_clicked(move |btn| {
        let (s, e) = buf_copy.bounds();
        copy_text_with_feedback(btn, &buf_copy.text(&s, &e, false));
    });

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_height(260)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();

    let thinking_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .vexpand(true)
        .build();
    thinking_section.append(&thinking_header);
    thinking_section.append(&scroller);
    (thinking_section, buf)
}

fn sync_entry_to_widgets(
    entry: &MessageEntry,
    is_user: bool,
    w: &InfoWidgets,
    last_len: &std::cell::Cell<usize>,
    rendered_tools: &std::cell::Cell<usize>,
    rendered_atts: &std::cell::Cell<bool>,
) {
    w.date_val.set_label(&format_timestamp_text(entry.created_at));
    if is_user {
        w.thinking_section.set_visible(false);
        w.tools_group.set_visible(false);
        if !entry.attachments.is_empty() && !rendered_atts.get() {
            w.attachments_group.set_visible(true);
            for att in &entry.attachments {
                w.attachments_group.add(
                    &adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&att.file_name))
                        .subtitle(glib::markup_escape_text(&att.mime_type))
                        .build(),
                );
            }
            rendered_atts.set(true);
        }
        return;
    }

    w.model_val
        .set_label(entry.model_name.as_deref().unwrap_or("—"));
    let device_label = match entry.execution_device.as_deref() {
        Some("gpu") => gettext("GPU (Vulkan)"),
        Some("cpu") => gettext("CPU"),
        _ => "—".to_string(),
    };
    w.device_val.set_label(&device_label);
    let lvl = entry
        .thinking_level
        .as_deref()
        .map(ThinkingLevel::from_str_lossy)
        .unwrap_or_default();
    w.thinking_level_val.set_label(&lvl.label());
    w.time_val.set_label(&format_duration_text(entry));
    w.speed_val.set_label(&format_speed_text(entry));
    w.tokens_val.set_label(&format_tokens_text(entry));

    if entry.tool_calls.is_empty() {
        w.tools_group.set_visible(false);
    } else {
        w.tools_group.set_visible(true);
        let prev_tc = rendered_tools.get();
        if entry.tool_calls.len() > prev_tc {
            for tc in &entry.tool_calls[prev_tc..] {
                w.tools_group.add(&build_tool_call_row(tc));
            }
            rendered_tools.set(entry.tool_calls.len());
        }
    }

    if let Some(full) = entry.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
        w.thinking_section.set_visible(true);
        let prev = last_len.get();
        if prev == 0 {
            w.thinking_buf.set_text(full);
            last_len.set(full.len());
        } else if full.len() > prev && full.is_char_boundary(prev) {
            let mut end = w.thinking_buf.end_iter();
            w.thinking_buf.insert(&mut end, &full[prev..]);
            last_len.set(full.len());
        }
    } else {
        w.thinking_section.set_visible(false);
    }
}

fn format_timestamp_text(created_at_ms: i64) -> String {
    glib::DateTime::from_unix_utc(created_at_ms / 1000)
        .and_then(|dt| dt.to_local())
        .and_then(|local| local.format("%x · %X"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "—".to_string())
}

fn format_duration_text(entry: &MessageEntry) -> String {
    if entry.is_streaming {
        gettext("Generating…")
    } else if let Some(ms) = entry.generation_ms {
        format!("{:.2} s", (ms as f64) / 1000.0)
    } else {
        "—".to_string()
    }
}

fn format_speed_text(entry: &MessageEntry) -> String {
    if entry.is_streaming {
        gettext("Generating…")
    } else if let Some(ms) = entry.generation_ms
        && ms > 0
        && entry.token_count > 0
    {
        let tps = (entry.token_count as f64) / ((ms as f64) / 1000.0);
        tr_f("{speed} tokens/s", &[("speed", &format!("{tps:.1}"))])
    } else {
        "—".to_string()
    }
}

fn format_tokens_text(entry: &MessageEntry) -> String {
    if entry.is_streaming {
        gettext("Generating…")
    } else if entry.token_count > 0 {
        entry.token_count.to_string()
    } else {
        "—".to_string()
    }
}

fn build_tool_call_row(tc: &crate::storage::ToolCallRecord) -> adw::ActionRow {
    let summary = summarize_tool_call(tc);
    let status_class = match tc.status {
        ToolCallStatus::Error => "error",
        ToolCallStatus::Denied => "warning",
        ToolCallStatus::Ok => "dim-label",
    };
    let status_lbl = gtk::Label::builder()
        .label(tc.status.as_str())
        .valign(gtk::Align::Center)
        .css_classes([status_class])
        .build();

    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&tc.tool_name))
        .subtitle(glib::markup_escape_text(&summary))
        .subtitle_lines(1)
        .build();
    row.add_suffix(&status_lbl);
    row
}

fn summarize_tool_call(tc: &crate::storage::ToolCallRecord) -> String {
    let raw_target = serde_json::from_str::<serde_json::Value>(&tc.arguments_json)
        .ok()
        .and_then(|v| {
            ["path", "subdir", "query", "command"]
                .iter()
                .find_map(|key| v.get(*key).and_then(|x| x.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| {
            tc.arguments_json
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
    let target = if raw_target.trim() == "." {
        gettext("Granted root (.)")
    } else {
        raw_target
    };

    if tc.status != ToolCallStatus::Ok
        && let Some(err) = tc.output.as_deref().map(str::trim).filter(|s| !s.is_empty())
    {
        return format!("{target} — {err}");
    }
    target
}
