//! `AdwDialog` modal showing message metadata, thinking trace, attachments, and tool calls.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::types::{MessageRole, ToolCallStatus};
use crate::i18n::gettext;
use crate::ui::message_object::{MessageEntry, MessageObject};

use super::view_model::MessageInfoViewModel;
use super::{copy_text_with_feedback, format_timestamp_text};

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
    let is_user = msg_obj.with_entry(|e| e.role == MessageRole::User);
    let dialog = adw::Dialog::builder()
        .title(gettext("Message Info"))
        .content_width(560)
        .content_height(560)
        .build();

    let (thinking_section, thinking_buf) = build_thinking_section();
    let (stats_group, widgets) = build_stats_group(is_user, thinking_section, thinking_buf);
    let content_box = build_info_content_box(&stats_group, &widgets);

    let (last_len, tool_rows, rendered_atts) = (
        Rc::new(std::cell::Cell::new(0usize)),
        Rc::new(RefCell::new(Vec::new())),
        Rc::new(std::cell::Cell::new(false)),
    );
    let sync_from_entry = move |entry: &MessageEntry| {
        sync_entry_to_widgets(
            entry,
            is_user,
            &widgets,
            &last_len,
            &tool_rows,
            &rendered_atts,
        );
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
    connect_info_live_updates(&dialog, msg_obj, sync_from_entry);
    dialog.present(Some(parent));
}

fn build_info_content_box(stats_group: &adw::PreferencesGroup, widgets: &InfoWidgets) -> gtk::Box {
    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.set_vexpand(true);
    for child in [
        stats_group.upcast_ref::<gtk::Widget>(),
        widgets.attachments_group.upcast_ref(),
        widgets.tools_group.upcast_ref(),
        widgets.thinking_section.upcast_ref(),
    ] {
        content_box.append(child);
    }
    content_box
}

fn connect_info_live_updates(
    dialog: &adw::Dialog,
    msg_obj: MessageObject,
    sync_from_entry: impl Fn(&MessageEntry) + 'static,
) {
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

fn build_stats_group(
    is_user: bool,
    thinking_section: gtk::Box,
    thinking_buf: gtk::TextBuffer,
) -> (adw::PreferencesGroup, InfoWidgets) {
    let date_val = make_val_label(true);
    let model_val = make_val_label(false);
    model_val.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let device_val = make_val_label(false);
    let thinking_level_val = make_val_label(false);
    let time_val = make_val_label(true);
    let speed_val = make_val_label(true);
    let tokens_val = make_val_label(true);

    let stats_group = adw::PreferencesGroup::new();
    add_stat_rows(
        &stats_group,
        is_user,
        &[
            (gettext("Date &amp; Time"), &date_val, false),
            (gettext("AI Model"), &model_val, true),
            (gettext("Compute Device"), &device_val, true),
            (gettext("Thinking Level"), &thinking_level_val, true),
            (gettext("Completion Time"), &time_val, true),
            (gettext("Generation Speed"), &speed_val, true),
            (gettext("Total Tokens"), &tokens_val, true),
        ],
    );

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
        thinking_section,
        thinking_buf,
    };
    (stats_group, widgets)
}

fn add_stat_rows(
    group: &adw::PreferencesGroup,
    is_user: bool,
    items: &[(String, &gtk::Label, bool)],
) {
    for (title, val, assistant_only) in items {
        if !assistant_only || !is_user {
            let row = adw::ActionRow::builder().title(title).build();
            row.add_suffix(*val);
            group.add(&row);
        }
    }
}

fn build_thinking_section() -> (gtk::Box, gtk::TextBuffer) {
    let (thinking_header, copy_btn) = build_thinking_header();
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
        .build();
    let buf = text_view.buffer();
    let buf_copy = buf.clone();
    copy_btn.connect_clicked(move |btn| {
        let (s, e) = buf_copy.bounds();
        copy_text_with_feedback(btn, &buf_copy.text(&s, &e, false));
    });
    let scroller = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .hexpand(true)
        .css_classes(["card"])
        .build();
    scroller.append(&text_view);
    let thinking_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .vexpand(true)
        .build();
    thinking_section.append(&thinking_header);
    thinking_section.append(&scroller);
    (thinking_section, buf)
}

fn build_thinking_header() -> (gtk::Box, gtk::Button) {
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
    (thinking_header, copy_btn)
}

struct ToolRowWidgets {
    row: adw::ActionRow,
    status_lbl: gtk::Label,
}

