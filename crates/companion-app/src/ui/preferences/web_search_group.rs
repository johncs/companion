//! Web Search settings group inside Agent & MCP preferences.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::agent::web_search::SearchEngine;
use crate::i18n::gettext;
use crate::storage::Database;
use crate::ui::widgets::spawn_blocking_ui;

pub(super) fn build_web_search_group<T, W>(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    on_tools_refresh: T,
    on_global_changed: W,
) -> adw::PreferencesGroup
where
    T: Fn(bool) + 'static,
    W: Fn(bool) + 'static,
{
    let (group, enable_row) = build_web_search_header();
    let syncing = Rc::new(Cell::new(true));
    let url_row = build_searxng_url_row(db, rt_handle, &syncing);
    let engine_row = build_engine_row(db, rt_handle, &url_row, &syncing);

    let (on_tools, on_global_changed) = (Rc::new(on_tools_refresh), Rc::new(on_global_changed));
    wire_web_search_enable_row(
        &enable_row,
        &engine_row,
        &url_row,
        db,
        rt_handle,
        &syncing,
        (&on_tools, on_global_changed),
    );
    load_initial_web_search_state(
        db,
        rt_handle,
        &enable_row,
        &engine_row,
        &url_row,
        syncing,
        on_tools,
    );

    group.add(&enable_row);
    group.add(&engine_row);
    group.add(&url_row);
    group
}

fn build_web_search_header() -> (adw::PreferencesGroup, adw::SwitchRow) {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Web Search"))
        .description(gettext(
            "Disabled by default for privacy. When enabled, you can toggle Web Search per chat and review every query or URL before it is sent.",
        ))
        .build();
    let enable_row = adw::SwitchRow::builder()
        .title(gettext("Enable Web Search"))
        .subtitle(gettext(
            "Allow chats to toggle Web Search and fetch approved HTTPS pages",
        ))
        .active(false)
        .build();
    (group, enable_row)
}

fn wire_web_search_enable_row(
    enable_row: &adw::SwitchRow,
    engine_row: &adw::ComboRow,
    url_row: &adw::EntryRow,
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    syncing: &Rc<Cell<bool>>,
    callbacks: (&Rc<impl Fn(bool) + 'static>, Rc<impl Fn(bool) + 'static>),
) {
    let (on_tools, on_global_changed) = callbacks;
    let (db_toggle, rt_toggle, engine_ref, url_ref, syncing_toggle, on_tools_toggle) = (
        db.clone(),
        rt_handle.clone(),
        engine_row.clone(),
        url_row.clone(),
        syncing.clone(),
        on_tools.clone(),
    );
    enable_row.connect_active_notify(move |row| {
        let active = row.is_active();
        engine_ref.set_sensitive(active);
        url_ref.set_sensitive(active);
        on_tools_toggle(active);
        if syncing_toggle.get() {
            return;
        }
        let db = db_toggle.clone();
        rt_toggle.spawn_blocking(move || {
            if let Err(err) = db.set_web_search_global_enabled(active) {
                tracing::error!("Failed to save web search toggle: {err}");
            }
        });
        on_global_changed(active);
    });
}

fn load_initial_web_search_state(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    enable_row: &adw::SwitchRow,
    engine_row: &adw::ComboRow,
    url_row: &adw::EntryRow,
    syncing: Rc<Cell<bool>>,
    on_tools: Rc<impl Fn(bool) + 'static>,
) {
    let (db_init, enable_init, engine_init, url_init) = (
        db.clone(),
        enable_row.clone(),
        engine_row.clone(),
        url_row.clone(),
    );
    spawn_blocking_ui(
        rt_handle,
        move || {
            (
                db_init.get_web_search_global_enabled().unwrap_or(false),
                db_init.get_search_engine().unwrap_or_default(),
                db_init.get_searxng_url().ok().flatten().unwrap_or_default(),
            )
        },
        move |(enabled, engine, url)| {
            syncing.set(true);
            enable_init.set_active(enabled);
            engine_init.set_sensitive(enabled);
            engine_init.set_selected(match engine {
                SearchEngine::DuckDuckGo => 0,
                SearchEngine::SearXNG => 1,
            });
            url_init.set_text(&url);
            url_init.set_visible(engine == SearchEngine::SearXNG);
            url_init.set_sensitive(enabled);
            on_tools(enabled);
            syncing.set(false);
        },
    );
}

