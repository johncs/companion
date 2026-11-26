//! Companion identity, personality preset, and "About You" preferences groups.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::core::AssistantPersonality;
use crate::i18n::gettext;
use crate::storage::Database;

pub(super) fn build_personalization_groups<P>(
    db: &Database,
    on_personalization_changed: P,
) -> (
    adw::PreferencesGroup,
    adw::PreferencesGroup,
    adw::PreferencesGroup,
)
where
    P: Fn(String, Option<String>) + 'static,
{
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

    let behavior_view = build_multiline_text_view();
    let behavior_buf = behavior_view.buffer();
    let syncing_behavior = Rc::new(Cell::new(false));
    let last_preset = Rc::new(Cell::new({
        let cur = db.get_assistant_personality().unwrap_or_default();
        if cur == AssistantPersonality::Custom {
            AssistantPersonality::Friendly
        } else {
            cur
        }
    }));

    let refresh_behavior_view = make_behavior_refresher(
        db,
        &behavior_view,
        &behavior_buf,
        &syncing_behavior,
        &last_preset,
    );

    let on_changed: Rc<dyn Fn(String, Option<String>)> = Rc::new(on_personalization_changed);
    let assistant_row = build_assistant_name_row(db, &refresh_behavior_view, &on_changed);
    let user_row = build_user_name_row(db, &refresh_behavior_view, &on_changed);
    let personality_row = build_personality_row(db, &refresh_behavior_view);

    wire_behavior_editor(db, &behavior_view, &behavior_buf, &syncing_behavior);
    let behavior_scroller = wrap_text_view_in_card(&behavior_view, 0);
    behavior_group.add(&behavior_scroller);

    let about_view = build_multiline_text_view();
    let about_buf = about_view.buffer();
    about_buf.set_text(&db.get_user_about().ok().flatten().unwrap_or_default());
    wire_about_editor(db, &about_view, &about_buf);
    let about_scroller = wrap_text_view_in_card(&about_view, 8);

    identity_group.add(&assistant_row);
    identity_group.add(&personality_row);
    about_group.add(&user_row);
    about_group.add(&about_scroller);
    (identity_group, behavior_group, about_group)
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

fn make_behavior_refresher(
    db: &Database,
    behavior_view: &gtk::TextView,
    behavior_buf: &gtk::TextBuffer,
    syncing_behavior: &Rc<Cell<bool>>,
    last_preset: &Rc<Cell<AssistantPersonality>>,
) -> Rc<dyn Fn(AssistantPersonality)> {
    let db = db.clone();
    let view = behavior_view.clone();
    let buf = behavior_buf.clone();
    let syncing = syncing_behavior.clone();
    let last_preset = last_preset.clone();
    Rc::new(move |personality| {
        let assistant = db
            .get_assistant_name()
            .unwrap_or_else(|_| "Companion".to_string());
        let user = db.get_user_name().ok().flatten();
        syncing.set(true);
        if personality == AssistantPersonality::Custom {
            let text = db
                .get_custom_personality()
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    let seed = last_preset
                        .get()
                        .prompt_guidance("{assistant_name}", user.as_ref().map(|_| "{user_name}"));
                    let _ = db.set_custom_personality(&seed);
                    seed
                });
            buf.set_text(&text);
            view.set_editable(true);
            view.set_sensitive(true);
        } else {
            last_preset.set(personality);
            let text = personality.prompt_guidance(&assistant, user.as_deref());
            buf.set_text(&text);
            view.set_editable(false);
            view.set_sensitive(false);
        }
        syncing.set(false);
    })
}

fn build_assistant_name_row(
    db: &Database,
    refresh_behavior_view: &Rc<dyn Fn(AssistantPersonality)>,
    on_changed: &Rc<dyn Fn(String, Option<String>)>,
) -> adw::EntryRow {
    let current_assistant = db
        .get_assistant_name()
        .unwrap_or_else(|_| "Companion".to_string());
    let assistant_row = adw::EntryRow::builder()
        .title(gettext("Companion Name"))
        .text(&current_assistant)
        .build();

    let db = db.clone();
    let on_changed = on_changed.clone();
    let refresh_behavior = refresh_behavior_view.clone();
    let commit = Rc::new(move |row: &adw::EntryRow, normalize_empty: bool| {
        let text = row.text().to_string();
        let _ = db.set_assistant_name(&text);
        let trimmed = text.trim();
        let effective = if trimmed.is_empty() {
            if normalize_empty && text != "Companion" {
                row.set_text("Companion");
            }
            "Companion".to_string()
        } else {
            trimmed.to_string()
        };
        let user = db.get_user_name().ok().flatten();
        let cur_p = db.get_assistant_personality().unwrap_or_default();
        if cur_p != AssistantPersonality::Custom {
            refresh_behavior(cur_p);
        }
        on_changed(effective, user);
    });

    let c1 = commit.clone();
    assistant_row.connect_changed(move |row| c1(row, false));
    let c2 = commit.clone();
    assistant_row.connect_entry_activated(move |row| c2(row, true));
    let focus = gtk::EventControllerFocus::new();
    let row_ref = assistant_row.clone();
    focus.connect_leave(move |_| commit(&row_ref, true));
    assistant_row.add_controller(focus);
    assistant_row
}