fn sync_entry_to_widgets(
    entry: &MessageEntry,
    is_user: bool,
    w: &InfoWidgets,
    last_len: &std::cell::Cell<usize>,
    tool_rows: &RefCell<Vec<ToolRowWidgets>>,
    rendered_atts: &std::cell::Cell<bool>,
) {
    w.date_val
        .set_label(&format_timestamp_text(entry.created_at));
    if is_user {
        w.thinking_section.set_visible(false);
        w.tools_group.set_visible(false);
        sync_user_attachments(entry, &w.attachments_group, rendered_atts);
        return;
    }

    w.model_val
        .set_label(entry.model_name.as_deref().unwrap_or("—"));
    w.device_val
        .set_label(&MessageInfoViewModel::device_label(entry.execution_device));
    w.thinking_level_val
        .set_label(&entry.thinking_level.unwrap_or_default().label());
    w.time_val
        .set_label(&MessageInfoViewModel::duration_text(entry));
    w.speed_val
        .set_label(&MessageInfoViewModel::speed_text(entry));
    w.tokens_val
        .set_label(&MessageInfoViewModel::tokens_text(entry));

    sync_tool_call_rows(&entry.tool_calls, &w.tools_group, tool_rows);
    sync_thinking_buffer(
        entry.reasoning_content.as_deref(),
        &w.thinking_section,
        &w.thinking_buf,
        last_len,
    );
}

fn sync_user_attachments(
    entry: &MessageEntry,
    group: &adw::PreferencesGroup,
    rendered_atts: &std::cell::Cell<bool>,
) {
    if !entry.attachments.is_empty() && !rendered_atts.get() {
        group.set_visible(true);
        for att in &entry.attachments {
            group.add(
                &adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&att.file_name))
                    .subtitle(glib::markup_escape_text(&att.mime_type))
                    .build(),
            );
        }
        rendered_atts.set(true);
    }
}

fn sync_tool_call_rows(
    tool_calls: &[crate::storage::ToolCallRecord],
    group: &adw::PreferencesGroup,
    tool_rows: &RefCell<Vec<ToolRowWidgets>>,
) {
    if tool_calls.is_empty() {
        group.set_visible(false);
        return;
    }
    group.set_visible(true);
    let mut rows = tool_rows.borrow_mut();
    while rows.len() > tool_calls.len() {
        if let Some(removed) = rows.pop() {
            group.remove(&removed.row);
        }
    }
    for (idx, tc) in tool_calls.iter().enumerate() {
        if let Some(existing) = rows.get(idx) {
            update_tool_call_row(existing, tc);
        } else {
            let built = build_tool_call_row(tc);
            group.add(&built.row);
            rows.push(built);
        }
    }
}

fn sync_thinking_buffer(
    reasoning: Option<&str>,
    section: &gtk::Box,
    buf: &gtk::TextBuffer,
    last_len: &std::cell::Cell<usize>,
) {
    let Some(full) = reasoning.filter(|r| !r.is_empty()) else {
        section.set_visible(false);
        if last_len.get() != 0 {
            buf.set_text("");
            last_len.set(0);
        }
        return;
    };
    section.set_visible(true);
    let prev = last_len.get();
    if prev > 0 && full.len() > prev && full.is_char_boundary(prev) {
        let mut end = buf.end_iter();
        buf.insert(&mut end, &full[prev..]);
        last_len.set(full.len());
    } else if full.len() != prev || prev == 0 {
        buf.set_text(full);
        last_len.set(full.len());
    }
}

fn build_tool_call_row(tc: &crate::storage::ToolCallRecord) -> ToolRowWidgets {
    let status_lbl = gtk::Label::builder().valign(gtk::Align::Center).build();
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(
            &MessageInfoViewModel::tool_display_name(&tc.tool_name),
        ))
        .subtitle_lines(1)
        .build();
    row.add_suffix(&status_lbl);
    let widgets = ToolRowWidgets { row, status_lbl };
    update_tool_call_row(&widgets, tc);
    widgets
}

fn update_tool_call_row(w: &ToolRowWidgets, tc: &crate::storage::ToolCallRecord) {
    let status_class = match tc.status {
        ToolCallStatus::Error => "error",
        ToolCallStatus::Denied => "warning",
        ToolCallStatus::Ok => "dim-label",
    };
    w.row.set_title(&glib::markup_escape_text(
        &MessageInfoViewModel::tool_display_name(&tc.tool_name),
    ));
    w.row.set_subtitle(&glib::markup_escape_text(
        &MessageInfoViewModel::summarize_tool_call(tc),
    ));
    w.status_lbl
        .set_label(&MessageInfoViewModel::tool_status_label(tc.status));
    w.status_lbl.set_css_classes(&[status_class]);
}
