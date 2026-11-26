//! Web Search settings group inside Agent & MCP preferences.

use std::rc::Rc;

use adw::prelude::*;

use crate::agent::web_search::SearchEngine;
use crate::i18n::gettext;
use crate::storage::Database;

pub(super) fn build_web_search_group<W>(
    db: &Database,
    on_global_changed: W,
) -> adw::PreferencesGroup
where
    W: Fn(bool) + 'static,
{
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Web Search"))
        .description(gettext(
            "Disabled by default for privacy. When enabled, you can toggle Web Search per chat and review every query or URL before it is sent.",
        ))
        .build();

    let global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
    let enable_row = adw::SwitchRow::builder()
        .title(gettext("Enable Web Search"))
        .subtitle(gettext(
            "Allow chats to toggle Web Search and fetch approved HTTPS pages",
        ))
        .active(global_enabled)
        .build();

    let engines = gtk::StringList::new(&[
        &gettext("DuckDuckGo"),
        &gettext("SearXNG (Custom URL)"),
    ]);
    let current_engine = db.get_search_engine().unwrap_or_default();
    let engine_row = adw::ComboRow::builder()
        .title(gettext("Search Engine"))
        .model(&engines)
        .selected(match current_engine {
            SearchEngine::DuckDuckGo => 0,
            SearchEngine::SearXNG => 1,
        })
        .sensitive(global_enabled)
        .build();

    let current_url = db.get_searxng_url().ok().flatten().unwrap_or_default();
    let url_row = adw::EntryRow::builder()
        .title(gettext("SearXNG Instance URL"))
        .text(&current_url)
        .visible(current_engine == SearchEngine::SearXNG)
        .sensitive(global_enabled)
        .build();

    {
        let db = db.clone();
        let engine_row_ref = engine_row.clone();
        let url_row_ref = url_row.clone();
        let on_global_changed = Rc::new(on_global_changed);
        enable_row.connect_active_notify(move |row| {
            let active = row.is_active();
            let _ = db.set_web_search_global_enabled(active);
            engine_row_ref.set_sensitive(active);
            url_row_ref.set_sensitive(active);
            on_global_changed(active);
        });
    }

    {
        let db = db.clone();
        let url_row_ref = url_row.clone();
        engine_row.connect_selected_notify(move |row| {
            let engine = match row.selected() {
                1 => SearchEngine::SearXNG,
                _ => SearchEngine::DuckDuckGo,
            };
            let _ = db.set_search_engine(engine);
            url_row_ref.set_visible(engine == SearchEngine::SearXNG);
        });
    }

    {
        let db = db.clone();
        let commit_url = Rc::new(move |row: &adw::EntryRow| {
            let _ = db.set_searxng_url(&row.text());
        });
        let c1 = commit_url.clone();
        url_row.connect_changed(move |row| c1(row));
        let c2 = commit_url.clone();
        url_row.connect_entry_activated(move |row| c2(row));
        let focus = gtk::EventControllerFocus::new();
        let row_ref = url_row.clone();
        focus.connect_leave(move |_| commit_url(&row_ref));
        url_row.add_controller(focus);
    }

    group.add(&enable_row);
    group.add(&engine_row);
    group.add(&url_row);
    group
}