fn build_engine_row(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    url_row: &adw::EntryRow,
    syncing: &Rc<Cell<bool>>,
) -> adw::ComboRow {
    let engines = gtk::StringList::new(&[&gettext("DuckDuckGo"), &gettext("SearXNG (Custom URL)")]);
    let engine_row = adw::ComboRow::builder()
        .title(gettext("Search Engine"))
        .model(&engines)
        .selected(0)
        .sensitive(false)
        .build();
    let db = db.clone();
    let rt = rt_handle.clone();
    let url_row_ref = url_row.clone();
    let syncing = syncing.clone();
    engine_row.connect_selected_notify(move |row| {
        let engine = match row.selected() {
            1 => SearchEngine::SearXNG,
            _ => SearchEngine::DuckDuckGo,
        };
        url_row_ref.set_visible(engine == SearchEngine::SearXNG);
        if syncing.get() {
            return;
        }
        let db = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db.set_search_engine(engine) {
                tracing::error!("Failed to save search engine: {err}");
            }
        });
    });
    engine_row
}

fn is_valid_searxng_url(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    !host.is_empty() && !host.contains(char::is_whitespace)
}

fn build_searxng_url_row(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    syncing: &Rc<Cell<bool>>,
) -> adw::EntryRow {
    let url_row = adw::EntryRow::builder()
        .title(gettext("SearXNG Instance URL"))
        .visible(false)
        .sensitive(false)
        .build();
    let db = db.clone();
    let rt = rt_handle.clone();
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let commit_url = Rc::new(move |row: &adw::EntryRow| {
        let text = row.text().to_string();
        if !is_valid_searxng_url(&text) {
            row.add_css_class("error");
            return;
        }
        row.remove_css_class("error");
        let db = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db.set_searxng_url(&text) {
                tracing::error!("Failed to save SearXNG URL: {err}");
            }
        });
    });

    wire_searxng_url_signals(&url_row, syncing, debounce, commit_url);
    url_row
}

fn wire_searxng_url_signals(
    url_row: &adw::EntryRow,
    syncing: &Rc<Cell<bool>>,
    debounce: Rc<RefCell<Option<glib::SourceId>>>,
    commit_url: Rc<dyn Fn(&adw::EntryRow)>,
) {
    let (c_changed, deb_changed, syncing_changed) =
        (commit_url.clone(), debounce.clone(), syncing.clone());
    url_row.connect_changed(move |row| {
        if syncing_changed.get() {
            return;
        }
        if is_valid_searxng_url(row.text().as_str()) {
            row.remove_css_class("error");
        } else {
            row.add_css_class("error");
        }
        if let Some(src) = deb_changed.borrow_mut().take() {
            src.remove();
        }
        let (row, commit, deb_done) = (row.clone(), c_changed.clone(), deb_changed.clone());
        *deb_changed.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(300),
            move || {
                *deb_done.borrow_mut() = None;
                commit(&row);
            },
        ));
    });
    let (c_act, deb_act) = (commit_url.clone(), debounce.clone());
    url_row.connect_entry_activated(move |row| {
        if let Some(src) = deb_act.borrow_mut().take() {
            src.remove();
        }
        c_act(row);
    });
    let focus = gtk::EventControllerFocus::new();
    let row_ref = url_row.clone();
    focus.connect_leave(move |_| {
        if let Some(src) = debounce.borrow_mut().take() {
            src.remove();
        }
        commit_url(&row_ref);
    });
    url_row.add_controller(focus);
}
