//! Composer message action handlers (`SendPressed`, `AttachmentLoaded`, `ControlsApplied`).

use std::path::PathBuf;

use adw::prelude::*;
use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::types::{SearchEngine, StagedAttachment};
use crate::i18n::gettext;
use crate::storage::{ConversationFolder, ModelRecord};
use crate::ui::mention_picker::MentionEntry;

use super::ComposerModel;
use super::attachments::rebuild_chips_box;

#[derive(Debug)]
pub enum ComposerMsg {
    SendPressed,
    StopPressed,
    AttachPressed,
    FilesDropped(Vec<PathBuf>),
    PathsClassified {
        dirs: Vec<PathBuf>,
        files: Vec<PathBuf>,
    },
    AttachmentLoaded {
        temp_id: String,
        result: Result<StagedAttachment, String>,
    },
    RemoveAttachment(String),
    OpenFolderPermissions,
    ToggleWebSearch,
    SetWebSearchEnabled(bool),
    SetWebSearchGlobalEnabled(bool),
    SetGrantedFolders(Vec<ConversationFolder>),
    GrantedFilesLoaded {
        seq: u64,
        files: Vec<MentionEntry>,
    },
    InitDataLoaded {
        assistant_name: String,
        web_search_global_enabled: bool,
        search_engine: SearchEngine,
        models: Vec<ModelRecord>,
    },
    ModelsMetadataLoaded(Vec<ModelRecord>),
    SearchEngineLoaded(SearchEngine),
    OpenChatControls,
    ControlsApplied {
        thinking_level: ThinkingLevel,
        system_prompt: String,
        context_length: u32,
    },
    OpenModelSettings,
    RefreshModels(Option<String>),
    SetSystemPrompt(String),
    SetThinkingLevel(ThinkingLevel),
    SetAssistantName(String),
    SetContextUsed(u32),
    SetGenerating(bool),
}

#[derive(Debug)]
pub enum ComposerOutput {
    Send {
        text: String,
        thinking_level: ThinkingLevel,
        attachments: Vec<StagedAttachment>,
    },
    StopRequested,
    OpenFolderPermissions,
    GrantFolder(PathBuf),
    WebSearchToggled(bool),
    SystemPromptChanged(String),
    ThinkingLevelChanged(ThinkingLevel),
    ModelContextLengthChanged(u32),
    OpenModelSettings,
    ShowToast(String),
}

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
        result: Result<StagedAttachment, String>,
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

        if self.active_model_id.is_some() {
            let _ = sender.output(ComposerOutput::ModelContextLengthChanged(context_length));
        }

        self.refresh_controls_button();

        if prompt_changed {
            let _ = sender.output(ComposerOutput::SystemPromptChanged(system_prompt));
        }
        if thinking_changed {
            let _ = sender.output(ComposerOutput::ThinkingLevelChanged(thinking_level));
        }
    }

    pub(super) fn handle_init_data_loaded(
        &mut self,
        assistant_name: String,
        web_search_global_enabled: bool,
        search_engine: crate::core::types::SearchEngine,
        models: Vec<crate::storage::ModelRecord>,
    ) {
        self.assistant_name = assistant_name;
        self.web_search_global_enabled = web_search_global_enabled;
        self.search_engine = search_engine;
        self.models = models;
        self.refresh_globe_button();
        self.refresh_controls_button();
        self.refresh_attach_button();
    }
}
