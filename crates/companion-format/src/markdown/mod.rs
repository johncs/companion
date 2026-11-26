//! Cached Markdown-to-Pango parser with syntect syntax highlighting for code blocks.

mod highlight;
mod inline;
mod parser;
mod table;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub use highlight::{format_code_language, highlight_code_pango, warm_syntax_highlighting};

/// Append XML/Pango-escaped `input` directly into `out`.
pub(crate) fn push_escaped_pango(out: &mut String, input: &str) {
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
}

/// Escape XML/Pango markup special characters (`&`, `<`, `>`, `"`, `'`) in pure Rust.
pub fn escape_pango_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    push_escaped_pango(&mut out, input);
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

const MAX_HIGHLIGHT_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Clone, Default)]
struct CachedHighlight {
    language: Option<String>,
    code: String,
    light: String,
    dark: String,
}

/// Bounded cache of already-highlighted `(language, code) -> (light_markup, dark_markup)` to avoid `O(N^2)` work while streaming.
#[derive(Debug, Clone, Default)]
pub struct CodeHighlightCache {
    entries: HashMap<u64, CachedHighlight>,
}

impl CodeHighlightCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn get(&self, language: Option<&str>, code: &str) -> Option<(&str, &str)> {
        let key = hash_code_key(language, code);
        let entry = self.entries.get(&key)?;
        if entry.language.as_deref() == language && entry.code == code {
            Some((entry.light.as_str(), entry.dark.as_str()))
        } else {
            None
        }
    }

    pub fn insert(&mut self, language: Option<&str>, code: &str, light: String, dark: String) {
        if self.entries.len() >= MAX_HIGHLIGHT_CACHE_ENTRIES {
            self.entries.clear();
        }
        let key = hash_code_key(language, code);
        self.entries.insert(
            key,
            CachedHighlight {
                language: language.map(str::to_string),
                code: code.to_string(),
                light,
                dark,
            },
        );
    }
}

fn hash_code_key(language: Option<&str>, code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    language.hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}

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
