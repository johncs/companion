//! Built-in agent tools capability matrix inside Agent & MCP preferences.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::agent::registry::ToolRegistry;
use crate::agent::tool::{ToolMetadata, ToolRisk};
use crate::agent::web_search::WebSearchService;
use crate::i18n::gettext;

pub(super) fn build_available_tools_group(
    web_search: Arc<WebSearchService>,
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

    let registry = ToolRegistry::with_builtins(web_search);
    let mut web_labels: Vec<gtk::Label> = Vec::new();
    for tool in registry.iter() {
        let meta = tool.metadata();
        let is_web = tool.risk() == ToolRisk::Network;
        let (row, r_lbl, rw_lbl) = build_tool_row(&meta, is_web, web_search_enabled);
        if is_web {
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
    meta: &ToolMetadata,
    is_web: bool,
    web_search_enabled: bool,
) -> (adw::ActionRow, gtk::Label, gtk::Label) {
    let row = adw::ActionRow::builder()
        .title(crate::format::markdown::escape_pango_markup(
            &meta.display_name,
        ))
        .subtitle(crate::format::markdown::escape_pango_markup(
            &meta.description,
        ))
        .build();
    let (read_col, rw_col) = if is_web {
        let status = if web_search_enabled {
            gettext("Ask")
        } else {
            gettext("Deny")
        };
        (status.clone(), status)
    } else {
        (gettext(meta.read_policy), gettext(meta.rw_policy))
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
