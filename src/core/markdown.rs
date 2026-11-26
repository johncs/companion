//! Cached Markdown-to-Pango parser with syntect syntax highlighting for code blocks.

use std::collections::HashMap;
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

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
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Pre-initialize `syntect` syntax and theme sets off the GTK main thread.
pub fn warm_syntax_highlighting() {
    let _ = &*SYNTAX_SET;
    let _ = &*THEME_SET;
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
    parse_markdown_blocks_impl(input, cache, false)
}

/// Streaming variant of [`parse_markdown_blocks_cached`] that skips `syntect` highlighting
/// for an unclosed trailing code block until its closing fence arrives or streaming finishes.
pub fn parse_markdown_blocks_streaming(
    input: &str,
    cache: &mut CodeHighlightCache,
) -> Vec<MarkdownBlock> {
    parse_markdown_blocks_impl(input, cache, true)
}

fn parse_markdown_blocks_impl(
    input: &str,
    cache: &mut CodeHighlightCache,
    skip_unclosed_highlight: bool,
) -> Vec<MarkdownBlock> {
    let mut blocks = Vec::new();
    let mut text_buf = String::new();
    let mut quote_buf = String::new();
    let mut quote_depth: usize = 0;

    let mut in_code_block = false;
    let mut code_lang: Option<String> = None;
    let mut code_buf = String::new();

    let mut in_table = false;
    let mut in_table_head = false;
    let mut in_table_cell = false;
    let mut table_headers: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut cell_buf = String::new();

    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut open_tags: Vec<&'static str> = Vec::new();

    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(input, options).into_offset_iter();

    for (event, range) in parser {
        if in_code_block {
            match event {
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    let code = code_buf.trim_end_matches('\n').to_string();
                    let lang = code_lang.take();
                    let is_unclosed_eof = skip_unclosed_highlight
                        && range.end == input.len()
                        && !input[range.start..range.end].trim_end().ends_with("```")
                        && !input[range.start..range.end].trim_end().ends_with("~~~");
                    let (light_pango_markup, dark_pango_markup) = if is_unclosed_eof {
                        let escaped = escape_pango_markup(&code);
                        (escaped.clone(), escaped)
                    } else {
                        let key = (lang.clone(), code.clone());
                        if let Some(cached) = cache.get(&key) {
                            cached.clone()
                        } else {
                            let light =
                                highlight_code_pango(&code, lang.as_deref(), "InspiredGitHub");
                            let dark =
                                highlight_code_pango(&code, lang.as_deref(), "base16-ocean.dark");
                            cache.insert(key, (light.clone(), dark.clone()));
                            (light, dark)
                        }
                    };
                    blocks.push(MarkdownBlock::Code {
                        language: lang,
                        code,
                        light_pango_markup,
                        dark_pango_markup,
                    });
                    code_buf.clear();
                }
                Event::Text(t) | Event::Code(t) => code_buf.push_str(&t),
                Event::SoftBreak | Event::HardBreak => code_buf.push('\n'),
                _ => {}
            }
            continue;
        }

        let target: &mut String = if in_table_cell {
            &mut cell_buf
        } else if quote_depth > 0 {
            &mut quote_buf
        } else {
            &mut text_buf
        };

        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                close_open_tags(&mut text_buf, &mut open_tags);
                flush_text_block(&mut text_buf, &mut blocks);
                in_code_block = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.split_whitespace().next().unwrap_or("").trim();
                        if l.is_empty() {
                            None
                        } else {
                            Some(l.to_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Event::Start(Tag::Table(_)) => {
                close_open_tags(&mut text_buf, &mut open_tags);
                flush_text_block(&mut text_buf, &mut blocks);
                in_table = true;
                in_table_head = false;
                in_table_cell = false;
                table_headers.clear();
                table_rows.clear();
                current_row.clear();
                cell_buf.clear();
            }
            Event::End(TagEnd::Table) => {
                if !current_row.is_empty() {
                    table_rows.push(std::mem::take(&mut current_row));
                }
                if !table_headers.is_empty() || !table_rows.is_empty() {
                    blocks.push(MarkdownBlock::Table {
                        headers: std::mem::take(&mut table_headers),
                        rows: std::mem::take(&mut table_rows),
                    });
                }
                in_table = false;
                in_table_head = false;
                in_table_cell = false;
            }
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
                current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                in_table_head = false;
                table_headers = std::mem::take(&mut current_row);
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                if !in_table_head {
                    table_rows.push(std::mem::take(&mut current_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                in_table_cell = true;
                cell_buf.clear();
            }
            Event::End(TagEnd::TableCell) => {
                close_open_tags(&mut cell_buf, &mut open_tags);
                in_table_cell = false;
                current_row.push(cell_buf.trim().to_string());
                cell_buf.clear();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if quote_depth == 0 {
                    close_open_tags(&mut text_buf, &mut open_tags);
                    flush_text_block(&mut text_buf, &mut blocks);
                    quote_buf.clear();
                }
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
                if quote_depth == 0 {
                    close_open_tags(&mut quote_buf, &mut open_tags);
                    let trimmed = quote_buf.trim();
                    if !trimmed.is_empty() {
                        blocks.push(MarkdownBlock::Quote(trimmed.to_string()));
                    }
                    quote_buf.clear();
                }
            }
            Event::Rule if !in_table => {
                close_open_tags(&mut text_buf, &mut open_tags);
                flush_text_block(&mut text_buf, &mut blocks);
                blocks.push(MarkdownBlock::Rule);
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if !in_table {
                    if list_stack.is_empty() && quote_depth == 0 {
                        close_open_tags(&mut text_buf, &mut open_tags);
                        flush_text_block(&mut text_buf, &mut blocks);
                    } else if !target.is_empty() && !target.ends_with('\n') {
                        target.push('\n');
                    }
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let scale = match level {
                    pulldown_cmark::HeadingLevel::H1 => "xx-large",
                    pulldown_cmark::HeadingLevel::H2 => "x-large",
                    pulldown_cmark::HeadingLevel::H3 => "large",
                    _ => "medium",
                };
                target.push_str(&format!("<span size=\"{scale}\" weight=\"bold\">"));
                open_tags.push("</span>");
            }
            Event::End(TagEnd::Heading(_)) => {
                pop_tag(target, &mut open_tags, "</span>");
                if quote_depth == 0 && list_stack.is_empty() {
                    close_open_tags(&mut text_buf, &mut open_tags);
                    flush_text_block(&mut text_buf, &mut blocks);
                } else if !target.ends_with("\n\n") {
                    target.push_str("\n\n");
                }
            }
            Event::Start(Tag::Strong) => {
                target.push_str("<b>");
                open_tags.push("</b>");
            }
            Event::End(TagEnd::Strong) => pop_tag(target, &mut open_tags, "</b>"),
            Event::Start(Tag::Emphasis) => {
                target.push_str("<i>");
                open_tags.push("</i>");
            }
            Event::End(TagEnd::Emphasis) => pop_tag(target, &mut open_tags, "</i>"),
            Event::Start(Tag::Strikethrough) => {
                target.push_str("<s>");
                open_tags.push("</s>");
            }
            Event::End(TagEnd::Strikethrough) => pop_tag(target, &mut open_tags, "</s>"),
            Event::Start(Tag::List(start)) => {
                list_stack.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
                if list_stack.is_empty() && quote_depth == 0 {
                    close_open_tags(&mut text_buf, &mut open_tags);
                    flush_text_block(&mut text_buf, &mut blocks);
                } else if list_stack.is_empty() && !target.ends_with("\n\n") {
                    target.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                if !target.is_empty() && !target.ends_with("\n\n") {
                    if !target.ends_with('\n') {
                        target.push('\n');
                    }
                    // Subtle vertical breathing room between list items.
                    target.push_str("<span size=\"4096\">\n</span>");
                }
                let depth = list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                if let Some(Some(idx)) = list_stack.last_mut() {
                    target.push_str(&format!("{indent}<b>{idx}.</b>  "));
                    *idx += 1;
                } else {
                    target.push_str(&format!("{indent}•  "));
                }
            }
            Event::End(TagEnd::Item) => {
                if !target.ends_with('\n') {
                    target.push('\n');
                }
            }
            Event::TaskListMarker(checked) => {
                // Replace the just-pushed "•  " with a checkbox glyph.
                if target.ends_with("•  ") {
                    target.truncate(target.len() - "•  ".len());
                }
                target.push_str(if checked { "☑  " } else { "☐  " });
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let escaped = escape_pango_markup(&dest_url);
                target.push_str(&format!("<a href=\"{escaped}\">"));
                open_tags.push("</a>");
            }
            Event::End(TagEnd::Link) => pop_tag(target, &mut open_tags, "</a>"),
            Event::Code(inline) => {
                let escaped = escape_pango_markup(&inline);
                target.push_str(&format!("<tt><span size=\"94%\">{escaped}</span></tt>"));
            }
            Event::Text(t) => {
                target.push_str(&escape_pango_markup(&t));
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                let trimmed = raw.trim();
                if trimmed.eq_ignore_ascii_case("<br>")
                    || trimmed.eq_ignore_ascii_case("<br/>")
                    || trimmed.eq_ignore_ascii_case("<br />")
                {
                    target.push('\n');
                } else {
                    target.push_str(&escape_pango_markup(&raw));
                }
            }
            Event::SoftBreak => target.push(' '),
            Event::HardBreak => target.push('\n'),
            _ => {}
        }
    }

    if in_code_block && !code_buf.is_empty() {
        // Handle unclosed fenced code block while streaming tokens.
        let code = code_buf.trim_end_matches('\n').to_string();
        let light_pango_markup =
            highlight_code_pango(&code, code_lang.as_deref(), "InspiredGitHub");
        let dark_pango_markup =
            highlight_code_pango(&code, code_lang.as_deref(), "base16-ocean.dark");
        blocks.push(MarkdownBlock::Code {
            language: code_lang,
            code,
            light_pango_markup,
            dark_pango_markup,
        });
    } else if in_table {
        if !current_row.is_empty() {
            table_rows.push(current_row);
        }
        if !table_headers.is_empty() || !table_rows.is_empty() {
            blocks.push(MarkdownBlock::Table {
                headers: table_headers,
                rows: table_rows,
            });
        }
    } else if quote_depth > 0 && !quote_buf.trim().is_empty() {
        close_open_tags(&mut quote_buf, &mut open_tags);
        blocks.push(MarkdownBlock::Quote(quote_buf.trim().to_string()));
    } else {
        close_open_tags(&mut text_buf, &mut open_tags);
        flush_text_block(&mut text_buf, &mut blocks);
    }

    if blocks.is_empty() && !input.trim().is_empty() {
        blocks.push(MarkdownBlock::Text(escape_pango_markup(input.trim())));
    }

    blocks
}

fn close_open_tags(buf: &mut String, open_tags: &mut Vec<&'static str>) {
    while let Some(tag) = open_tags.pop() {
        buf.push_str(tag);
    }
}

fn pop_tag(buf: &mut String, open_tags: &mut Vec<&'static str>, expected: &'static str) {
    if open_tags.last().copied() == Some(expected) {
        open_tags.pop();
        buf.push_str(expected);
    }
}

fn flush_text_block(text_buf: &mut String, blocks: &mut Vec<MarkdownBlock>) {
    let trimmed = text_buf.trim();
    if !trimmed.is_empty() {
        blocks.push(MarkdownBlock::Text(trimmed.to_string()));
    }
    text_buf.clear();
}

fn resolve_syntax(lang: &str) -> &'static syntect::parsing::SyntaxReference {
    if let Some(syn) = SYNTAX_SET.find_syntax_by_token(lang) {
        return syn;
    }
    let fallback = match lang.trim().to_ascii_lowercase().as_str() {
        "typescript" | "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" => "js",
        "jsonc" | "json5" => "json",
        "toml" | "ini" | "conf" => "yaml",
        "zsh" | "fish" | "ksh" | "dockerfile" | "containerfile" => "sh",
        "scss" | "sass" | "less" => "css",
        "kotlin" | "kt" | "kts" | "swift" | "dart" => "java",
        _ => "",
    };
    if !fallback.is_empty()
        && let Some(syn) = SYNTAX_SET.find_syntax_by_token(fallback)
    {
        return syn;
    }
    SYNTAX_SET.find_syntax_plain_text()
}

/// Syntax-highlight `code` into Pango-compatible `<span>` markup using the named `syntect` theme.
pub fn highlight_code_pango(code: &str, language: Option<&str>, theme_name: &str) -> String {
    let syntax = language
        .map(resolve_syntax)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

    let theme = &THEME_SET.themes[theme_name];
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = String::with_capacity(code.len() * 2);

    for line in LinesWithEndings::from(code) {
        match highlighter.highlight_line(line, &SYNTAX_SET) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    append_styled_span(&mut out, style, text);
                }
            }
            Err(_) => {
                out.push_str(&escape_pango_markup(line));
            }
        }
    }

    out
}