fn build_user_name_row(
    db: &Database,
    refresh_behavior_view: &Rc<dyn Fn(AssistantPersonality)>,
    on_changed: &Rc<dyn Fn(String, Option<String>)>,
) -> adw::EntryRow {
    let current_user = db.get_user_name().ok().flatten().unwrap_or_default();
    let user_row = adw::EntryRow::builder()
        .title(gettext("Your Name"))
        .text(&current_user)
        .build();

    let db = db.clone();
    let on_changed = on_changed.clone();
    let refresh_behavior = refresh_behavior_view.clone();
    let commit = Rc::new(move |row: &adw::EntryRow| {
        let raw = row.text().to_string();
        let _ = db.set_user_name(&raw);
        let assistant = db
            .get_assistant_name()
            .unwrap_or_else(|_| "Companion".to_string());
        let trimmed = raw.trim();
        let user_opt = (!trimmed.is_empty()).then(|| trimmed.to_string());
        let cur_p = db.get_assistant_personality().unwrap_or_default();
        if cur_p != AssistantPersonality::Custom {
            refresh_behavior(cur_p);
        }
        on_changed(assistant, user_opt);
    });

    let c1 = commit.clone();
    user_row.connect_changed(move |row| c1(row));
    let c2 = commit.clone();
    user_row.connect_entry_activated(move |row| c2(row));
    let focus = gtk::EventControllerFocus::new();
    let row_ref = user_row.clone();
    focus.connect_leave(move |_| commit(&row_ref));
    user_row.add_controller(focus);
    user_row
}

fn build_personality_row(
    db: &Database,
    refresh_behavior_view: &Rc<dyn Fn(AssistantPersonality)>,
) -> adw::ComboRow {
    let current_personality = db.get_assistant_personality().unwrap_or_default();
    let personality_labels: Vec<String> = AssistantPersonality::ALL
        .iter()
        .map(|p| p.label())
        .collect();
    let personality_refs: Vec<&str> = personality_labels.iter().map(String::as_str).collect();
    let personality_row = adw::ComboRow::builder()
        .title(gettext("Personality"))
        .subtitle(gettext("Tone and style used across all chats"))
        .model(&gtk::StringList::new(&personality_refs))
        .build();
    if let Some(idx) = AssistantPersonality::ALL
        .iter()
        .position(|p| *p == current_personality)
    {
        personality_row.set_selected(idx as u32);
    }
    refresh_behavior_view(current_personality);

    let db = db.clone();
    let refresh_behavior = refresh_behavior_view.clone();
    personality_row.connect_selected_notify(move |row| {
        let selected = AssistantPersonality::from_index(row.selected());
        let _ = db.set_assistant_personality(selected);
        refresh_behavior(selected);
    });
    personality_row
}

fn wire_behavior_editor(
    db: &Database,
    behavior_view: &gtk::TextView,
    behavior_buf: &gtk::TextBuffer,
    syncing_behavior: &Rc<Cell<bool>>,
) {
    let db_changed = db.clone();
    let db_focus = db.clone();
    let syncing = syncing_behavior.clone();
    let buf_ref = RefCell::new(behavior_buf.clone());
    behavior_buf.connect_changed(move |buf| {
        if syncing.get() {
            return;
        }
        if db_changed.get_assistant_personality().unwrap_or_default()
            == AssistantPersonality::Custom
        {
            let (start, end) = buf.bounds();
            let _ = db_changed.set_custom_personality(&buf.text(&start, &end, false));
        }
    });
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| {
        if db_focus.get_assistant_personality().unwrap_or_default() == AssistantPersonality::Custom
        {
            let buf = buf_ref.borrow();
            let (start, end) = buf.bounds();
            let _ = db_focus.set_custom_personality(&buf.text(&start, &end, false));
        }
    });
    behavior_view.add_controller(focus);
}

fn wire_about_editor(db: &Database, about_view: &gtk::TextView, about_buf: &gtk::TextBuffer) {
    let db_changed = db.clone();
    let db_focus = db.clone();
    let buf_ref = RefCell::new(about_buf.clone());
    about_buf.connect_changed(move |buf| {
        let (start, end) = buf.bounds();
        let _ = db_changed.set_user_about(&buf.text(&start, &end, false));
    });
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| {
        let buf = buf_ref.borrow();
        let (start, end) = buf.bounds();
        let _ = db_focus.set_user_about(&buf.text(&start, &end, false));
    });
    about_view.add_controller(focus);
}
