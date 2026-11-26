//! GObject wrapper for chat messages stored in a `gio::ListStore` for `gtk::ListView`.

use std::cell::RefCell;

use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::prelude::ObjectExt;
use gtk::subclass::prelude::*;
use std::sync::OnceLock;

use crate::core::ThinkingLevel;
use crate::core::markdown::{
    CodeHighlightCache, MarkdownBlock, parse_markdown_blocks, parse_markdown_blocks_cached,
    parse_markdown_blocks_streaming,
};
use crate::core::types::{ExecutionDevice, MessageRole};
use crate::storage::entities::Message;
use crate::storage::{AttachmentRecord, ToolCallRecord};

#[derive(Debug, Clone)]
pub struct MessageEntry {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub model_name: Option<String>,
    pub execution_device: Option<ExecutionDevice>,
    pub token_count: i64,
    pub generation_ms: Option<i64>,
    pub created_at: i64,
    pub blocks: Vec<MarkdownBlock>,
    pub attachments: Vec<AttachmentRecord>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub is_streaming: bool,
    pub status_text: Option<String>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MessageObject {
        pub entry: RefCell<Option<MessageEntry>>,
        pub code_cache: RefCell<CodeHighlightCache>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageObject {
        const NAME: &'static str = "CompanionMessageObject";
        type Type = super::MessageObject;
    }

    impl ObjectImpl for MessageObject {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("updated").build()])
        }
    }
}

glib::wrapper! {
    pub struct MessageObject(ObjectSubclass<imp::MessageObject>);
}

impl MessageObject {
    /// Construct a [`MessageObject`] from a persisted [`Message`], attachments, and tool calls.
    pub fn from_message_with_extras(
        msg: &Message,
        attachments: Vec<AttachmentRecord>,
        tool_calls: Vec<ToolCallRecord>,
    ) -> Self {
        let blocks = if msg.role == crate::core::types::MessageRole::Assistant {
            parse_markdown_blocks(&msg.content)
        } else {
            vec![MarkdownBlock::Text(
                glib::markup_escape_text(&msg.content).to_string(),
            )]
        };
        Self::from_message_with_blocks(msg, attachments, tool_calls, blocks)
    }

    /// Construct a [`MessageObject`] from pre-parsed [`MarkdownBlock`]s (zero main-thread parsing).
    pub fn from_message_with_blocks(
        msg: &Message,
        attachments: Vec<AttachmentRecord>,
        tool_calls: Vec<ToolCallRecord>,
        blocks: Vec<MarkdownBlock>,
    ) -> Self {
        let obj: Self = glib::Object::new();
        *obj.imp().entry.borrow_mut() = Some(MessageEntry {
            id: msg.id.clone(),
            role: msg.role,
            content: msg.content.clone(),
            reasoning_content: msg.reasoning_content.clone(),
            thinking_level: msg.thinking_level,
            model_name: msg.model_name.clone(),
            execution_device: msg.execution_device,
            token_count: msg.token_count,
            generation_ms: msg.generation_ms,
            created_at: msg.created_at,
            blocks,
            attachments,
            tool_calls,
            is_streaming: false,
            status_text: None,
        });
        obj
    }

