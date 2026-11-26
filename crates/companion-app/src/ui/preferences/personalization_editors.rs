//! Debounced text editors and preset behavior preview helpers for Personalization preferences.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::core::AssistantPersonality;
use crate::storage::Database;

#[derive(Clone)]
pub(super) struct PersonalizationCache {
    pub(super) assistant_name: Rc<RefCell<String>>,
    pub(super) user_name: Rc<RefCell<Option<String>>>,
    pub(super) personality: Rc<Cell<AssistantPersonality>>,
    pub(super) custom_personality: Rc<RefCell<Option<String>>>,
    pub(super) last_preset: Rc<Cell<AssistantPersonality>>,
    pub(super) syncing: Rc<Cell<bool>>,
}

pub(super) fn make_behavior_refresher(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    behavior_view: &gtk::TextView,
    behavior_buf: &gtk::TextBuffer,
    cache: &PersonalizationCache,
) -> Rc<dyn Fn(AssistantPersonality)> {
    let db = db.clone();
    let rt = rt_handle.clone();
    let view = behavior_view.clone();
    let buf = behavior_buf.clone();
    let cache = cache.clone();
    Rc::new(move |personality| {
        let was_syncing = cache.syncing.replace(true);
        let assistant = cache.assistant_name.borrow().clone();
        let user = cache.user_name.borrow().clone();
        if personality == AssistantPersonality::Custom {
            let text = cache
                .custom_personality
                .borrow()
                .clone()
                .unwrap_or_else(|| {
                    let seed = cache
                        .last_preset
                        .get()
                        .prompt_guidance("{assistant_name}", user.as_ref().map(|_| "{user_name}"));
                    *cache.custom_personality.borrow_mut() = Some(seed.clone());
                    let db_save = db.clone();
                    let seed_save = seed.clone();
                    rt.spawn_blocking(move || {
                        if let Err(err) = db_save.set_custom_personality(&seed_save) {
                            tracing::error!("Failed to save initial custom personality: {err}");
                        }
                    });
                    seed
                });
            buf.set_text(&text);
            view.set_editable(true);
            view.set_sensitive(true);
        } else {
            cache.last_preset.set(personality);
            buf.set_text(&personality.prompt_guidance(&assistant, user.as_deref()));
            view.set_editable(false);
            view.set_sensitive(false);
        }
        cache.syncing.set(was_syncing);
    })
}

pub(super) fn schedule_debounced<F: FnOnce() + 'static>(
    slot: &Rc<RefCell<Option<glib::SourceId>>>,
    f: F,
) {
    if let Some(src) = slot.borrow_mut().take() {
        src.remove();
    }
    let slot_done = slot.clone();
    let src = glib::timeout_add_local_once(Duration::from_millis(300), move || {
        *slot_done.borrow_mut() = None;
        f();
    });
    *slot.borrow_mut() = Some(src);
}

pub(super) fn wire_behavior_editor(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    view: &gtk::TextView,
    buf: &gtk::TextBuffer,
    cache: &PersonalizationCache,
) {
    let db = db.clone();
    let rt = rt_handle.clone();
    let cache = cache.clone();
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let commit = Rc::new(move |b: &gtk::TextBuffer| {
        if cache.personality.get() == AssistantPersonality::Custom {
            let (s, e) = b.bounds();
            let text = b.text(&s, &e, false).to_string();
            *cache.custom_personality.borrow_mut() = Some(text.clone());
            let db_save = db.clone();
            rt.spawn_blocking(move || {
                if let Err(err) = db_save.set_custom_personality(&text) {
                    tracing::error!("Failed to save custom personality: {err}");
                }
            });
        }
    });
    let c1 = commit.clone();
    let deb1 = debounce.clone();
    let syncing = cache.syncing.clone();
    buf.connect_changed(move |b| {
        if syncing.get() {
            return;
        }
        let b = b.clone();
        let c = c1.clone();
        schedule_debounced(&deb1, move || c(&b));
    });
    let focus = gtk::EventControllerFocus::new();
    let buf_f = buf.clone();
    focus.connect_leave(move |_| {
        if let Some(s) = debounce.borrow_mut().take() {
            s.remove();
        }
        commit(&buf_f);
    });
    view.add_controller(focus);
}

pub(super) fn wire_about_editor(
    db: &Database,
    rt_handle: &tokio::runtime::Handle,
    view: &gtk::TextView,
    buf: &gtk::TextBuffer,
    syncing: &Rc<Cell<bool>>,
) {
    let db = db.clone();
    let rt = rt_handle.clone();
    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let commit = Rc::new(move |b: &gtk::TextBuffer| {
        let (s, e) = b.bounds();
        let text = b.text(&s, &e, false).to_string();
        let db_save = db.clone();
        rt.spawn_blocking(move || {
            if let Err(err) = db_save.set_user_about(&text) {
                tracing::error!("Failed to save user about text: {err}");
            }
        });
    });
    let c1 = commit.clone();
    let deb1 = debounce.clone();
    let syncing = syncing.clone();
    buf.connect_changed(move |b| {
        if syncing.get() {
            return;
        }
        let b = b.clone();
        let c = c1.clone();
        schedule_debounced(&deb1, move || c(&b));
    });
    let focus = gtk::EventControllerFocus::new();
    let buf_f = buf.clone();
    focus.connect_leave(move |_| {
        if let Some(s) = debounce.borrow_mut().take() {
            s.remove();
        }
        commit(&buf_f);
    });
    view.add_controller(focus);
}
