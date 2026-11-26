//! Composer message action handlers (`SendPressed`, `AttachmentLoaded`, `ControlsApplied`).

use adw::prelude::*;
use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::i18n::gettext;
use crate::storage::AttachmentRecord;

use super::attachments::rebuild_chips_box;
use super::{ComposerModel, ComposerMsg, ComposerOutput};

impl ComposerModel {
    pub(super) fn handle_send_pressed(&mut self, sender: &ComponentSender<Self>) {
        if self.is_generating {
            sender.input(ComposerMsg::StopPressed);
            return;
        }
        if self.attachments.has_loading() {
            let _ = sender.output(ComposerOutput::ShowToast(gettext(
                "Please wait for attached files to finish loading",
            )));
            return;
        }

        let buffer = self.text_view.buffer();
        let (start, end) = buffer.bounds();
        let text = buffer.text(&start, &end, false).trim().to_string();
        let ready_attachments = self.attachments.take_ready();

        if text.is_empty() && ready_attachments.is_empty() {
            return;
        }

        buffer.set_text("");
        rebuild_chips_box(&self.chips_box, &self.attachments, sender);

        let _ = sender.output(ComposerOutput::Send {
            text,
            thinking_level: self.thinking_level,
            attachments: ready_attachments,
        });
    }

    pub(super) fn handle_attachment_loaded(
        &mut self,
        temp_id: String,
        result: Result<AttachmentRecord, String>,
        sender: &ComponentSender<Self>,
    ) {
        match result {
            Ok(record) => {
                self.attachments.mark_loaded(&temp_id, record);
                rebuild_chips_box(&self.chips_box, &self.attachments, sender);
            }
            Err(err) => {
                self.attachments.remove(&temp_id);
                rebuild_chips_box(&self.chips_box, &self.attachments, sender);
                let _ = sender.output(ComposerOutput::ShowToast(err));
            }
        }
    }

    pub(super) fn handle_controls_applied(
        &mut self,
        thinking_level: ThinkingLevel,
        system_prompt: String,
        context_length: u32,
        sender: &ComponentSender<Self>,
    ) {
        let prompt_changed = self.system_prompt != system_prompt;
        let thinking_changed = self.thinking_level != thinking_level;
        self.thinking_level = thinking_level;
        self.system_prompt = system_prompt.clone();

        if let Some(mid) = &self.active_model_id
            && let Ok(Some(mut rec)) = self.db.get_model(mid)
        {
            rec.default_params.n_ctx = context_length;
            let _ = self.db.upsert_model(&rec);
        }

        self.refresh_controls_button();

        if prompt_changed {
            let _ = sender.output(ComposerOutput::SystemPromptChanged(system_prompt));
        }
        if thinking_changed {
            let _ = sender.output(ComposerOutput::ThinkingLevelChanged(thinking_level));
        }
    }
}
