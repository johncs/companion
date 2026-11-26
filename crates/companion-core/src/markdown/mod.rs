//! Cached Markdown-to-Pango parser with syntect syntax highlighting for code blocks.

mod highlight;
mod inline;
mod parser;
mod table;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

pub use highlight::{format_code_language, highlight_code_pango, warm_syntax_highlighting};

/// Escape XML/Pango markup special characters (`&`, `<`, `>`, `"`, `'`) in pure Rust.
pub fn escape_pango_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A pre-rendered Markdown block cached per message for fast `gtk::ListView` recycling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    /// Rich text rendered as Pango markup.
    Text(String),
    /// Styled blockquote rendered with a GNOME accent bar.
    Quote(String),
    /// Horizontal rule rendered as a native `gtk::Separator`.
    Rule,
    /// Markdown table with Pango-formatted header and body cells.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// Fenced or indented code block with pre-highlighted Pango markup for light & dark themes.
    Code {
        language: Option<String>,
        code: String,
        light_pango_markup: String,
        dark_pango_markup: String,
    },
}

/// Cache of already-highlighted `(language, code) -> (light_markup, dark_markup)` to avoid `O(N^2)` work while streaming.
pub type CodeHighlightCache = HashMap<(Option<String>, String), (String, String)>;

/// Parse Markdown into cached [`MarkdownBlock`]s ready for GTK4 labels.
pub fn parse_markdown_blocks(input: &str) -> Vec<MarkdownBlock> {
    let mut cache = CodeHighlightCache::new();
    parse_markdown_blocks_cached(input, &mut cache)
}

/// Parse Markdown into [`MarkdownBlock`]s, reusing previously highlighted code blocks from `cache`.
pub fn parse_markdown_blocks_cached(
    input: &str,
    cache: &mut CodeHighlightCache,
) -> Vec<MarkdownBlock> {
    parser::parse_markdown_blocks_impl(input, cache, false)
}

/// Streaming variant of [`parse_markdown_blocks_cached`] that skips `syntect` highlighting
/// for an unclosed trailing code block until its closing fence arrives or streaming finishes.
pub fn parse_markdown_blocks_streaming(
    input: &str,
    cache: &mut CodeHighlightCache,
) -> Vec<MarkdownBlock> {
    parser::parse_markdown_blocks_impl(input, cache, true)
}
