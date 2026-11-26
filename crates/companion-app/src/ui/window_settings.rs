use std::sync::Arc;

use relm4::prelude::*;

use crate::models::hardware::{HardwareInfo, probe_hardware};
use crate::services::AppServices;
use crate::ui::preferences::{SettingsDialogConfig, SettingsPage, present_settings_dialog};

use super::window::{MainWindow, MainWindowMsg};

pub(super) fn open_settings_dialog(
    toast_overlay: &adw::ToastOverlay,
    services: &Arc<AppServices>,
    hardware_info: Option<&HardwareInfo>,
    active_model_id: Option<String>,
    initial_page: SettingsPage,
    sender: &ComponentSender<MainWindow>,
) {
    if let Some(hw) = hardware_info.cloned() {
        present_settings(
            toast_overlay,
            services.clone(),
            active_model_id,
            initial_page,
            hw,
            sender,
        );
        return;
    }
    let toast_overlay = toast_overlay.clone();
    let services_bg = services.clone();
    let sender_ui = sender.clone();
    crate::ui::widgets::spawn_blocking_ui(&services.rt_handle, probe_hardware, move |hw| {
        sender_ui.input(MainWindowMsg::HardwareProbed(hw.clone()));
        present_settings(
            &toast_overlay,
            services_bg,
            active_model_id,
            initial_page,
            hw,
            &sender_ui,
        );
    });
}

fn present_settings(
    toast_overlay: &adw::ToastOverlay,
    services: Arc<AppServices>,
    active_model_id: Option<String>,
    initial_page: SettingsPage,
    hw: HardwareInfo,
    sender: &ComponentSender<MainWindow>,
) {
    let (sender_model, sender_name, sender_web) = (sender.clone(), sender.clone(), sender.clone());
    present_settings_dialog(
        toast_overlay,
        SettingsDialogConfig {
            services,
            hardware_info: hw,
            active_model_id,
            initial_page,
            on_active_changed: move |id| sender_model.input(MainWindowMsg::ActiveModelChanged(id)),
            on_personalization_changed: move |assistant_name, user_name| {
                sender_name.input(MainWindowMsg::PersonalizationChanged {
                    assistant_name,
                    user_name,
                });
            },
            on_web_search_global_changed: move |enabled| {
                sender_web.input(MainWindowMsg::WebSearchGlobalChanged(enabled));
            },
        },
    );
}
