//! Built-in agent tools capability matrix inside Agent & MCP preferences.

use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::gettext;

struct ToolDisplaySpec {
    name: &'static str,
    desc: &'static str,
    read_policy: &'static str,
    rw_policy: &'static str,
    is_web: bool,
}

const TOOL_SPECS: &[ToolDisplaySpec] = &[
    ToolDisplaySpec {
        name: "list_files",
        desc: "List files and folders inside a granted directory (respecting .gitignore).",
        read_policy: "Allow",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "read_file",
        desc: "Read UTF-8 text files up to 100 KB (prompts if the path is not granted yet).",
        read_policy: "Allow",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "search_files",
        desc: "Search for matching text lines across granted files and folders.",
        read_policy: "Allow",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "write_file",
        desc: "Create or overwrite files inside a granted directory.",
        read_policy: "Ask",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "create_directory",
        desc: "Create subdirectories inside a granted directory.",
        read_policy: "Ask",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "move_path",
        desc: "Move or rename files and folders within a granted directory.",
        read_policy: "Ask",
        rw_policy: "Allow",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "delete_file",
        desc: "Delete a single file inside a granted directory.",
        read_policy: "Ask",
        rw_policy: "Ask",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "delete_directory",
        desc: "Delete a subdirectory inside a granted directory.",
        read_policy: "Ask",
        rw_policy: "Ask",
        is_web: false,
    },
    ToolDisplaySpec {
        name: "web_search",
        desc: "Search the web via DuckDuckGo or SearXNG when Web Search is enabled.",
        read_policy: "Ask",
        rw_policy: "Ask",
        is_web: true,
    },
    ToolDisplaySpec {
        name: "fetch_url",
        desc: "Read plain text from an approved HTTPS web page when Web Search is enabled.",
        read_policy: "Ask",
        rw_policy: "Ask",
        is_web: true,
    },
];

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

    let mut web_labels: Vec<gtk::Label> = Vec::new();
    for spec in TOOL_SPECS {
        let (row, r_lbl, rw_lbl) = build_tool_row(spec, web_search_enabled);
        if spec.is_web {
            web_labels.push(r_lbl);
            web_labels.push(rw_lbl);
        }
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

fn build_tool_row(
    spec: &ToolDisplaySpec,
    web_search_enabled: bool,
) -> (adw::ActionRow, gtk::Label, gtk::Label) {
    let friendly = crate::ui::permission_dialog::friendly_tool_name(spec.name);
    let desc = gettext(spec.desc);
    let row = adw::ActionRow::builder()
        .title(crate::core::markdown::escape_pango_markup(&friendly))
        .subtitle(crate::core::markdown::escape_pango_markup(&desc))
        .build();
    let (read_col, rw_col) = if spec.is_web {
        let status = if web_search_enabled {
            gettext("Ask")
        } else {
            gettext("Deny")
        };
        (status.clone(), status)
    } else {
        (gettext(spec.read_policy), gettext(spec.rw_policy))
    };
    let (cols, r_lbl, rw_lbl) = build_columns(&read_col, &rw_col, false);
    row.add_suffix(&cols);
    (row, r_lbl, rw_lbl)
}

fn build_columns(
    read_col: &str,
    rw_col: &str,
    is_header: bool,
) -> (gtk::Box, gtk::Label, gtk::Label) {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .valign(gtk::Align::Center)
        .build();
    let make_lbl = |text: &str, width: i32| {
        let lbl = gtk::Label::builder()
            .label(text)
            .width_chars(width)
            .xalign(0.5)
            .build();
        if is_header {
            lbl.add_css_class("caption-heading");
        } else {
            lbl.add_css_class("dim-label");
            lbl.add_css_class("caption");
        }
        lbl
    };
    let r = make_lbl(read_col, 6);
    let rw = make_lbl(rw_col, 12);
    box_.append(&r);
    box_.append(&rw);
    (box_, r, rw)
}
