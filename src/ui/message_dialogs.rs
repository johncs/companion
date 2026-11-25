//! GNOME `AdwDialog` modals for inspecting message metadata/thinking and editing messages.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::ThinkingLevel;
use crate::i18n::{gettext, tr_f};
use crate::ui::message_object::{MessageEntry, MessageObject};

/// Copy `text` to the system clipboard and flash a checkmark icon on `btn` for 1.2s.
pub fn copy_text_with_feedback(btn: &gtk::Button, text: &str) {
    btn.display().clipboard().set_text(text);
    btn.set_icon_name("object-select-symbolic");
    let btn_weak = btn.downgrade();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
        if let Some(b) = btn_weak.upgrade() {
            b.set_icon_name("edit-copy-symbolic");
        }
    });
}

/// Present a GNOME `AdwDialog` to edit a user message (with a reset warning hint) and resend.
pub fn present_edit_message_dialog(
    parent: &impl IsA<gtk::Widget>,
    message_id: &str,
    initial_content: &str,
    on_save: Rc<dyn Fn(String, String)>,
) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Edit Message"))
        .content_width(580)
        .content_height(380)
        .build();

    let cancel_btn = gtk::Button::builder().label(gettext("Cancel")).build();

    let save_btn = gtk::Button::builder()
        .label(gettext("Save & Resend"))
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let text_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(14)
        .bottom_margin(14)
        .left_margin(16)
        .right_margin(16)
        .css_classes(["composer-input"])
        .build();
    text_view.buffer().set_text(initial_content);

    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_height(260)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["card"])
        .child(&text_view)
        .build();

    let hint_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Start)
        .build();
    let hint_icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .pixel_size(14)
        .css_classes(["dim-label"])
        .build();
    let hint_label = gtk::Label::builder()
        .label(gettext(
            "Editing this message will reset and remove all messages after it.",
        ))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    hint_box.append(&hint_icon);
    hint_box.append(&hint_label);

    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.append(&scroller);
    content_box.append(&hint_box);

    let outer_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&outer_scroller));
    dialog.set_child(Some(&toolbar));

    let save_btn_watch = save_btn.clone();
    text_view.buffer().connect_changed(move |buf| {
        let (s, e) = buf.bounds();
        let non_empty = !buf.text(&s, &e, false).trim().is_empty();
        save_btn_watch.set_sensitive(non_empty);
    });

    let dialog_cancel = dialog.clone();
    cancel_btn.connect_clicked(move |_| {
        dialog_cancel.close();
    });

    let msg_id = message_id.to_string();
    let trigger_save: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        let tv = text_view.clone();
        let on_save = on_save.clone();
        Rc::new(move || {
            let buf = tv.buffer();
            let (s, e) = buf.bounds();
            let updated = buf.text(&s, &e, false).trim().to_string();
            if !updated.is_empty() {
                dialog.close();
                on_save(msg_id.clone(), updated);
            }
        })
    };

    let trigger_click = trigger_save.clone();
    save_btn.connect_clicked(move |_| trigger_click());

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.connect_key_pressed(move |_, key, _, mods| {
        if (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
            && mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            trigger_save();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    text_view.add_controller(key_ctrl);

    dialog.present(Some(parent));
    text_view.grab_focus();
}

/// Present a GNOME `AdwDialog` showing message info (date/time, and for assistant messages: model, thinking level, speed, tokens, thinking trace, and tool calls).
pub fn present_message_info_dialog(parent: &impl IsA<gtk::Widget>, msg_obj: MessageObject) {
    let is_user = msg_obj.with_entry(|e| e.role == "user");
    let dialog = adw::Dialog::builder()
        .title(gettext("Message Info"))
        .content_width(560)
        .content_height(560)
        .build();

    let header = adw::HeaderBar::new();

    let date_val = gtk::Label::builder()
        .css_classes(["dim-label", "numeric"])
        .valign(gtk::Align::Center)
        .build();
    let model_val = gtk::Label::builder()
        .css_classes(["dim-label"])
        .valign(gtk::Align::Center)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let device_val = gtk::Label::builder()
        .css_classes(["dim-label"])
        .valign(gtk::Align::Center)
        .build();
    let thinking_level_val = gtk::Label::builder()
        .css_classes(["dim-label"])
        .valign(gtk::Align::Center)
        .build();
    let time_val = gtk::Label::builder()
        .css_classes(["dim-label", "numeric"])
        .valign(gtk::Align::Center)
        .build();
    let speed_val = gtk::Label::builder()
        .css_classes(["dim-label", "numeric"])
        .valign(gtk::Align::Center)
        .build();
    let tokens_val = gtk::Label::builder()
        .css_classes(["dim-label", "numeric"])
        .valign(gtk::Align::Center)
        .build();

    let stats_group = adw::PreferencesGroup::new();
    let date_row = adw::ActionRow::builder()
        .title(gettext("Date &amp; Time"))
        .build();
    date_row.add_suffix(&date_val);

    let model_row = adw::ActionRow::builder()
        .title(gettext("AI Model"))
        .build();
    model_row.add_suffix(&model_val);

    let device_row = adw::ActionRow::builder()
        .title(gettext("Compute Device"))
        .build();
    device_row.add_suffix(&device_val);

    let level_row = adw::ActionRow::builder()
        .title(gettext("Thinking Level"))
        .build();
    level_row.add_suffix(&thinking_level_val);

    let time_row = adw::ActionRow::builder()
        .title(gettext("Completion Time"))
        .build();
    time_row.add_suffix(&time_val);

    let speed_row = adw::ActionRow::builder()
        .title(gettext("Generation Speed"))
        .build();
    speed_row.add_suffix(&speed_val);

    let tokens_row = adw::ActionRow::builder()
        .title(gettext("Total Tokens"))
        .build();
    tokens_row.add_suffix(&tokens_val);

    stats_group.add(&date_row);
    if !is_user {
        stats_group.add(&model_row);
        stats_group.add(&device_row);
        stats_group.add(&level_row);
        stats_group.add(&time_row);
        stats_group.add(&speed_row);
        stats_group.add(&tokens_row);
    }

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
        let text = buf_copy.text(&s, &e, false);
        copy_text_with_feedback(btn, &text);
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

    let attachments_group = adw::PreferencesGroup::builder()
        .title(gettext("Attachments"))
        .visible(false)
        .build();

    let tools_group = adw::PreferencesGroup::builder()
        .title(gettext("Tool Calls"))
        .visible(false)
        .build();

    let last_len = Rc::new(std::cell::Cell::new(0usize));
    let rendered_tools = Rc::new(std::cell::Cell::new(0usize));
    let rendered_atts = Rc::new(std::cell::Cell::new(false));
    let sync_from_entry = {
        let date_val = date_val.clone();
        let model_val = model_val.clone();
        let thinking_level_val = thinking_level_val.clone();
        let time_val = time_val.clone();
        let speed_val = speed_val.clone();
        let tokens_val = tokens_val.clone();
        let attachments_group = attachments_group.clone();
        let rendered_atts = rendered_atts.clone();
        let tools_group = tools_group.clone();
        let rendered_tools = rendered_tools.clone();
        let thinking_section = thinking_section.clone();
        let buf = buf.clone();
        let last_len = last_len.clone();
        move |entry: &MessageEntry| {
            date_val.set_label(&format_timestamp_text(entry.created_at));
            if is_user {
                thinking_section.set_visible(false);
                tools_group.set_visible(false);
                if !entry.attachments.is_empty() && !rendered_atts.get() {
                    attachments_group.set_visible(true);
                    for att in &entry.attachments {
                        let att_row = adw::ActionRow::builder()
                            .title(glib::markup_escape_text(&att.file_name))
                            .subtitle(glib::markup_escape_text(&att.mime_type))
                            .build();
                        attachments_group.add(&att_row);
                    }
                    rendered_atts.set(true);
                }
                return;
            }
            model_val.set_label(entry.model_name.as_deref().unwrap_or("—"));
            let device_label = match entry.execution_device.as_deref() {
                Some("gpu") => gettext("GPU (Vulkan)"),
                Some("cpu") => gettext("CPU"),
                _ => "—".to_string(),
            };
            device_val.set_label(&device_label);
            let lvl = entry
                .thinking_level
                .as_deref()
                .map(ThinkingLevel::from_str_lossy)
                .unwrap_or_default();
            thinking_level_val.set_label(&lvl.label());
            time_val.set_label(&format_duration_text(entry));
            speed_val.set_label(&format_speed_text(entry));
            tokens_val.set_label(&format_tokens_text(entry));

            if entry.tool_calls.is_empty() {
                tools_group.set_visible(false);
            } else {
                tools_group.set_visible(true);
                let prev_tc = rendered_tools.get();
                if entry.tool_calls.len() > prev_tc {
                    for tc in &entry.tool_calls[prev_tc..] {
                        tools_group.add(&build_tool_call_row(tc));
                    }
                    rendered_tools.set(entry.tool_calls.len());
                }
            }

            if let Some(full) = entry.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
                thinking_section.set_visible(true);
                let prev = last_len.get();
                if prev == 0 {
                    buf.set_text(full);
                    last_len.set(full.len());
                } else if full.len() > prev && full.is_char_boundary(prev) {
                    let mut end = buf.end_iter();
                    buf.insert(&mut end, &full[prev..]);
                    last_len.set(full.len());
                }
            } else {
                thinking_section.set_visible(false);
            }
        }
    };

    msg_obj.with_entry(&sync_from_entry);

    let content_box = crate::ui::widgets::dialog_content_box();
    content_box.set_vexpand(true);
    content_box.append(&stats_group);
    content_box.append(&attachments_group);
    content_box.append(&tools_group);
    content_box.append(&thinking_section);

    let outer_scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content_box)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
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
    let status_class = match tc.status.as_str() {
        "error" => "error",
        "denied" => "warning",
        _ => "dim-label",
    };
    let status_lbl = gtk::Label::builder()
        .label(&tc.status)
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
            for key in ["path", "subdir", "query", "command"] {
                if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                    return Some(s.to_string());
                }
            }
            None
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

    if tc.status != "ok"
        && let Some(err) = tc.output.as_deref().map(str::trim).filter(|s| !s.is_empty())
    {
        return format!("{target} — {err}");
    }
    target
}
