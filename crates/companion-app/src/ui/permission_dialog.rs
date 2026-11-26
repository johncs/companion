//! Permission confirmation dialog (`AdwAlertDialog`) for sensitive agent tool calls.

use adw::prelude::*;

use crate::agent::permissions::PermissionDecision;
use crate::i18n::{gettext, tr_f};

/// Return a localized, human-readable name for a built-in assistant tool.
pub fn friendly_tool_name(tool_name: &str) -> String {
    match tool_name {
        "list_files" => gettext("List Files"),
        "read_file" => gettext("Read File"),
        "search_files" => gettext("Search Files"),
        "write_file" => gettext("Write File"),
        "create_directory" => gettext("Create Directory"),
        "move_path" => gettext("Move / Rename"),
        "delete_file" => gettext("Delete File"),
        "delete_directory" => gettext("Delete Directory"),
        "web_search" => gettext("Web Search"),
        "fetch_url" => gettext("Fetch Web Page"),
        other => other.to_string(),
    }
}

/// Prompt the user to approve or deny a tool call.
pub fn present_tool_permission_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    tool_name: &str,
    preview: &str,
    on_decision: F,
) where
    F: FnOnce(PermissionDecision) + 'static,
{
    let friendly = friendly_tool_name(tool_name);
    let (body_text, detail_text) = match preview.split_once("\n\n") {
        Some((head, rest)) if !head.trim().is_empty() && !rest.trim().is_empty() => {
            (head.trim().to_string(), rest.trim().to_string())
        }
        _ => (
            gettext("Companion wants to perform this action:"),
            preview.trim().to_string(),
        ),
    };

    let alert = adw::AlertDialog::builder()
        .heading(tr_f("Allow {tool}?", &[("tool", &friendly)]))
        .body(&body_text)
        .build();

    let allow_chat = !matches!(tool_name, "delete_file" | "delete_directory");
    alert.add_response("deny", &gettext("Deny"));
    alert.add_response("once", &gettext("Allow Once"));
    if allow_chat {
        alert.add_response("chat", &gettext("Allow for Chat"));
        alert.set_response_appearance("chat", adw::ResponseAppearance::Suggested);
    } else {
        alert.set_response_appearance("once", adw::ResponseAppearance::Suggested);
    }
    alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("once"));

    let preview_label = gtk::Label::builder()
        .label(&detail_text)
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .can_focus(false)
        .css_classes(["monospace", "caption"])
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();

    if detail_text.lines().count() > 6 {
        let scroller = gtk::ScrolledWindow::builder()
            .propagate_natural_height(true)
            .max_content_height(220)
            .min_content_width(340)
            .css_classes(["card"])
            .child(&preview_label)
            .build();
        alert.set_extra_child(Some(&scroller));
    } else {
        let card = gtk::Box::builder().css_classes(["card"]).build();
        card.append(&preview_label);
        alert.set_extra_child(Some(&card));
    }

    let cb = std::cell::RefCell::new(Some(on_decision));
    alert.connect_response(None, move |_, response| {
        if let Some(f) = cb.borrow_mut().take() {
            let decision = match response {
                "once" => PermissionDecision::AllowOnce,
                "chat" => PermissionDecision::AllowChat,
                _ => PermissionDecision::Deny,
            };
            f(decision);
        }
    });

    alert.present(Some(parent));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderDropGrantChoice {
    Cancel,
    ReadOnly,
    ReadWrite,
}

