//! Unified Settings dialog (`AdwPreferencesDialog`) with General, Models, and Agent & MCP pages.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::{gettext, tr_f};
use crate::models::hardware::{HardwareInfo, probe_hardware};
use crate::models::{format_bytes, managed_models_dir};
use crate::storage::Database;
use crate::ui::model_manager::build_models_page;

/// Present the unified Settings [`adw::PreferencesDialog`], optionally opening directly to `initial_page`
/// (`"general"`, `"models"`, or `"agent"`).
pub fn present_settings_dialog<F, P, W>(
    parent: &impl IsA<gtk::Widget>,
    db: Database,
    active_model_id: Option<String>,
    initial_page: &str,
    on_active_changed: F,
    on_personalization_changed: P,
    on_web_search_global_changed: W,
) where
    F: Fn(Option<String>) + 'static,
    P: Fn(String, Option<String>) + 'static,
    W: Fn(bool) + 'static,
{
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Settings"))
        .content_width(820)
        .content_height(660)
        .build();

    // --- General Page ---
    let general_page = adw::PreferencesPage::builder()
        .name("general")
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();

    let appearance_group = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .description(gettext("Companion follows your GNOME desktop theme and accent color."))
        .build();

    let style_row = adw::ComboRow::builder()
        .title(gettext("Color Scheme"))
        .subtitle(gettext("System default follows GNOME dark/light preference"))
        .model(&gtk::StringList::new(&[
            &gettext("System"),
            &gettext("Light"),
            &gettext("Dark"),
        ]))
        .build();

    let current_scheme = adw::StyleManager::default().color_scheme();
    style_row.set_selected(match current_scheme {
        adw::ColorScheme::ForceLight => 1,
        adw::ColorScheme::ForceDark => 2,
        _ => 0,
    });

    style_row.connect_selected_notify(|row| {
        let manager = adw::StyleManager::default();
        match row.selected() {
            1 => manager.set_color_scheme(adw::ColorScheme::ForceLight),
            2 => manager.set_color_scheme(adw::ColorScheme::ForceDark),
            _ => manager.set_color_scheme(adw::ColorScheme::Default),
        }
    });
    appearance_group.add(&style_row);

    let storage_group = adw::PreferencesGroup::builder()
        .title(gettext("Storage &amp; Data"))
        .build();

    let db_path = db.path().to_path_buf();
    let db_size_label = build_size_label();
    let db_row = adw::ActionRow::builder()
        .title(gettext("SQLite Database"))
        .subtitle(db_path.to_string_lossy().as_ref())
        .build();
    db_row.add_css_class("property");
    db_row.add_suffix(&db_size_label);
    db_row.add_suffix(&build_open_folder_button(&dialog, db_path.clone(), true));
    storage_group.add(&db_row);

    let models_dir = managed_models_dir();
    let models_size_label = build_size_label();
    let models_dir_row = adw::ActionRow::builder()
        .title(gettext("Downloaded AI Model Folder"))
        .subtitle(models_dir.to_string_lossy().as_ref())
        .build();
    models_dir_row.add_css_class("property");
    models_dir_row.add_suffix(&models_size_label);
    models_dir_row.add_suffix(&build_open_folder_button(&dialog, models_dir.clone(), false));
    storage_group.add(&models_dir_row);

    let total_row = adw::ActionRow::builder()
        .title(gettext("Total Allocated"))
        .subtitle(gettext("Calculating…"))
        .build();
    total_row.add_css_class("property");
    storage_group.add(&total_row);

    spawn_storage_size_refresh(
        total_row,
        db_size_label,
        models_size_label,
        db_path,
        models_dir,
    );

    let (identity_group, behavior_group, about_group) =
        build_personalization_groups(&db, on_personalization_changed);
    let (tools_group, update_web_tools) =
        build_available_tools_group(db.get_web_search_global_enabled().unwrap_or(false));
    let web_search_group = build_web_search_group(&db, move |enabled| {
        update_web_tools(enabled);
        on_web_search_global_changed(enabled);
    });
    let hw = probe_hardware();

    general_page.add(&appearance_group);
    general_page.add(&storage_group);
    general_page.add(&build_hardware_group(&hw));

    // --- Personalization Page ---
    let personalization_page = adw::PreferencesPage::builder()
        .name("personalization")
        .title(gettext("Personalization"))
        .icon_name("avatar-default-symbolic")
        .build();
    personalization_page.add(&identity_group);
    personalization_page.add(&behavior_group);
    personalization_page.add(&about_group);

    // --- Models Page ---
    let models_page = build_models_page(
        &dialog,
        db.clone(),
        hw,
        active_model_id,
        Rc::new(on_active_changed),
    );

    // --- Agent & MCP Page ---
    let agent_page = adw::PreferencesPage::builder()
        .name("agent")
        .title(gettext("Agent & MCP"))
        .icon_name("applications-science-symbolic")
        .build();

    agent_page.add(&web_search_group);
    agent_page.add(&tools_group);

    let agent_group = adw::PreferencesGroup::builder()
        .title(gettext("External MCP Servers"))
        .description(gettext("External MCP server connections will be configured here."))
        .build();
    agent_page.add(&agent_group);

    dialog.add(&general_page);
    dialog.add(&personalization_page);
    dialog.add(&models_page);
    dialog.add(&agent_page);

    dialog.set_visible_page_name(initial_page);
    dialog.present(Some(parent));
}

