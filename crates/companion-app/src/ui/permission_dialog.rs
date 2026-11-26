//! Permission confirmation dialog (`AdwAlertDialog`) for sensitive agent tool calls.

use adw::prelude::*;

use crate::agent::permissions::PermissionDecision;
use crate::agent::tool::ToolRisk;
use crate::i18n::{gettext, tr_f};

/// Prompt the user to approve or deny a tool call.
pub fn present_tool_permission_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    friendly_name: &str,
    preview: &str,
    risk: ToolRisk,
    allow_for_chat: bool,
    on_decision: F,
) where
    F: FnOnce(PermissionDecision) + 'static,
{
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
        .heading(tr_f("Allow {tool}?", &[("tool", friendly_name)]))
        .body(&body_text)
        .build();
    configure_tool_alert_responses(&alert, risk, allow_for_chat);
    set_preview_extra_child(&alert, &detail_text);

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

fn configure_tool_alert_responses(alert: &adw::AlertDialog, risk: ToolRisk, allow_for_chat: bool) {
    let is_destructive = risk == ToolRisk::Destructive;
    let allow_chat = allow_for_chat && !is_destructive && !matches!(risk, ToolRisk::Network);
    alert.add_response("deny", &gettext("Deny"));
    alert.add_response("once", &gettext("Allow Once"));
    if allow_chat {
        alert.add_response("chat", &gettext("Allow for Chat"));
        alert.set_response_appearance("chat", adw::ResponseAppearance::Suggested);
    } else if !is_destructive {
        alert.set_response_appearance("once", adw::ResponseAppearance::Suggested);
    }
    if is_destructive {
        alert.set_response_appearance("deny", adw::ResponseAppearance::Default);
        alert.set_response_appearance("once", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("deny"));
    } else {
        alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
        alert.set_default_response(Some("once"));
    }
    alert.set_close_response("deny");
}

fn set_preview_extra_child(alert: &adw::AlertDialog, detail_text: &str) {
    let preview_label = gtk::Label::builder()
        .label(detail_text)
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
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
    alert.set_extra_child(Some(&build_folder_path_card(folder_path)));

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

fn build_folder_path_card(folder_path: &str) -> gtk::Box {
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
    card
}

struct EditableConfirmSpec<'a> {
    heading: String,
    body: String,
    confirm_id: &'a str,
    confirm_label: String,
    initial_text: &'a str,
    placeholder: String,
    warnings: &'a [String],
}

fn present_editable_confirm_dialog<F>(
    parent: &impl IsA<gtk::Widget>,
    spec: EditableConfirmSpec<'_>,
    on_decision: F,
) where
    F: FnOnce(Option<String>) + 'static,
{
    let alert = adw::AlertDialog::builder()
        .heading(&spec.heading)
        .body(&spec.body)
        .build();

    alert.add_response("deny", &gettext("Deny"));
    alert.add_response(spec.confirm_id, &spec.confirm_label);
    alert.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
    alert.set_response_appearance(spec.confirm_id, adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some(spec.confirm_id));

    let (content, entry) = build_editable_confirm_content(&spec);
    alert.set_extra_child(Some(&content));

    let cb = std::cell::RefCell::new(Some(on_decision));
    let entry_clone = entry.clone();
    let confirm_id = spec.confirm_id.to_string();
    alert.connect_response(None, move |_, response| {
        if let Some(f) = cb.borrow_mut().take() {
            if response == confirm_id {
                let text = entry_clone.text().trim().to_string();
                f((!text.is_empty()).then_some(text));
            } else {
                f(None);
            }
        }
    });

    alert.present(Some(parent));
    entry.grab_focus();
}

fn build_editable_confirm_content(spec: &EditableConfirmSpec<'_>) -> (gtk::Box, gtk::Entry) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    if !spec.warnings.is_empty() {
        let warn_text = spec
            .warnings
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
        .text(spec.initial_text)
        .activates_default(true)
        .placeholder_text(&spec.placeholder)
        .build();
    content.append(&entry);
    (content, entry)
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
    present_editable_confirm_dialog(
        parent,
        EditableConfirmSpec {
            heading: gettext("Allow Web Search?"),
            body: tr_f(
                "Review or edit the search query before sending it to {engine}:",
                &[("engine", engine_label)],
            ),
            confirm_id: "search",
            confirm_label: gettext("Search"),
            initial_text: proposed_query,
            placeholder: gettext("Search query"),
            warnings,
        },
        on_decision,
    );
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
    present_editable_confirm_dialog(
        parent,
        EditableConfirmSpec {
            heading: gettext("Allow Web Page Fetch?"),
            body: gettext("Review or edit the HTTPS URL before Companion fetches its text:"),
            confirm_id: "fetch",
            confirm_label: gettext("Fetch Page"),
            initial_text: proposed_url,
            placeholder: "https://…".to_string(),
            warnings,
        },
        on_decision,
    );
}
