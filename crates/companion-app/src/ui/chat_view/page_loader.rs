//! Thin UI bridge for paginated conversation loading and scroll helpers.

use adw::prelude::*;

use crate::storage::Database;

pub(super) const PAGE_SIZE: usize = 20;

pub type LoadedPageData = crate::agent::session::LoadedConversationPage;

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

/// Fetch a conversation page from SQLite and parse all Markdown/syntect blocks in parallel.
pub(super) fn load_and_parse_page(
    db: &Database,
    conv_id: &str,
    before_message_id: Option<&str>,
    sys_prompt_len: usize,
) -> Result<LoadedPageData, String> {
    crate::agent::session::load_conversation_page(
        db,
        conv_id,
        before_message_id,
        sys_prompt_len,
        PAGE_SIZE,
    )
}
