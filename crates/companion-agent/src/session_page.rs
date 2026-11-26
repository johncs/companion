//! Paginated SQLite message loading and parallel Markdown block parsing.

use companion_core::types::MessageRole;
use companion_format::markdown::{MarkdownBlock, escape_pango_markup, parse_markdown_blocks};
use companion_storage::entities::Message;
use companion_storage::{
    AttachmentRecord, Conversation, ConversationFolder, Database, ToolCallRecord,
};

/// Single message row bundled with its attachments, tool calls, and parsed Markdown blocks.
pub type LoadedMessageItem = (
    Message,
    Vec<AttachmentRecord>,
    Vec<ToolCallRecord>,
    Vec<MarkdownBlock>,
);

/// Paginated conversation snapshot fetched from SQLite and parsed in parallel.
#[derive(Debug)]
pub struct LoadedConversationPage {
    pub conversation: Option<Conversation>,
    pub granted_folders: Option<Vec<ConversationFolder>>,
    pub items: Vec<LoadedMessageItem>,
    pub has_more_pages: bool,
    pub oldest_loaded_id: Option<String>,
    pub est_tokens: u32,
}

/// Fetch a conversation page from SQLite and parse all Markdown/syntect blocks in parallel.
pub fn load_conversation_page(
    db: &Database,
    conv_id: &str,
    before_message_id: Option<&str>,
    sys_prompt_len: usize,
    page_size: usize,
) -> Result<LoadedConversationPage, String> {
    let (conversation, granted_folders, effective_sys_len) =
        load_page_header(db, conv_id, before_message_id, sys_prompt_len);

    let messages = db
        .list_messages_page(conv_id, before_message_id, page_size)
        .map_err(|e| e.to_string())?;
    let has_more_pages = messages.len() == page_size;
    let oldest_loaded_id = messages.first().map(|m| m.id.clone());
    let msg_ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
    let mut att_map = db
        .list_attachments_for_messages(&msg_ids)
        .unwrap_or_default();
    let mut tc_map = db
        .list_tool_calls_for_messages(&msg_ids)
        .unwrap_or_default();
    let est_tokens = estimate_page_tokens(effective_sys_len, &messages, &att_map);
    let parsed_blocks = parse_page_blocks_parallel(&messages);
    let items = messages
        .into_iter()
        .zip(parsed_blocks)
        .map(|(m, blocks)| {
            let atts = att_map.remove(&m.id).unwrap_or_default();
            let tcs = tc_map.remove(&m.id).unwrap_or_default();
            (m, atts, tcs, blocks)
        })
        .collect();

    Ok(LoadedConversationPage {
        conversation,
        granted_folders,
        items,
        has_more_pages,
        oldest_loaded_id,
        est_tokens,
    })
}

fn load_page_header(
    db: &Database,
    conv_id: &str,
    before_message_id: Option<&str>,
    sys_prompt_len: usize,
) -> (
    Option<companion_storage::Conversation>,
    Option<Vec<companion_storage::ConversationFolder>>,
    usize,
) {
    if before_message_id.is_some() {
        return (None, None, sys_prompt_len);
    }
    let conversation = db.get_conversation(conv_id).ok().flatten();
    let folders = db.list_conversation_folders(conv_id).unwrap_or_default();
    let effective_sys_len = conversation
        .as_ref()
        .and_then(|c| c.system_prompt.as_deref())
        .map(str::len)
        .unwrap_or(sys_prompt_len);
    (conversation, Some(folders), effective_sys_len)
}

fn estimate_page_tokens(
    sys_len: usize,
    messages: &[Message],
    att_map: &std::collections::HashMap<String, Vec<AttachmentRecord>>,
) -> u32 {
    if messages.is_empty() {
        return 0;
    }
    let mut approx_chars = sys_len;
    for m in messages {
        approx_chars += m.content.len();
        if let Some(atts) = att_map.get(&m.id) {
            approx_chars += atts
                .iter()
                .filter_map(|a| a.extracted_text.as_deref())
                .map(str::len)
                .sum::<usize>();
        }
    }
    (approx_chars / 4).max(1) as u32
}

fn parse_page_blocks_parallel(messages: &[Message]) -> Vec<Vec<MarkdownBlock>> {
    companion_core::parallel_chunks(messages, 1, |chunk| {
        chunk
            .iter()
            .map(|m| {
                if m.role == MessageRole::Assistant {
                    parse_markdown_blocks(&m.content)
                } else {
                    vec![MarkdownBlock::Text(escape_pango_markup(&m.content))]
                }
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use companion_storage::{NewConversation, NewMessage};

    fn insert_msg(db: &Database, conv_id: &str, role: MessageRole, content: &str) {
        db.insert_message(NewMessage {
            conversation_id: conv_id,
            role,
            content,
            reasoning_content: None,
            thinking_level: None,
            model_name: None,
            execution_device: None,
            token_count: 0,
            generation_ms: None,
        })
        .expect("insert db msg");
    }

    #[test]
    fn load_conversation_page_parses_blocks_and_tracks_pagination() {
        let db = Database::open_in_memory().expect("in-memory db");
        let conv = db
            .create_conversation(NewConversation::with_title("Page Test"))
            .unwrap();
        db.add_conversation_folder(&conv.id, "/tmp").unwrap();
        insert_msg(&db, &conv.id, MessageRole::User, "Hello <world>");
        insert_msg(&db, &conv.id, MessageRole::Assistant, "**Bold**");

        let page = load_conversation_page(&db, &conv.id, None, 10, 2).unwrap();
        assert!(page.conversation.is_some());
        assert_eq!(page.granted_folders.as_ref().map(Vec::len), Some(1));
        assert!(page.has_more_pages);
        assert_eq!(page.items.len(), 2);
        assert!(page.est_tokens > 0);
        assert!(matches!(
            page.items[0].3[0],
            MarkdownBlock::Text(ref t) if t == "Hello &lt;world&gt;"
        ));
    }
}
