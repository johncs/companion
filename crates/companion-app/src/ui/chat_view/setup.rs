use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::core::types::StagedAttachment;
use crate::services::AppServices;
use crate::storage::AttachmentRecord;
use crate::ui::composer::{ComposerModel, ComposerOutput};
use crate::ui::message_row::build_message_list_factory;

use super::{ChatViewModel, LoadedPageData, agent_runner, message_actions};

#[derive(Debug)]
pub enum ChatViewMsg {
    LoadConversation {
        conversation_id: String,
        title: String,
    },
    ConversationPageLoaded {
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
    },
    ClearConversation,
    CancelConversation(String),
    OpenFolderPermissions,
    GrantFolder(PathBuf),
    PresentGrantFolderDialog {
        conv_id: String,
        folder_name: String,
        folder_str: String,
    },
    RefreshGrantedFolders,
    GrantedFoldersLoaded {
        conv_id: String,
        folders: Vec<crate::storage::ConversationFolder>,
    },
    SendMessage {
        text: String,
        thinking_level: ThinkingLevel,
        attachments: Vec<StagedAttachment>,
    },
    SendPrepared {
        conv_id: String,
        thinking_level: ThinkingLevel,
        result: Result<
            (
                crate::storage::entities::Message,
                Vec<AttachmentRecord>,
                agent_runner::PreparedPrompt,
            ),
            String,
        >,
    },
    EditAndResendMessage {
        message_id: String,
        new_content: String,
    },
    EditPrepared(message_actions::EditPreparedPayload),
    DeleteMessageFrom(String),
    DeleteCompleted {
        conv_id: String,
        message_id: String,
        result: Result<(), String>,
    },
    StopGeneration,
    LoadOlderPage,
    OlderPageLoaded {
        conversation_id: String,
        request_seq: u64,
        result: Result<LoadedPageData, String>,
    },
    SystemPromptChanged(String),
    ThinkingLevelChanged(ThinkingLevel),
    WebSearchToggled(bool),
    RefreshWebSearchGlobal(bool),
    RefreshModels(Option<String>),
    PersonalizationChanged {
        assistant_name: String,
        user_name: Option<String>,
    },
    ConversationTitleUpdated {
        id: String,
        title: String,
    },
    OpenModelSettings,
    ModelContextLengthChanged(u32),
    ShowToast(String),
}

#[derive(Debug)]
pub enum ChatViewOutput {
    ShowToast(String),
    ConversationTitleUpdated {
        id: String,
        title: String,
    },
    ChatRunningChanged {
        conversation_id: String,
        is_running: bool,
    },
    ModelContextLengthChanged(u32),
    OpenModelSettings,
}

pub(super) fn build_chat_list_factory(
    assistant_name: Rc<RefCell<String>>,
    user_name: Rc<RefCell<Option<String>>>,
    sender: &ComponentSender<ChatViewModel>,
) -> gtk::SignalListItemFactory {
    let edit_sender = sender.input_sender().clone();
    let delete_sender = sender.input_sender().clone();
    build_message_list_factory(
        assistant_name,
        user_name,
        Rc::new(move |message_id, new_content| {
            let _ = edit_sender.send(ChatViewMsg::EditAndResendMessage {
                message_id,
                new_content,
            });
        }),
        Rc::new(move |message_id| {
            let _ = delete_sender.send(ChatViewMsg::DeleteMessageFrom(message_id));
        }),
    )
}

pub(super) fn build_chat_composer(
    services: Arc<AppServices>,
    active_model_id: Option<String>,
    sender: &ComponentSender<ChatViewModel>,
) -> Controller<ComposerModel> {
    ComposerModel::builder()
        .launch((services, active_model_id))
        .forward(sender.input_sender(), |output| match output {
            ComposerOutput::Send {
                text,
                thinking_level,
                attachments,
            } => ChatViewMsg::SendMessage {
                text,
                thinking_level,
                attachments,
            },
            ComposerOutput::StopRequested => ChatViewMsg::StopGeneration,
            ComposerOutput::OpenFolderPermissions => ChatViewMsg::OpenFolderPermissions,
            ComposerOutput::GrantFolder(path) => ChatViewMsg::GrantFolder(path),
            ComposerOutput::WebSearchToggled(enabled) => ChatViewMsg::WebSearchToggled(enabled),
            ComposerOutput::SystemPromptChanged(p) => ChatViewMsg::SystemPromptChanged(p),
            ComposerOutput::ThinkingLevelChanged(lvl) => ChatViewMsg::ThinkingLevelChanged(lvl),
            ComposerOutput::ModelContextLengthChanged(n_ctx) => {
                ChatViewMsg::ModelContextLengthChanged(n_ctx)
            }
            ComposerOutput::OpenModelSettings => ChatViewMsg::OpenModelSettings,
            ComposerOutput::ShowToast(msg) => ChatViewMsg::ShowToast(msg),
        })
}
