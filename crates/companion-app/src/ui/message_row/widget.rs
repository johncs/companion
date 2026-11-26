//! Safe GObject subclass owning recycled message-row widgets and signal lifetimes.

use std::cell::{OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::ui::message_object::MessageObject;

use super::RowWidgets;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageRowWidget {
        pub(super) widgets: OnceCell<RowWidgets>,
        pub(super) bound_signal: RefCell<Option<(MessageObject, glib::SignalHandlerId)>>,
        pub(super) theme_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageRowWidget {
        const NAME: &'static str = "CompanionMessageRowWidget";
        type Type = super::MessageRowWidget;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for MessageRowWidget {
        fn dispose(&self) {
            if let Some((prev_obj, prev_id)) = self.bound_signal.borrow_mut().take() {
                prev_obj.disconnect(prev_id);
            }
            if let Some(theme_id) = self.theme_handler.borrow_mut().take() {
                adw::StyleManager::default().disconnect(theme_id);
            }
        }
    }

    impl WidgetImpl for MessageRowWidget {}
    impl BinImpl for MessageRowWidget {}
}

glib::wrapper! {
    pub struct MessageRowWidget(ObjectSubclass<imp::MessageRowWidget>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MessageRowWidget {
    pub(super) fn new(
        clamp: &adw::Clamp,
        widgets: RowWidgets,
        theme_handler: glib::SignalHandlerId,
    ) -> Self {
        let obj: Self = glib::Object::new();
        obj.set_child(Some(clamp));
        let _ = obj.imp().widgets.set(widgets);
        *obj.imp().theme_handler.borrow_mut() = Some(theme_handler);
        obj
    }

    pub(super) fn row_widgets(&self) -> Option<&RowWidgets> {
        self.imp().widgets.get()
    }

    pub(super) fn bind_signal(&self, obj: MessageObject, id: glib::SignalHandlerId) {
        self.unbind_signal();
        *self.imp().bound_signal.borrow_mut() = Some((obj, id));
    }

    pub(super) fn unbind_signal(&self) {
        if let Some((prev_obj, prev_id)) = self.imp().bound_signal.borrow_mut().take() {
            prev_obj.disconnect(prev_id);
        }
    }
}