fn build_web_search_group<W>(
    db: &Database,
    on_global_changed: W,
) -> adw::PreferencesGroup
where
    W: Fn(bool) + 'static,
{
    use crate::agent::web_search::SearchEngine;

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

fn build_available_tools_group(
    web_search_enabled: bool,
) -> (adw::PreferencesGroup, Rc<dyn Fn(bool)>) {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Available Companion Tools"))
        .description(gettext(
            "Built-in tools your companion can invoke. Local file tools are jailed to granted folders, and sensitive or web actions require your approval.",
        ))
        .build();

    let build_columns =
        |read_col: &str, rw_col: &str, is_header: bool| -> (gtk::Box, gtk::Label, gtk::Label) {
            let cols = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .valign(gtk::Align::Center)
                .build();
            let classes: &[&str] = if is_header {
                &["heading", "caption", "dim-label"]
            } else {
                &["caption", "dim-label"]
            };
            let read_lbl = gtk::Label::builder()
                .label(read_col)
                .xalign(1.0)
                .width_request(64)
                .css_classes(classes)
                .build();
            let rw_lbl = gtk::Label::builder()
                .label(rw_col)
                .xalign(1.0)
                .width_request(105)
                .css_classes(classes)
                .build();
            cols.append(&read_lbl);
            cols.append(&rw_lbl);
            (cols, read_lbl, rw_lbl)
        };

    let header_row = adw::ActionRow::builder()
        .title(gettext("Tool"))
        .activatable(false)
        .selectable(false)
        .build();
    header_row.add_css_class("dim-label");
    let (header_cols, _, _) = build_columns(&gettext("Read"), &gettext("Read & Write"), true);
    header_row.add_suffix(&header_cols);
    group.add(&header_row);

    let web_status = if web_search_enabled {
        gettext("Ask")
    } else {
        gettext("Deny")
    };
    let tools = [
        (
            "list_files",
            gettext("List files and folders inside a granted directory (respecting .gitignore)."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "read_file",
            gettext("Read UTF-8 text files up to 100 KB (prompts if the path is not granted yet)."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "search_files",
            gettext("Search for matching text lines across granted files and folders."),
            gettext("Allow"),
            gettext("Allow"),
            false,
        ),
        (
            "write_file",
            gettext("Create or overwrite files inside a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "create_directory",
            gettext("Create subdirectories inside a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "move_path",
            gettext("Move or rename files and folders within a granted directory."),
            gettext("Ask"),
            gettext("Allow"),
            false,
        ),
        (
            "delete_file",
            gettext("Delete a single file inside a granted directory."),
            gettext("Ask"),
            gettext("Ask"),
            false,
        ),
        (
            "delete_directory",
            gettext("Delete a subdirectory inside a granted directory."),
            gettext("Ask"),
            gettext("Ask"),
            false,
        ),
        (
            "web_search",
            gettext("Search the web via DuckDuckGo or SearXNG when Web Search is enabled."),
            web_status.clone(),
            web_status.clone(),
            true,
        ),
        (
            "fetch_url",
            gettext("Read plain text from an approved HTTPS web page when Web Search is enabled."),
            web_status.clone(),
            web_status,
            true,
        ),
    ];

    let mut web_labels: Vec<gtk::Label> = Vec::new();
    for (name, desc, read_col, rw_col, is_web) in tools {
        let friendly = crate::ui::permission_dialog::friendly_tool_name(name);
        let row = adw::ActionRow::builder()
            .title(&friendly)
            .subtitle(&desc)
            .build();
        let (cols, r_lbl, rw_lbl) = build_columns(&read_col, &rw_col, false);
        if is_web {
            web_labels.push(r_lbl);
            web_labels.push(rw_lbl);
        }
        row.add_suffix(&cols);
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

fn build_personalization_groups<P>(
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
    use crate::core::AssistantPersonality;
    use std::cell::{Cell, RefCell};

    let identity_group = adw::PreferencesGroup::builder()
        .title(gettext("Companion Identity"))
        .description(gettext("Give your companion a name and choose its character."))
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

    let behavior_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .build();
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

    let refresh_behavior_view: Rc<dyn Fn(AssistantPersonality)> = {
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
                let text = db.get_custom_personality().ok().flatten().unwrap_or_else(|| {
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
    };

    let on_changed = Rc::new(on_personalization_changed);
    let current_assistant = db
        .get_assistant_name()
        .unwrap_or_else(|_| "Companion".to_string());
    let assistant_row = adw::EntryRow::builder()
        .title(gettext("Companion Name"))
        .text(&current_assistant)
        .build();
    {
        let db = db.clone();
        let on_changed = on_changed.clone();
        let refresh_behavior = refresh_behavior_view.clone();
        let commit_assistant = Rc::new(move |row: &adw::EntryRow, normalize_empty: bool| {
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

        let c1 = commit_assistant.clone();
        assistant_row.connect_changed(move |row| c1(row, false));

        let c2 = commit_assistant.clone();
        assistant_row.connect_entry_activated(move |row| c2(row, true));

        let focus = gtk::EventControllerFocus::new();
        let row_ref = assistant_row.clone();
        focus.connect_leave(move |_| commit_assistant(&row_ref, true));
        assistant_row.add_controller(focus);
    }

    let current_user = db.get_user_name().ok().flatten().unwrap_or_default();
    let user_row = adw::EntryRow::builder()
        .title(gettext("Your Name"))
        .text(&current_user)
        .build();
    {
        let db = db.clone();
        let refresh_behavior = refresh_behavior_view.clone();
        let commit_user = Rc::new(move |row: &adw::EntryRow| {
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

        let c1 = commit_user.clone();
        user_row.connect_changed(move |row| c1(row));

        let c2 = commit_user.clone();
        user_row.connect_entry_activated(move |row| c2(row));

        let focus = gtk::EventControllerFocus::new();
        let row_ref = user_row.clone();
        focus.connect_leave(move |_| commit_user(&row_ref));
        user_row.add_controller(focus);
    }

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
    {
        let db = db.clone();
        let refresh_behavior = refresh_behavior_view.clone();
        personality_row.connect_selected_notify(move |row| {
            let selected = AssistantPersonality::from_index(row.selected());
            let _ = db.set_assistant_personality(selected);
            refresh_behavior(selected);
        });
    }

    {
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
            if db_focus.get_assistant_personality().unwrap_or_default()
                == AssistantPersonality::Custom
            {
                let buf = buf_ref.borrow();
                let (start, end) = buf.bounds();
                let _ = db_focus.set_custom_personality(&buf.text(&start, &end, false));
            }
        });
        behavior_view.add_controller(focus);
    }

    let behavior_scroller = gtk::ScrolledWindow::builder()
        .min_content_height(100)
        .max_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["card"])
        .child(&behavior_view)
        .build();
    behavior_group.add(&behavior_scroller);

    let current_about = db.get_user_about().ok().flatten().unwrap_or_default();
    let about_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .build();
    let about_buf = about_view.buffer();
    about_buf.set_text(&current_about);
    {
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
    let about_scroller = gtk::ScrolledWindow::builder()
        .min_content_height(100)
        .max_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .overflow(gtk::Overflow::Hidden)
        .margin_top(8)
        .css_classes(["card"])
        .child(&about_view)
        .build();

    identity_group.add(&assistant_row);
    identity_group.add(&personality_row);
    about_group.add(&user_row);
    about_group.add(&about_scroller);
    (identity_group, behavior_group, about_group)
}

fn build_hardware_group(hw: &HardwareInfo) -> adw::PreferencesGroup {
    let hw_group = adw::PreferencesGroup::builder()
        .title(gettext("System Hardware"))
        .build();
    let ram_str = if hw.total_ram_bytes > 0 {
        tr_f(
            "{total} total ({avail} available)",
            &[
                ("total", &format_bytes(hw.total_ram_bytes)),
                ("avail", &format_bytes(hw.available_ram_bytes)),
            ],
        )
    } else {
        gettext("Unknown")
    };
    let gpu_str = if hw.has_dri_gpu {
        gettext("GPU/DRI detected (Vulkan)")
    } else {
        gettext("CPU mode")
    };
    let hw_row = adw::ActionRow::builder()
        .title(gettext("Memory &amp; Acceleration"))
        .subtitle(format!("{ram_str} · {gpu_str}"))
        .build();
    hw_row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    hw_group.add(&hw_row);
    hw_group
}

fn build_open_folder_button(
    dialog: &adw::PreferencesDialog,
    path: PathBuf,
    is_file: bool,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Open in File Manager"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    let dialog = dialog.clone();
    btn.connect_clicked(move |_| {
        open_path_in_file_manager(&dialog, &path, is_file);
    });
    btn
}

fn open_path_in_file_manager(dialog: &adw::PreferencesDialog, path: &PathBuf, is_file: bool) {
    if !is_file && !path.exists() {
        if let Err(err) = std::fs::create_dir_all(path) {
            dialog.add_toast(adw::Toast::new(&tr_f(
                "Failed to open folder: {err}",
                &[("err", &err.to_string())],
            )));
            return;
        }
    }

    let file = gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));
    let window = dialog.root().and_downcast::<gtk::Window>();
    let dialog_toast = dialog.clone();

    let handle_result = move |res: Result<(), gtk::glib::Error>| {
        if let Err(err) = res {
            tracing::warn!("Failed to open path in file manager: {err}");
            dialog_toast.add_toast(adw::Toast::new(&tr_f(
                "Failed to open folder: {err}",
                &[("err", &err.to_string())],
            )));
        }
    };

    if is_file {
        launcher.open_containing_folder(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    } else {
        launcher.launch(window.as_ref(), None::<&gio::Cancellable>, handle_result);
    }
}

fn build_size_label() -> gtk::Label {
    gtk::Label::builder()
        .label(gettext("Calculating…"))
        .valign(gtk::Align::Center)
        .css_classes(["dim-label", "numeric"])
        .build()
}

fn spawn_storage_size_refresh(
    total_row: adw::ActionRow,
    db_size_label: gtk::Label,
    models_size_label: gtk::Label,
    db_path: PathBuf,
    models_dir: PathBuf,
) {
    let (tx, rx) = tokio::sync::oneshot::channel::<(u64, u64)>();
    crate::core::runtime().spawn_blocking(move || {
        let (db_bytes, models_bytes) = std::thread::scope(|s| {
            let db_handle = s.spawn(|| compute_sqlite_storage_bytes(&db_path));
            let models_bytes = compute_dir_size_parallel(&models_dir);
            let db_bytes = db_handle.join().unwrap_or(0);
            (db_bytes, models_bytes)
        });
        let _ = tx.send((db_bytes, models_bytes));
    });

    glib::MainContext::default().spawn_local(async move {
        if let Ok((db_bytes, models_bytes)) = rx.await {
            db_size_label.set_text(&format_bytes(db_bytes));
            models_size_label.set_text(&format_bytes(models_bytes));
            let total = db_bytes.saturating_add(models_bytes);
            total_row.set_subtitle(&format_bytes(total));
        }
    });
}

fn compute_sqlite_storage_bytes(db_path: &Path) -> u64 {
    let base = db_path.as_os_str();
    let mut wal_os = base.to_os_string();
    wal_os.push("-wal");
    let mut shm_os = base.to_os_string();
    shm_os.push("-shm");

    [db_path, Path::new(&wal_os), Path::new(&shm_os)]
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn compute_dir_size_parallel(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    if entries.is_empty() {
        return 0;
    }

    let has_subdirs = entries
        .iter()
        .any(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false));
    let min_parallel = if has_subdirs { 1 } else { 16 };
    crate::core::parallel_chunks(&entries, min_parallel, sum_entries_iterative)
        .into_iter()
        .sum()
}

fn visit_dir_entry(entry: &std::fs::DirEntry, dir_stack: &mut Vec<PathBuf>) -> u64 {
    let Ok(ft) = entry.file_type() else {
        return 0;
    };
    if ft.is_file() {
        entry.metadata().map(|m| m.len()).unwrap_or(0)
    } else if ft.is_dir() {
        dir_stack.push(entry.path());
        0
    } else {
        0
    }
}

fn sum_entries_iterative(entries: &[std::fs::DirEntry]) -> u64 {
    let mut total = 0u64;
    let mut dir_stack: Vec<PathBuf> = Vec::new();

    for entry in entries {
        total = total.saturating_add(visit_dir_entry(entry, &mut dir_stack));
    }

    while let Some(dir) = dir_stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            total = total.saturating_add(visit_dir_entry(&entry, &mut dir_stack));
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_storage_sums_db_and_wal_shm_sidecars() {
        let dir = std::env::temp_dir().join(format!("companion_pref_db_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("companion.db");
        std::fs::write(&db_path, vec![1u8; 100]).unwrap();
        std::fs::write(dir.join("companion.db-wal"), vec![2u8; 250]).unwrap();
        std::fs::write(dir.join("companion.db-shm"), vec![3u8; 50]).unwrap();

        assert_eq!(compute_sqlite_storage_bytes(&db_path), 400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_dir_size_handles_nested_and_flat_dirs() {
        let dir = std::env::temp_dir().join(format!("companion_pref_dir_{}", uuid::Uuid::new_v4()));
        let sub = dir.join("nested");
        std::fs::create_dir_all(&sub).unwrap();

        for i in 0..20 {
            std::fs::write(dir.join(format!("file_{i}.bin")), vec![0u8; 10]).unwrap();
        }
        std::fs::write(sub.join("deep.bin"), vec![0u8; 55]).unwrap();

        assert_eq!(compute_dir_size_parallel(&dir), 255);
        assert_eq!(compute_dir_size_parallel(&dir.join("missing")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