fn append_styled_span(out: &mut String, style: Style, text: &str) {
    let escaped = escape_pango_markup(text);
    let fg = style.foreground;
    out.push_str(&format!(
        "<span foreground=\"#{:02x}{:02x}{:02x}\">{escaped}</span>",
        fg.r, fg.g, fg.b
    ));
}

/// Formats a fenced code block's language tag for display by capitalizing its
/// first character (e.g. `"typescript"` -> `"Typescript"`, `"rust"` -> `"Rust"`).
pub fn format_code_language(language: Option<&str>) -> String {
    let raw = language
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("text");
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(raw.len());
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            out
        }
        None => "Text".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_and_fenced_code_blocks() {
        let md = "Hello **world**!\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            MarkdownBlock::Text(t) => assert!(t.contains("<b>world</b>")),
            _ => panic!("expected text block"),
        }
        match &blocks[1] {
            MarkdownBlock::Code { language, code, .. } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert!(code.contains("fn main()"));
            }
            _ => panic!("expected code block"),
        }
    }

    #[test]
    fn handles_unclosed_streaming_code_block() {
        let md = "Working on it:\n\n```python\nprint(1)";
        let blocks = parse_markdown_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[1], MarkdownBlock::Code { .. }));

        let mut cache = CodeHighlightCache::new();
        let streaming_blocks = parse_markdown_blocks_streaming(md, &mut cache);
        assert!(cache.is_empty());
        match &streaming_blocks[1] {
            MarkdownBlock::Code {
                light_pango_markup,
                dark_pango_markup,
                ..
            } => {
                assert_eq!(light_pango_markup, "print(1)");
                assert_eq!(dark_pango_markup, "print(1)");
            }
            _ => panic!("expected code block"),
        }
    }

    #[test]
    fn parses_tables_quotes_rules_and_task_lists() {
        let md = "> A **wise** note\n\n---\n\n| Name | Role |\n| --- | --- |\n| Ada | `admin` |\n\n- [x] Done\n- [ ] Todo\n";
        let blocks = parse_markdown_blocks(md);
        assert!(matches!(&blocks[0], MarkdownBlock::Quote(q) if q.contains("<b>wise</b>")));
        assert!(matches!(&blocks[1], MarkdownBlock::Rule));
        match &blocks[2] {
            MarkdownBlock::Table { headers, rows } => {
                assert_eq!(headers, &vec!["Name".to_string(), "Role".to_string()]);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0], "Ada");
                assert!(rows[0][1].contains("<tt>") && rows[0][1].contains("admin"));
            }
            _ => panic!("expected table block"),
        }
        match &blocks[3] {
            MarkdownBlock::Text(t) => {
                assert!(t.contains("☑") && t.contains("Done"));
                assert!(t.contains("☐") && t.contains("Todo"));
            }
            _ => panic!("expected task list text block"),
        }
    }

    #[test]
    fn highlights_typescript_and_respects_light_and_dark_themes() {
        let ts_dark = highlight_code_pango(
            "const email: string = 'a@b.com';",
            Some("typescript"),
            "base16-ocean.dark",
        );
        let ts_light = highlight_code_pango(
            "const email: string = 'a@b.com';",
            Some("typescript"),
            "InspiredGitHub",
        );
        assert_ne!(ts_dark, ts_light);
        assert!(ts_dark.matches("<span foreground=").count() > 2);
    }

    #[test]
    fn formats_code_language_header() {
        assert_eq!(format_code_language(Some("typescript")), "Typescript");
        assert_eq!(format_code_language(Some("rust")), "Rust");
        assert_eq!(format_code_language(Some("  python ")), "Python");
        assert_eq!(format_code_language(None), "Text");
        assert_eq!(format_code_language(Some("")), "Text");
    }

    #[test]
    fn escapes_pango_markup_special_chars_and_unicode() {
        assert_eq!(escape_pango_markup(""), "");
        assert_eq!(
            escape_pango_markup(r#"a & b <tag> "q" 's' 🚀"#),
            "a &amp; b &lt;tag&gt; &quot;q&quot; &apos;s&apos; 🚀"
        );
    }
}