    /// Construct a new streaming assistant [`MessageObject`].
    pub fn new_streaming(
        id: String,
        thinking_level: Option<ThinkingLevel>,
        model_name: Option<String>,
        execution_device: Option<ExecutionDevice>,
        created_at: i64,
    ) -> Self {
        let obj: Self = glib::Object::new();
        *obj.imp().entry.borrow_mut() = Some(MessageEntry {
            id,
            role: MessageRole::Assistant,
            content: String::new(),
            reasoning_content: None,
            thinking_level,
            model_name,
            execution_device,
            token_count: 0,
            generation_ms: None,
            created_at,
            blocks: Vec::new(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            is_streaming: true,
            status_text: Some(crate::i18n::gettext("Starting…")),
        });
        obj
    }

    /// Append a completed/denied tool call to this message and refresh its bound row.
    pub fn append_tool_call(&self, tc: ToolCallRecord) {
        {
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                entry.tool_calls.push(tc);
            }
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    /// Update only the transient streaming status label (e.g. `"Loading model…"`).
    pub fn set_status(&self, status: Option<&str>) {
        {
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                entry.status_text = status.map(str::to_string);
            }
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    /// Apply pre-parsed streaming `blocks` (computed off the GTK main thread) and notify bound rows.
    pub fn apply_streaming_snapshot(
        &self,
        content: String,
        blocks: Option<Vec<MarkdownBlock>>,
        reasoning: Option<String>,
        is_streaming: bool,
    ) {
        let should_emit = {
            let mut borrow = self.imp().entry.borrow_mut();
            let Some(entry) = borrow.as_mut() else {
                return;
            };
            let content_changed = entry.content != content;
            let streaming_changed = entry.is_streaming != is_streaming;
            let had_status = entry.status_text.is_some();
            if let Some(blocks) = blocks {
                entry.content = content;
                entry.blocks = blocks;
            }
            entry.reasoning_content = reasoning;
            entry.is_streaming = is_streaming;
            entry.status_text = None;
            content_changed || streaming_changed || had_status
        };
        if should_emit {
            self.emit_by_name::<()>("updated", &[]);
        }
    }

    /// Update streaming content/reasoning and notify any bound `gtk::ListView` row.
    pub fn update_streaming(&self, content: &str, reasoning: Option<&str>, is_streaming: bool) {
        let should_emit = {
            let (content_unchanged, was_streaming, had_status) = {
                let borrow = self.imp().entry.borrow();
                match borrow.as_ref() {
                    Some(e) => (
                        e.content == content,
                        e.is_streaming,
                        e.status_text.is_some(),
                    ),
                    None => (true, false, false),
                }
            };
            let mut cache = self.imp().code_cache.borrow_mut();
            let blocks = if content_unchanged && is_streaming {
                None
            } else if is_streaming {
                Some(parse_markdown_blocks_streaming(content, &mut cache))
            } else {
                Some(parse_markdown_blocks_cached(content, &mut cache))
            };
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                if let Some(blocks) = blocks {
                    entry.content = content.to_string();
                    entry.blocks = blocks;
                }
                entry.reasoning_content = reasoning.map(str::to_string);
                entry.is_streaming = is_streaming;
                entry.status_text = None;
            }
            if !is_streaming {
                cache.clear();
            }
            !content_unchanged || was_streaming != is_streaming || had_status
        };
        if should_emit {
            self.emit_by_name::<()>("updated", &[]);
        }
    }

    /// Apply the authoritative persisted message ID, timestamp, and final content after DB save.
    pub fn finalize_persisted(
        &self,
        saved_id: Option<&str>,
        created_at: Option<i64>,
        final_answer: &str,
        final_reasoning: Option<&str>,
        total_tokens: i64,
        elapsed_ms: i64,
    ) {
        self.update_streaming(final_answer, final_reasoning, false);
        {
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                if let Some(id) = saved_id {
                    entry.id = id.to_string();
                }
                if let Some(ts) = created_at {
                    entry.created_at = ts;
                }
                entry.token_count = total_tokens;
                entry.generation_ms = Some(elapsed_ms);
            }
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    /// Record completion token count and generation duration (ms) for the Message Info dialog.
    pub fn set_metrics(&self, token_count: i64, generation_ms: i64) {
        {
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                entry.token_count = token_count;
                entry.generation_ms = Some(generation_ms);
            }
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    /// Borrow the underlying [`MessageEntry`] without cloning vectors/strings.
    #[allow(clippy::expect_used)]
    pub fn with_entry<R>(&self, f: impl FnOnce(&MessageEntry) -> R) -> R {
        let borrow = self.imp().entry.borrow();
        let entry = borrow.as_ref().expect("MessageObject initialized");
        f(entry)
    }

    /// Replace a user message's displayed text after editing.
    pub fn set_user_content(&self, new_content: &str) {
        {
            let mut borrow = self.imp().entry.borrow_mut();
            if let Some(entry) = borrow.as_mut() {
                entry.content = new_content.to_string();
                entry.blocks = vec![MarkdownBlock::Text(
                    glib::markup_escape_text(new_content).to_string(),
                )];
            }
        }
        self.emit_by_name::<()>("updated", &[]);
    }

    /// Trigger a UI rebind on any row bound to this [`MessageObject`].
    pub fn emit_updated(&self) {
        self.emit_by_name::<()>("updated", &[]);
    }

    #[allow(clippy::expect_used)]
    pub fn entry(&self) -> MessageEntry {
        self.imp()
            .entry
            .borrow()
            .clone()
            .expect("MessageObject initialized")
    }
}
