//! Companion identity, personality preset, and "About You" preferences groups.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::core::AssistantPersonality;
use crate::i18n::gettext;
use crate::storage::Database;
use crate::ui::widgets::spawn_blocking_ui;

use super::personalization_editors::{
    PersonalizationCache, make_behavior_refresher, schedule_debounced, wire_about_editor,
    wire_behavior_editor,
};

pub(super) fn build_personalization_groups<P>(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    on_personalization_changed: P,
) -> (
    adw::PreferencesGroup,
    adw::PreferencesGroup,
    adw::PreferencesGroup,
)
where
    P: Fn(String, Option<String>) + 'static,
{
    let (identity_group, behavior_group, about_group) = build_personalization_section_groups();
    let cache = default_personalization_cache();
    let behavior_view = build_multiline_text_view();
    let behavior_buf = behavior_view.buffer();
    let refresh_behavior_view =
        make_behavior_refresher(db, rt_handle, &behavior_view, &behavior_buf, &cache);
    let on_changed: Rc<dyn Fn(String, Option<String>)> = Rc::new(on_personalization_changed);

    let assistant_row =
        build_assistant_name_row(db, rt_handle, &cache, &refresh_behavior_view, &on_changed);
    let user_row = build_user_name_row(db, rt_handle, &cache, &refresh_behavior_view, &on_changed);
    let personality_row = build_personality_row(db, rt_handle, &cache, &refresh_behavior_view);
    wire_behavior_editor(db, rt_handle, &behavior_view, &behavior_buf, &cache);
    behavior_group.add(&wrap_text_view_in_card(&behavior_view, 0));

    let about_view = build_multiline_text_view();
    let about_buf = about_view.buffer();
    wire_about_editor(db, rt_handle, &about_view, &about_buf, &cache.syncing);
    identity_group.add(&assistant_row);
    identity_group.add(&personality_row);
    about_group.add(&user_row);
    about_group.add(&wrap_text_view_in_card(&about_view, 8));

    spawn_initial_load(
        db,
        rt_handle,
        cache,
        InitialLoadTargets {
            assistant_row,
            user_row,
            personality_row,
            about_buf,
            refresh_behavior: refresh_behavior_view,
        },
    );
    (identity_group, behavior_group, about_group)
}

fn default_personalization_cache() -> PersonalizationCache {
    PersonalizationCache {
        assistant_name: Rc::new(RefCell::new("Companion".to_string())),
        user_name: Rc::new(RefCell::new(None)),
        personality: Rc::new(Cell::new(AssistantPersonality::default())),
        custom_personality: Rc::new(RefCell::new(None)),
        last_preset: Rc::new(Cell::new(AssistantPersonality::Friendly)),
        syncing: Rc::new(Cell::new(true)),
    }
}

fn build_personalization_section_groups() -> (
    adw::PreferencesGroup,
    adw::PreferencesGroup,
    adw::PreferencesGroup,
) {
    let identity_group = adw::PreferencesGroup::builder()
        .title(gettext("Companion Identity"))
        .description(gettext(
            "Give your companion a name and choose its character.",
        ))
        .build();
    let behavior_group = adw::PreferencesGroup::builder()
        .title(gettext("Character Behavior"))
        .description(gettext(
            "Preview the active preset instructions, or choose Custom to write your own. Supports {assistant_name} and {user_name}.",
        ))
        .build();
    let about_group = adw::PreferencesGroup::builder()
        .title(gettext("About You"))
        .description(gettext(
            "Share your name and optional background so your companion remembers it across chats.",
        ))
        .build();
    (identity_group, behavior_group, about_group)
}

struct InitialLoadTargets {
    assistant_row: adw::EntryRow,
    user_row: adw::EntryRow,
    personality_row: adw::ComboRow,
    about_buf: gtk::TextBuffer,
    refresh_behavior: Rc<dyn Fn(AssistantPersonality)>,
}

fn spawn_initial_load(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    cache: PersonalizationCache,
    targets: InitialLoadTargets,
) {
    let db = db.clone();
    spawn_blocking_ui(
        rt_handle,
        move || {
            let settings = db.get_personalization_settings().ok();
            let custom = db.get_custom_personality().ok().flatten();
            (settings, custom)
        },
        move |(settings, custom)| {
            cache.syncing.set(true);
            if let Some(s) = settings {
                *cache.assistant_name.borrow_mut() = s.assistant_name.clone();
                *cache.user_name.borrow_mut() = s.user_name.clone();
                cache.personality.set(s.personality);
                if s.personality != AssistantPersonality::Custom {
                    cache.last_preset.set(s.personality);
                }
                *cache.custom_personality.borrow_mut() = custom;
                targets.assistant_row.set_text(&s.assistant_name);
                targets
                    .user_row
                    .set_text(s.user_name.as_deref().unwrap_or(""));
                if let Some(idx) = AssistantPersonality::ALL
                    .iter()
                    .position(|p| *p == s.personality)
                {
                    targets.personality_row.set_selected(idx as u32);
                }
                targets
                    .about_buf
                    .set_text(s.user_about.as_deref().unwrap_or(""));
                (targets.refresh_behavior)(s.personality);
            }
            cache.syncing.set(false);
        },
    );
}

