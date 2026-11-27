//! State synchronization message handlers and text-view wiring for [`ComposerModel`].

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::ui::mention_picker::MentionPicker;

use super::input_helpers::{attach_clipboard_paste_handler, attach_dynamic_height_scaler};
use super::{ComposerModel, ComposerMsg};

impl ComposerModel {
    pub(super) fn handle_state_sync_msg(
        &mut self,
        msg: ComposerMsg,
        sender: &ComponentSender<Self>,
    ) {
        match msg {
            ComposerMsg::InitDataLoaded {
                assistant_name,
                web_search_global_enabled,
                search_engine,
                models,
            } => self.handle_init_data_loaded(
                assistant_name,
                web_search_global_enabled,
                search_engine,
                models,
            ),
            ComposerMsg::ModelsMetadataLoaded(models) => {
                self.models = models;
                self.refresh_controls_button();
                self.refresh_attach_button();
            }
            ComposerMsg::SearchEngineLoaded(engine) => {
                self.search_engine = engine;
                self.refresh_globe_button();
            }
            ComposerMsg::RefreshModels(active_id) => {
                self.active_model_id = active_id;
                self.refresh_models_cache(sender);
                self.refresh_controls_button();
                self.refresh_attach_button();
            }
            ComposerMsg::SetSystemPrompt(prompt) => self.system_prompt = prompt,
            ComposerMsg::SetThinkingLevel(level) => {
                self.thinking_level = level;
                self.refresh_controls_button();
            }
            ComposerMsg::SetAssistantName(name) => {
                self.assistant_name = name;
                self.refresh_controls_button();
            }
            ComposerMsg::SetContextUsed(tokens) => {
                self.context_used_tokens = tokens;
                self.refresh_controls_button();
            }
            ComposerMsg::SetGenerating(generating) => self.is_generating = generating,
            _ => {}
        }
    }

    pub(super) fn refresh_all_toolbar_buttons(&self, sender: &ComponentSender<Self>) {
        self.refresh_folders_button();
        self.refresh_globe_button();
        self.refresh_controls_button();
        self.refresh_attach_button();
        self.spawn_init_data_load(sender);
    }
}

pub(super) fn build_composer_widgets() -> (
    gtk::TextView,
    gtk::Box,
    gtk::Button,
    gtk::Button,
    gtk::Button,
    gtk::Button,
) {
    (
        gtk::TextView::new(),
        gtk::Box::new(gtk::Orientation::Horizontal, 8),
        gtk::Button::new(),
        gtk::Button::new(),
        gtk::Button::new(),
        gtk::Button::new(),
    )
}

pub(super) fn wire_composer_text_view(
    text_view: &gtk::TextView,
    placeholder: &gtk::Label,
    mention_picker: MentionPicker,
    clipboard_dir: PathBuf,
    rt_handle: tokio::runtime::Handle,
    sender: &ComponentSender<ComposerModel>,
) {
    text_view.update_property(&[gtk::accessible::Property::Label(&crate::i18n::gettext(
        "Message input",
    ))]);
    text_view.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        placeholder.upcast_ref()
    ])]);
    let placeholder = placeholder.clone();
    let relayout_queued = Rc::new(Cell::new(false));
    let tv_weak = text_view.downgrade();
    text_view.buffer().connect_changed(move |buf| {
        placeholder.set_visible(buf.char_count() == 0);
        mention_picker.update_from_buffer();
        schedule_text_view_relayout(&tv_weak, &relayout_queued);
    });
    attach_clipboard_paste_handler(text_view, clipboard_dir, rt_handle, sender);
    attach_dynamic_height_scaler(text_view);
}

fn schedule_text_view_relayout(
    tv_weak: &glib::WeakRef<gtk::TextView>,
    relayout_queued: &Rc<Cell<bool>>,
) {
    if relayout_queued.get() {
        return;
    }
    relayout_queued.set(true);
    let tv_weak = tv_weak.clone();
    let queued = relayout_queued.clone();
    glib::idle_add_local_once(move || {
        queued.set(false);
        let Some(tv) = tv_weak.upgrade() else { return };
        let end_iter = tv.buffer().end_iter();
        let _ = tv.iter_location(&end_iter);
        if let Some(scroller) = tv.parent().and_downcast::<gtk::ScrolledWindow>() {
            let cap = scroller.max_content_height();
            if cap > 1 {
                scroller.set_max_content_height(cap - 1);
                scroller.set_max_content_height(cap);
            }
            scroller.queue_resize();
            let vadj = scroller.vadjustment();
            glib::idle_add_local_once(move || {
                let max_val = (vadj.upper() - vadj.page_size()).max(0.0);
                if vadj.value() > max_val {
                    vadj.set_value(max_val);
                }
                if let Some(tv) = tv_weak.upgrade() {
                    tv.scroll_mark_onscreen(&tv.buffer().get_insert());
                }
            });
        }
    });
}
