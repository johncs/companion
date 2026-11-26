//! Built-in agent tools capability matrix inside Agent & MCP preferences.

use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::gettext;

pub(super) fn build_available_tools_group(
    web_search_enabled: bool,
) -> (adw::PreferencesGroup, Rc<dyn Fn(bool)>) {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Available Companion Tools"))
        .description(gettext(
            "Built-in tools your companion can invoke. Local file tools are jailed to granted folders, and sensitive or web actions require your approval.",
        ))
        .build();

    let header_row = adw::ActionRow::builder()
        .title(gettext("Tool"))
        .activatable(false)
        .selectable(false)
        .build();
    header_row.add_css_class("dim-label");
    let (header_cols, _, _) = build_columns(&gettext("Read"), &gettext("Read & Write"), true);
    header_row.add_suffix(&header_cols);
    group.add(&header_row);

    let web_status = if web_search_enabled {
        gettext("Ask")
    } else {
        gettext("Deny")
    };
    let tools = [
        (
            "list_files",
            gettext("List files and folders inside a granted directory (respecting .gitignore)."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "read_file",
            gettext("Read UTF-8 text files up to 100 KB (prompts if the path is not granted yet)."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "search_files",
            gettext("Search for matching text lines across granted files and folders."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "write_file",
            gettext("Create or overwrite files inside a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "create_directory",
            gettext("Create subdirectories inside a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "move_path",
            gettext("Move or rename files and folders within a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "delete_file",
            gettext("Delete a single file inside a granted directory."),
            gettext("Ask"),
            gettext("Ask"),
            false,
        ),
        (
            "delete_directory",
            gettext("Delete a subdirectory inside a granted directory."),
            gettext("Ask"),
            gettext("Ask"),
            false,
        ),
        (
            "web_search",
            gettext("Search the web via DuckDuckGo or SearXNG when Web Search is enabled."),
            web_status.clone(),
            web_status.clone(),
            true,
        ),
        (
            "fetch_url",
            gettext("Read plain text from an approved HTTPS web page when Web Search is enabled."),
            web_status.clone(),
            web_status,
            true,
        ),
    ];

    let mut web_labels: Vec<gtk::Label> = Vec::new();
    for (name, desc, read_col, rw_col, is_web) in tools {
        let friendly = crate::ui::permission_dialog::friendly_tool_name(name);
        let row = adw::ActionRow::builder()
            .title(&friendly)
            .subtitle(&desc)
            .build();
        let (cols, r_lbl, rw_lbl) = build_columns(&read_col, &rw_col, false);
        if is_web {
            web_labels.push(r_lbl);
            web_labels.push(rw_lbl);
        }
        row.add_suffix(&cols);
        group.add(&row);
    }

    let update_web_tools: Rc<dyn Fn(bool)> = Rc::new(move |enabled| {
        let text = if enabled {
            gettext("Ask")
        } else {
            gettext("Deny")
        };
        for lbl in &web_labels {
            lbl.set_label(&text);
        }
    });

    (group, update_web_tools)
}

fn build_columns(read_col: &str, rw_col: &str, is_header: bool) -> (gtk::Box, gtk::Label, gtk::Label) {
    let cols = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .valign(gtk::Align::Center)
        .build();
    let classes: &[&str] = if is_header {
        &["heading", "caption", "dim-label"]
    } else {
        &["caption", "dim-label"]
    };
    let read_lbl = gtk::Label::builder()
        .label(read_col)
        .xalign(1.0)
        .width_request(64)
        .css_classes(classes)
        .build();
    let rw_lbl = gtk::Label::builder()
        .label(rw_col)
        .xalign(1.0)
        .width_request(105)
        .css_classes(classes)
        .build();
    cols.append(&read_lbl);
    cols.append(&rw_lbl);
    (cols, read_lbl, rw_lbl)
}