fn build_multiline_text_view() -> gtk::TextView {
    gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .build()
}

fn wrap_text_view_in_card(view: &gtk::TextView, margin_top: i32) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .min_content_height(100)
        .max_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .overflow(gtk::Overflow::Hidden)
        .margin_top(margin_top)
        .css_classes(["card"])
        .child(view)
        .build()
}

fn build_assistant_name_row(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    cache: &PersonalizationCache,
    refresh_behavior: &Rc<dyn Fn(AssistantPersonality)>,
    on_changed: &Rc<dyn Fn(String, Option<String>)>,
) -> adw::EntryRow {
    let assistant_row = adw::EntryRow::builder()
        .title(gettext("Companion Name"))
        .build();
    let (db, rt, cache, on_changed, refresh_behavior) = (
        db.clone(),
        rt_handle.clone(),
        cache.clone(),
        on_changed.clone(),
        refresh_behavior.clone(),
    );
    let commit = Rc::new(move |row: &adw::EntryRow, normalize_empty: bool| {
        let raw = row.text().to_string();
        let trimmed = raw.trim();
        let effective = if trimmed.is_empty() {
            if normalize_empty && raw != "Companion" {
                row.set_text("Companion");
            }
            "Companion".to_string()
        } else {
            trimmed.to_string()
        };
        *cache.assistant_name.borrow_mut() = effective.clone();
        let db_save = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db_save.set_assistant_name(&raw) {
                tracing::error!("Failed to save assistant name: {err}");
            }
        });
        if cache.personality.get() != AssistantPersonality::Custom {
            refresh_behavior(cache.personality.get());
        }
        on_changed(effective, cache.user_name.borrow().clone());
    });
    wire_debounced_entry_row(&assistant_row, &cache.syncing, commit);
    assistant_row
}

fn build_user_name_row(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    cache: &PersonalizationCache,
    refresh_behavior: &Rc<dyn Fn(AssistantPersonality)>,
    on_changed: &Rc<dyn Fn(String, Option<String>)>,
) -> adw::EntryRow {
    let user_row = adw::EntryRow::builder().title(gettext("Your Name")).build();
    let (db, rt, cache, on_changed, refresh_behavior) = (
        db.clone(),
        rt_handle.clone(),
        cache.clone(),
        on_changed.clone(),
        refresh_behavior.clone(),
    );
    let commit = Rc::new(move |row: &adw::EntryRow, _normalize: bool| {
        let raw = row.text().to_string();
        let trimmed = raw.trim();
        let user_opt = (!trimmed.is_empty()).then(|| trimmed.to_string());
        *cache.user_name.borrow_mut() = user_opt.clone();
        let db_save = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db_save.set_user_name(&raw) {
                tracing::error!("Failed to save user name: {err}");
            }
        });
        if cache.personality.get() != AssistantPersonality::Custom {
            refresh_behavior(cache.personality.get());
        }
        on_changed(cache.assistant_name.borrow().clone(), user_opt);
    });
    wire_debounced_entry_row(&user_row, &cache.syncing, commit);
    user_row
}

type EntryCommitCb = Rc<dyn Fn(&adw::EntryRow, bool)>;

fn wire_debounced_entry_row(
    entry_row: &adw::EntryRow,
    syncing: &Rc<Cell<bool>>,
    commit: EntryCommitCb,
) {
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let (c1, deb1, syncing1) = (commit.clone(), debounce.clone(), syncing.clone());
    entry_row.connect_changed(move |row| {
        if syncing1.get() {
            return;
        }
        let (row, c) = (row.clone(), c1.clone());
        schedule_debounced(&deb1, move || c(&row, false));
    });
    let (c2, deb2) = (commit.clone(), debounce.clone());
    entry_row.connect_entry_activated(move |row| {
        if let Some(s) = deb2.borrow_mut().take() {
            s.remove();
        }
        c2(row, true);
    });
    let focus = gtk::EventControllerFocus::new();
    let row_ref = entry_row.clone();
    focus.connect_leave(move |_| {
        if let Some(s) = debounce.borrow_mut().take() {
            s.remove();
        }
        commit(&row_ref, true);
    });
    entry_row.add_controller(focus);
}

fn build_personality_row(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    cache: &PersonalizationCache,
    refresh_behavior: &Rc<dyn Fn(AssistantPersonality)>,
) -> adw::ComboRow {
    let labels: Vec<String> = AssistantPersonality::ALL
        .iter()
        .map(|p| p.label())
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(gettext("Personality"))
        .subtitle(gettext("Tone and style used across all chats"))
        .model(&gtk::StringList::new(&refs))
        .build();
    let db = db.clone();
    let rt = rt_handle.clone();
    let cache = cache.clone();
    let refresh = refresh_behavior.clone();
    row.connect_selected_notify(move |r| {
        if cache.syncing.get() {
            return;
        }
        let selected = AssistantPersonality::from_index(r.selected());
        cache.personality.set(selected);
        let db_save = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db_save.set_assistant_personality(selected) {
                tracing::error!("Failed to save assistant personality: {err}");
            }
        });
        refresh(selected);
    });
    row
}