/// Prompt the user to confirm granting access to a folder dropped onto the chat.
pub fn present_folder_drop_confirm_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    folder_name: &str,
    folder_path: &str,
    on_choice: F,
) where
    F: FnOnce(FolderDropGrantChoice) + 'static,
{
    let alert = adw::AlertDialog::builder()
        .heading(tr_f(
            "Grant access to \"{folder}\"?",
            &[("folder", folder_name)],
        ))
        .body(gettext(
            "Choose whether Companion may read or also modify files in this folder:",
        ))
        .build();

    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("read_only", &gettext("Grant Read-Only"));
    alert.add_response("read_write", &gettext("Grant Read & Write"));
    alert.set_response_appearance("read_only", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("read_only"));

    let path_label = gtk::Label::builder()
        .label(folder_path)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .css_classes(["monospace", "caption"])
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();

    let card = gtk::Box::builder().css_classes(["card"]).build();
    card.append(&path_label);
    alert.set_extra_child(Some(&card));

    let cb = std::cell::RefCell::new(Some(on_choice));
    alert.connect_response(None, move |_, response| {
        if let Some(f) = cb.borrow_mut().take() {
            let choice = match response {
                "read_only" => FolderDropGrantChoice::ReadOnly,
                "read_write" => FolderDropGrantChoice::ReadWrite,
                _ => FolderDropGrantChoice::Cancel,
            };
            f(choice);
        }
    });

    alert.present(Some(parent));
}

/// Prompt the user to inspect/edit a web search query and approve or deny it.
pub fn present_web_search_confirm_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    engine_label: &str,
    proposed_query: &str,
    warnings: &[String],
    on_decision: F,
) where
    F: FnOnce(Option<String>) + 'static,
{
    let alert = adw::AlertDialog::builder()
        .heading(gettext("Allow Web Search?"))
        .body(tr_f(
            "Review or edit the search query before sending it to {engine}:",
            &[("engine", engine_label)],
        ))
        .build();

    alert.add_response("deny", &gettext("Deny"));
    alert.add_response("search", &gettext("Search"));
    alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
    alert.set_response_appearance("search", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("search"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();

    if !warnings.is_empty() {
        let warn_text = warnings
            .iter()
            .map(|w| format!("• {w}"))
            .collect::<Vec<_>>()
            .join("\n");
        let warn_label = gtk::Label::builder()
            .label(&warn_text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["warning", "caption"])
            .build();
        content.append(&warn_label);
    }

    let entry = gtk::Entry::builder()
        .text(proposed_query)
        .activates_default(true)
        .placeholder_text(gettext("Search query"))
        .build();
    content.append(&entry);
    alert.set_extra_child(Some(&content));

    let cb = std::cell::RefCell::new(Some(on_decision));
    let entry_clone = entry.clone();
    alert.connect_response(None, move |_, response| {
        if let Some(f) = cb.borrow_mut().take() {
            if response == "search" {
                let q = entry_clone.text().trim().to_string();
                f((!q.is_empty()).then_some(q));
            } else {
                f(None);
            }
        }
    });

    alert.present(Some(parent));
    entry.grab_focus();
}

/// Prompt the user to inspect/edit an HTTPS URL before fetching its page text.
pub fn present_fetch_url_confirm_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    proposed_url: &str,
    warnings: &[String],
    on_decision: F,
) where
    F: FnOnce(Option<String>) + 'static,
{
    let alert = adw::AlertDialog::builder()
        .heading(gettext("Allow Web Page Fetch?"))
        .body(gettext(
            "Review or edit the HTTPS URL before Companion fetches its text:",
        ))
        .build();

    alert.add_response("deny", &gettext("Deny"));
    alert.add_response("fetch", &gettext("Fetch Page"));
    alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
    alert.set_response_appearance("fetch", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("fetch"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();

    if !warnings.is_empty() {
        let warn_text = warnings
            .iter()
            .map(|w| format!("• {w}"))
            .collect::<Vec<_>>()
            .join("\n");
        let warn_label = gtk::Label::builder()
            .label(&warn_text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["warning", "caption"])
            .build();
        content.append(&warn_label);
    }

    let entry = gtk::Entry::builder()
        .text(proposed_url)
        .activates_default(true)
        .placeholder_text("https://…")
        .build();
    content.append(&entry);
    alert.set_extra_child(Some(&content));

    let cb = std::cell::RefCell::new(Some(on_decision));
    let entry_clone = entry.clone();
    alert.connect_response(None, move |_, response| {
        if let Some(f) = cb.borrow_mut().take() {
            if response == "fetch" {
                let u = entry_clone.text().trim().to_string();
                f((!u.is_empty()).then_some(u));
            } else {
                f(None);
            }
        }
    });

    alert.present(Some(parent));
    entry.grab_focus();
}
