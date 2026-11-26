//! Paginated SQLite message loading, parallel Markdown parsing, and scroll/param helpers.

use adw::prelude::*;
use gtk::glib;

use crate::storage::{
    AttachmentRecord, Conversation, ConversationFolder, Database, ToolCallRecord,
};

pub(super) const PAGE_SIZE: usize = 20;

pub(super) type LoadedMessageItem = (
    crate::storage::entities::Message,
    Vec<AttachmentRecord>,
    Vec<ToolCallRecord>,
    Vec<crate::core::markdown::MarkdownBlock>,
);

#[derive(Debug)]
pub struct LoadedPageData {
    pub(super) conversation: Option<Conversation>,
    pub(super) granted_folders: Option<Vec<ConversationFolder>>,
    pub(super) items: Vec<LoadedMessageItem>,
    pub(super) has_more_pages: bool,
    pub(super) oldest_loaded_ts: Option<i64>,
    pub(super) est_tokens: u32,
}

pub(super) fn maybe_scroll_bottom(scroller: &gtk::ScrolledWindow) {
    let vadj = scroller.vadjustment();
    let near_bottom = vadj.value() + vadj.page_size() >= vadj.upper() - 120.0;
    if near_bottom {
        scroll_scroller_to_bottom(scroller);
    }
}

pub(super) fn scroll_scroller_to_bottom(scroller: &gtk::ScrolledWindow) {
    let vadj = scroller.vadjustment();
    let target = (vadj.upper() - vadj.page_size()).max(0.0);
    vadj.set_value(target);
}

/// Fetch a conversation page from SQLite and parse all Markdown/syntect blocks in parallel across CPU cores.
pub(super) fn load_and_parse_page(
    db: &Database,
    conv_id: &str,
    before_created_at: Option<i64>,
    sys_prompt_len: usize,
) -> Result<LoadedPageData, String> {
    let (conversation, granted_folders) = if before_created_at.is_none() {
        (
            db.get_conversation(conv_id).ok().flatten(),
            Some(db.list_conversation_folders(conv_id).unwrap_or_default()),
        )
    } else {
        (None, None)
    };
    let effective_sys_len = conversation
        .as_ref()
        .and_then(|c| c.system_prompt.as_deref())
        .map(str::len)
        .unwrap_or(sys_prompt_len);

    let messages = db
        .list_messages_page(conv_id, before_created_at, PAGE_SIZE)
        .map_err(|e| e.to_string())?;
    let has_more_pages = messages.len() == PAGE_SIZE;
    let oldest_loaded_ts = messages.first().map(|m| m.created_at);

    let msg_ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
    let mut att_map = db
        .list_attachments_for_messages(&msg_ids)
        .unwrap_or_default();
    let mut tc_map = db
        .list_tool_calls_for_messages(&msg_ids)
        .unwrap_or_default();

    let mut approx_chars = effective_sys_len;
    for m in &messages {
        approx_chars += m.content.len();
        if let Some(atts) = att_map.get(&m.id) {
            for a in atts {
                if let Some(t) = &a.extracted_text {
                    approx_chars += t.len();
                }
            }
        }
    }
    let est_tokens = if messages.is_empty() {
        0
    } else {
        (approx_chars / 4).max(1) as u32
    };

    // Parse assistant Markdown & syntect code blocks in parallel across CPU cores.
    let parsed_blocks: Vec<Vec<crate::core::markdown::MarkdownBlock>> =
        crate::core::parallel_chunks(&messages, 1, |chunk| {
            chunk
                .iter()
                .map(|m| {
                    if m.role == crate::core::types::MessageRole::Assistant {
                        crate::core::markdown::parse_markdown_blocks(&m.content)
                    } else {
                        vec![crate::core::markdown::MarkdownBlock::Text(
                            glib::markup_escape_text(&m.content).to_string(),
                        )]
                    }
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .flatten()
        .collect();

    let items = messages
        .into_iter()
        .zip(parsed_blocks)
        .map(|(m, blocks)| {
            let atts = att_map.remove(&m.id).unwrap_or_default();
            let tcs = tc_map.remove(&m.id).unwrap_or_default();
            (m, atts, tcs, blocks)
        })
        .collect();

    Ok(LoadedPageData {
        conversation,
        granted_folders,
        items,
        has_more_pages,
        oldest_loaded_ts,
        est_tokens,
    })
}
