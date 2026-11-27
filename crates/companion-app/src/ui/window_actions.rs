//! GNOME window actions, keyboard accelerators, Shortcuts dialog, and `AdwAboutDialog`.

use adw::prelude::*;
use gtk::{gdk, gio};
use relm4::{ComponentController, ComponentSender};

use crate::i18n::gettext;

use super::window::{MainWindow, MainWindowMsg};

pub(super) fn register_window_actions(
    root: &adw::ApplicationWindow,
    sender: &ComponentSender<MainWindow>,
) {
    add_window_action(root, "new-chat", sender, || MainWindowMsg::NewChat);
    add_window_action(root, "search-chats", sender, || MainWindowMsg::FocusSearch);
    add_window_action(root, "preferences", sender, || {
        MainWindowMsg::OpenPreferences
    });

    let about_act = gio::SimpleAction::new("about", None);
    let win_about = root.clone();
    about_act.connect_activate(move |_, _| present_about_dialog(&win_about));
    root.add_action(&about_act);

    let shortcuts_act = gio::SimpleAction::new("show-help-overlay", None);
    let win_shortcuts = root.clone();
    shortcuts_act.connect_activate(move |_, _| present_shortcuts_dialog(&win_shortcuts));
    root.add_action(&shortcuts_act);

    wire_accelerators(root);
    wire_escape_stop_generation(root, sender);
}

fn add_window_action(
    root: &adw::ApplicationWindow,
    name: &str,
    sender: &ComponentSender<MainWindow>,
    make_msg: impl Fn() -> MainWindowMsg + 'static,
) {
    let act = gio::SimpleAction::new(name, None);
    let input_sender = sender.input_sender().clone();
    act.connect_activate(move |_, _| {
        let _ = input_sender.send(make_msg());
    });
    root.add_action(&act);
}

fn wire_accelerators(root: &adw::ApplicationWindow) {
    let apply = |app: &gtk::Application| {
        app.set_accels_for_action("win.new-chat", &["<Control>n"]);
        app.set_accels_for_action("win.search-chats", &["<Control>f"]);
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.show-help-overlay", &["<Control>question"]);
    };
    if let Some(app) = root.application() {
        apply(&app);
    } else {
        root.connect_notify_local(Some("application"), move |win, _| {
            if let Some(app) = win.application() {
                apply(&app);
            }
        });
    }
}

fn wire_escape_stop_generation(
    root: &adw::ApplicationWindow,
    sender: &ComponentSender<MainWindow>,
) {
    let key_ctrl = gtk::EventControllerKey::new();
    let input_sender = sender.input_sender().clone();
    key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            let _ = input_sender.send(MainWindowMsg::StopGeneration);
        }
        gtk::glib::Propagation::Proceed
    });
    root.add_controller(key_ctrl);
}

fn present_about_dialog(parent: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("Companion")
        .application_icon(crate::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Companion Contributors")
        .license_type(gtk::License::Gpl30)
        .website("https://github.com/johncs/companion")
        .issue_url("https://github.com/johncs/companion/issues")
        .comments(gettext(
            "Native GNOME AI agent with local GGUF models and folder-scoped tools.",
        ))
        .build();
    about.present(Some(parent));
}

fn present_shortcuts_dialog(parent: &adw::ApplicationWindow) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Keyboard Shortcuts"))
        .content_width(480)
        .content_height(460)
        .build();

    let group = adw::PreferencesGroup::builder()
        .title(gettext("General"))
        .build();
    let shortcuts: [(String, &[&str]); 6] = [
        (gettext("New Chat"), &["Ctrl", "N"]),
        (gettext("Search Chats"), &["Ctrl", "F"]),
        (gettext("Preferences"), &["Ctrl", ","]),
        (gettext("Keyboard Shortcuts"), &["Ctrl", "?"]),
        (gettext("Stop Generation"), &["Esc"]),
        (gettext("Send Message"), &["Ctrl", "↵"]),
    ];
    for (title, keys) in shortcuts {
        let row = adw::ActionRow::builder().title(title).build();
        row.add_suffix(&build_keycaps_box(keys));
        group.add(&row);
    }

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

fn build_keycaps_box(keys: &[&str]) -> gtk::Box {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .valign(gtk::Align::Center)
        .build();
    for key in keys {
        let cap = gtk::Label::builder()
            .label(*key)
            .xalign(0.5)
            .valign(gtk::Align::Center)
            .css_classes(["shortcut-keycap"])
            .build();
        box_.append(&cap);
    }
    box_
}

impl MainWindow {
    pub(super) fn focus_search(&self) {
        self.split_view.set_show_content(false);
        self.sidebar.emit(super::sidebar::SidebarMsg::FocusSearch);
    }
}
