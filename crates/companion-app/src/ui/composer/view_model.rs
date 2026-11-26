//! Pure, GTK-free toolbar state calculation for [`super::ComposerModel`].

use crate::core::types::SearchEngine;
use crate::i18n::{gettext, tr_f};
use crate::storage::ConversationFolder;

/// Pure view-model computing composer toolbar button states and tooltips.
pub(super) struct ComposerViewModel;

impl ComposerViewModel {
    /// Returns `(sensitive, tooltip)` for the attach-file button.
    pub(super) fn attach_button_state(has_active_model: bool, has_mmproj: bool) -> (bool, String) {
        if !has_active_model {
            return (
                false,
                gettext("Select an AI model in Settings to attach files"),
            );
        }
        let tip = if has_mmproj {
            gettext("Attach files (images, text, PDF)")
        } else {
            gettext("Attach text or PDF files (active model has no vision projector for images)")
        };
        (true, tip)
    }

    /// Returns `(accent, tooltip)` for the granted-folders button.
    pub(super) fn folders_button_state(granted_folders: &[ConversationFolder]) -> (bool, String) {
        if granted_folders.is_empty() {
            return (false, gettext("Files & Folders (none granted)"));
        }
        let mut tip = tr_f(
            "Files & Folders ({count} granted)",
            &[("count", &granted_folders.len().to_string())],
        );
        for f in granted_folders {
            let mode_label = if f.permission_mode.allows_write() {
                gettext("Read & Write")
            } else {
                gettext("Read-Only")
            };
            tip.push_str(&format!("\n• {} ({mode_label})", f.folder_path));
        }
        (true, tip)
    }

    /// Returns `(sensitive, accent, tooltip)` for the web-search toggle button.
    pub(super) fn globe_button_state(
        global_enabled: bool,
        chat_enabled: bool,
        engine: SearchEngine,
    ) -> (bool, bool, String) {
        if !global_enabled {
            return (false, false, gettext("Web Search is disabled in Settings"));
        }
        if chat_enabled {
            let engine_label = engine.label();
            (
                true,
                true,
                tr_f("Web Search (On · {engine})", &[("engine", &engine_label)]),
            )
        } else {
            (true, false, gettext("Web Search (Off)"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ComposerViewModel;
    use crate::core::types::{FolderPermission, SearchEngine};
    use crate::storage::ConversationFolder;

    #[test]
    fn computes_toolbar_button_states() {
        assert!(!ComposerViewModel::attach_button_state(false, false).0);
        let (sens, tip_pdf) = ComposerViewModel::attach_button_state(true, false);
        assert!(sens && tip_pdf.contains("no vision projector"));
        let (sens_img, tip_img) = ComposerViewModel::attach_button_state(true, true);
        assert!(sens_img && tip_img.contains("images, text, PDF"));

        assert_eq!(
            ComposerViewModel::folders_button_state(&[]),
            (false, "Files & Folders (none granted)".to_string())
        );
        let (accent, tip) = ComposerViewModel::folders_button_state(&[ConversationFolder {
            id: "1".into(),
            conversation_id: "c1".into(),
            folder_path: "/tmp/docs".into(),
            permission_mode: FolderPermission::ReadWrite,
            created_at: 0,
        }]);
        assert!(accent && tip.contains("/tmp/docs") && tip.contains("Read & Write"));

        assert_eq!(
            ComposerViewModel::globe_button_state(false, true, SearchEngine::DuckDuckGo),
            (
                false,
                false,
                "Web Search is disabled in Settings".to_string()
            )
        );
        assert_eq!(
            ComposerViewModel::globe_button_state(true, true, SearchEngine::SearXNG),
            (true, true, "Web Search (On · SearXNG)".to_string())
        );
    }
}
