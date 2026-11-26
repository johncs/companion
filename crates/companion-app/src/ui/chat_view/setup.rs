use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use relm4::prelude::*;

use crate::core::ThinkingLevel;
use crate::storage::{AttachmentRecord, Database};
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
        attachments: Vec<AttachmentRecord>,
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
        pos: u32,
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
    db: Database,
    active_model_id: Option<String>,
    sender: &ComponentSender<ChatViewModel>,
) -> Controller<ComposerModel> {
    ComposerModel::builder()
        .launch((db, active_model_id))
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
            ComposerOutput::OpenModelSettings => ChatViewMsg::OpenModelSettings,
            ComposerOutput::ShowToast(msg) => ChatViewMsg::ShowToast(msg),
        })
}
