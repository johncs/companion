//! Stateful `pulldown-cmark` event builder that produces [`MarkdownBlock`]s.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::highlight::highlight_code_pango;
use super::{CodeHighlightCache, MarkdownBlock, escape_pango_markup};

pub(super) fn parse_markdown_blocks_impl(
    input: &str,
    cache: &mut CodeHighlightCache,
    skip_unclosed_highlight: bool,
) -> Vec<MarkdownBlock> {
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(input, options).into_offset_iter();

    let mut builder = MarkdownBuilder::new(input, cache, skip_unclosed_highlight);
    for (event, range) in parser {
        builder.push_event(event, range);
    }
    builder.finish()
}

struct MarkdownBuilder<'a> {
    input: &'a str,
    cache: &'a mut CodeHighlightCache,
    skip_unclosed_highlight: bool,

    blocks: Vec<MarkdownBlock>,
    text_buf: String,
    quote_buf: String,
    quote_depth: usize,

    in_code_block: bool,
    code_lang: Option<String>,
    code_buf: String,

    in_table: bool,
    in_table_head: bool,
    in_table_cell: bool,
    table_headers: Vec<String>,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    cell_buf: String,

    list_stack: Vec<Option<u64>>,
    open_tags: Vec<&'static str>,
}

impl<'a> MarkdownBuilder<'a> {
    fn new(
        input: &'a str,
        cache: &'a mut CodeHighlightCache,
        skip_unclosed_highlight: bool,
    ) -> Self {
        Self {
            input,
            cache,
            skip_unclosed_highlight,
            blocks: Vec::new(),
            text_buf: String::new(),
            quote_buf: String::new(),
            quote_depth: 0,
            in_code_block: false,
            code_lang: None,
            code_buf: String::new(),
            in_table: false,
            in_table_head: false,
            in_table_cell: false,
            table_headers: Vec::new(),
            table_rows: Vec::new(),
            current_row: Vec::new(),
            cell_buf: String::new(),
            list_stack: Vec::new(),
            open_tags: Vec::new(),
        }
    }

    fn push_event(&mut self, event: Event<'_>, range: Range<usize>) {
        if self.in_code_block {
            self.handle_code_block(event, range);
            return;
        }
        if self.handle_structure_event(&event) {
            return;
        }
        self.handle_inline(event);
    }

    fn handle_code_block(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                let code = self.code_buf.trim_end_matches('\n').to_string();
                let lang = self.code_lang.take();
                let slice = self.input.get(range.start..range.end).unwrap_or("");
                let is_unclosed_eof = self.skip_unclosed_highlight
                    && range.end == self.input.len()
                    && !slice.trim_end().ends_with("```")
                    && !slice.trim_end().ends_with("~~~");
                let block = self.build_code_block(lang, code, is_unclosed_eof);
                self.blocks.push(block);
                self.code_buf.clear();
            }
            Event::Text(t) | Event::Code(t) => self.code_buf.push_str(&t),
            Event::SoftBreak | Event::HardBreak => self.code_buf.push('\n'),
            _ => {}
        }
    }

    fn build_code_block(
        &mut self,
        language: Option<String>,
        code: String,
        skip_highlight: bool,
    ) -> MarkdownBlock {
        let (light_pango_markup, dark_pango_markup) = if skip_highlight {
            let escaped = escape_pango_markup(&code);
            (escaped.clone(), escaped)
        } else {
            let key = (language.clone(), code.clone());
            if let Some(cached) = self.cache.get(&key) {
                cached.clone()
            } else {
                let light = highlight_code_pango(&code, language.as_deref(), "InspiredGitHub");
                let dark = highlight_code_pango(&code, language.as_deref(), "base16-ocean.dark");
                self.cache.insert(key, (light.clone(), dark.clone()));
                (light, dark)
            }
        };
        MarkdownBlock::Code {
            language,
            code,
            light_pango_markup,
            dark_pango_markup,
        }
    }

    /// Handle block/container events (code block start, tables, quotes, rules, paragraphs).
    /// Returns `true` if the event was consumed.
    fn handle_structure_event(&mut self, event: &Event<'_>) -> bool {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                close_open_tags(&mut self.text_buf, &mut self.open_tags);
                flush_text_block(&mut self.text_buf, &mut self.blocks);
                self.in_code_block = true;
                self.code_buf.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.split_whitespace().next().unwrap_or("").trim();
                        (!l.is_empty()).then(|| l.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                true
            }
            Event::Start(Tag::Table(_))
            | Event::End(TagEnd::Table)
            | Event::Start(Tag::TableHead)
            | Event::End(TagEnd::TableHead)
            | Event::Start(Tag::TableRow)
            | Event::End(TagEnd::TableRow)
            | Event::Start(Tag::TableCell)
            | Event::End(TagEnd::TableCell) => {
                self.handle_table(event);
                true
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if self.quote_depth == 0 {
                    close_open_tags(&mut self.text_buf, &mut self.open_tags);
                    flush_text_block(&mut self.text_buf, &mut self.blocks);
                    self.quote_buf.clear();
                }
                self.quote_depth += 1;
                true
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                if self.quote_depth == 0 {
                    close_open_tags(&mut self.quote_buf, &mut self.open_tags);
                    let trimmed = self.quote_buf.trim();
                    if !trimmed.is_empty() {
                        self.blocks.push(MarkdownBlock::Quote(trimmed.to_string()));
                    }
                    self.quote_buf.clear();
                }
                true
            }
            Event::Rule if !self.in_table => {
                close_open_tags(&mut self.text_buf, &mut self.open_tags);
                flush_text_block(&mut self.text_buf, &mut self.blocks);
                self.blocks.push(MarkdownBlock::Rule);
                true
            }
            Event::Start(Tag::Paragraph) => true,
            Event::End(TagEnd::Paragraph) => {
                if !self.in_table {
                    if self.list_stack.is_empty() && self.quote_depth == 0 {
                        close_open_tags(&mut self.text_buf, &mut self.open_tags);
                        flush_text_block(&mut self.text_buf, &mut self.blocks);
                    } else {
                        let target = self.active_target();
                        if !target.is_empty() && !target.ends_with('\n') {
                            target.push('\n');
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn handle_table(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(Tag::Table(_)) => {
                close_open_tags(&mut self.text_buf, &mut self.open_tags);
                flush_text_block(&mut self.text_buf, &mut self.blocks);
                self.in_table = true;
                self.in_table_head = false;
                self.in_table_cell = false;
                self.table_headers.clear();
                self.table_rows.clear();
                self.current_row.clear();
                self.cell_buf.clear();
            }
            Event::End(TagEnd::Table) => {
                if !self.current_row.is_empty() {
                    self.table_rows.push(std::mem::take(&mut self.current_row));
                }
                if !self.table_headers.is_empty() || !self.table_rows.is_empty() {
                    self.blocks.push(MarkdownBlock::Table {
                        headers: std::mem::take(&mut self.table_headers),
                        rows: std::mem::take(&mut self.table_rows),
                    });
                }
                self.in_table = false;
                self.in_table_head = false;
                self.in_table_cell = false;
            }
            Event::Start(Tag::TableHead) => {
                self.in_table_head = true;
                self.current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                self.in_table_head = false;
                self.table_headers = std::mem::take(&mut self.current_row);
            }
            Event::Start(Tag::TableRow) => self.current_row.clear(),
            Event::End(TagEnd::TableRow) => {
                if !self.in_table_head {
                    self.table_rows.push(std::mem::take(&mut self.current_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                self.in_table_cell = true;
                self.cell_buf.clear();
            }
            Event::End(TagEnd::TableCell) => {
                close_open_tags(&mut self.cell_buf, &mut self.open_tags);
                self.in_table_cell = false;
                self.current_row.push(self.cell_buf.trim().to_string());
                self.cell_buf.clear();
            }
            _ => {}
        }
    }

    fn active_target(&mut self) -> &mut String {
        if self.in_table_cell {
            &mut self.cell_buf
        } else if self.quote_depth > 0 {
            &mut self.quote_buf
        } else {
            &mut self.text_buf
        }
    }

    fn handle_inline(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let scale = match level {
                    pulldown_cmark::HeadingLevel::H1 => "xx-large",
                    pulldown_cmark::HeadingLevel::H2 => "x-large",
                    pulldown_cmark::HeadingLevel::H3 => "large",
                    _ => "medium",
                };
                self.active_target()
                    .push_str(&format!("<span size=\"{scale}\" weight=\"bold\">"));
                self.open_tags.push("</span>");
            }
            Event::End(TagEnd::Heading(_)) => {
                let mut open_tags = std::mem::take(&mut self.open_tags);
                pop_tag(self.active_target(), &mut open_tags, "</span>");
                self.open_tags = open_tags;
                if self.quote_depth == 0 && self.list_stack.is_empty() {
                    close_open_tags(&mut self.text_buf, &mut self.open_tags);
                    flush_text_block(&mut self.text_buf, &mut self.blocks);
                } else {
                    let target = self.active_target();
                    if !target.ends_with("\n\n") {
                        target.push_str("\n\n");
                    }
                }
            }
            Event::Start(Tag::Strong) => self.push_tag("<b>", "</b>"),
            Event::End(TagEnd::Strong) => self.close_tag("</b>"),
            Event::Start(Tag::Emphasis) => self.push_tag("<i>", "</i>"),
            Event::End(TagEnd::Emphasis) => self.close_tag("</i>"),
            Event::Start(Tag::Strikethrough) => self.push_tag("<s>", "</s>"),
            Event::End(TagEnd::Strikethrough) => self.close_tag("</s>"),
            Event::Start(Tag::List(start)) => self.list_stack.push(start),
            Event::End(TagEnd::List(_)) => self.handle_list_end(),
            Event::Start(Tag::Item) => self.handle_list_item_start(),
            Event::End(TagEnd::Item) => {
                let target = self.active_target();
                if !target.ends_with('\n') {
                    target.push('\n');
                }
            }
            Event::TaskListMarker(checked) => {
                let target = self.active_target();
                if target.ends_with("•  ") {
                    target.truncate(target.len() - "•  ".len());
                }
                target.push_str(if checked { "☑  " } else { "☐  " });
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let escaped = escape_pango_markup(&dest_url);
                self.active_target()
                    .push_str(&format!("<a href=\"{escaped}\">"));
                self.open_tags.push("</a>");
            }
            Event::End(TagEnd::Link) => self.close_tag("</a>"),
            Event::Code(inline) => {
                let escaped = escape_pango_markup(&inline);
                self.active_target()
                    .push_str(&format!("<tt><span size=\"94%\">{escaped}</span></tt>"));
            }
            Event::Text(t) => {
                let escaped = escape_pango_markup(&t);
                self.active_target().push_str(&escaped);
            }
            Event::Html(raw) | Event::InlineHtml(raw) => {
                let trimmed = raw.trim();
                let is_br = trimmed.eq_ignore_ascii_case("<br>")
                    || trimmed.eq_ignore_ascii_case("<br/>")
                    || trimmed.eq_ignore_ascii_case("<br />");
                if is_br {
                    self.active_target().push('\n');
                } else {
                    let escaped = escape_pango_markup(&raw);
                    self.active_target().push_str(&escaped);
                }
            }
            Event::SoftBreak => self.active_target().push(' '),
            Event::HardBreak => self.active_target().push('\n'),
            _ => {}
        }
    }

    fn push_tag(&mut self, open: &str, close: &'static str) {
        self.active_target().push_str(open);
        self.open_tags.push(close);
    }

    fn close_tag(&mut self, expected: &'static str) {
        let mut open_tags = std::mem::take(&mut self.open_tags);
        pop_tag(self.active_target(), &mut open_tags, expected);
        self.open_tags = open_tags;
    }

    fn handle_list_end(&mut self) {
        self.list_stack.pop();
        if self.list_stack.is_empty() && self.quote_depth == 0 {
            close_open_tags(&mut self.text_buf, &mut self.open_tags);
            flush_text_block(&mut self.text_buf, &mut self.blocks);
        } else if self.list_stack.is_empty() {
            let target = self.active_target();
            if !target.ends_with("\n\n") {
                target.push('\n');
            }
        }
    }

    fn handle_list_item_start(&mut self) {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = if let Some(Some(idx)) = self.list_stack.last_mut() {
            let s = format!("{indent}<b>{idx}.</b>  ");
            *idx += 1;
            s
        } else {
            format!("{indent}•  ")
        };

        let target = self.active_target();
        if !target.is_empty() && !target.ends_with("\n\n") {
            if !target.ends_with('\n') {
                target.push('\n');
            }
            // Subtle vertical breathing room between list items.
            target.push_str("<span size=\"4096\">\n</span>");
        }
        target.push_str(&marker);
    }

    fn finish(mut self) -> Vec<MarkdownBlock> {
        if self.in_code_block && !self.code_buf.is_empty() {
            // Handle unclosed fenced code block while streaming tokens.
            let code = self.code_buf.trim_end_matches('\n').to_string();
            let lang = self.code_lang.take();
            let block = self.build_code_block(lang, code, self.skip_unclosed_highlight);
            self.blocks.push(block);
        } else if self.in_table {
            if !self.current_row.is_empty() {
                self.table_rows.push(self.current_row);
            }
            if !self.table_headers.is_empty() || !self.table_rows.is_empty() {
                self.blocks.push(MarkdownBlock::Table {
                    headers: self.table_headers,
                    rows: self.table_rows,
                });
            }
        } else if self.quote_depth > 0 && !self.quote_buf.trim().is_empty() {
            close_open_tags(&mut self.quote_buf, &mut self.open_tags);
            self.blocks
                .push(MarkdownBlock::Quote(self.quote_buf.trim().to_string()));
        } else {
            close_open_tags(&mut self.text_buf, &mut self.open_tags);
            flush_text_block(&mut self.text_buf, &mut self.blocks);
        }

        if self.blocks.is_empty() && !self.input.trim().is_empty() {
            self.blocks
                .push(MarkdownBlock::Text(escape_pango_markup(self.input.trim())));
        }

        self.blocks
    }
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

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::MarkdownBuilder;

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
        let md = "Working on it:\n\n```python\nprint(1 < 2 & 3)";
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
                assert_eq!(light_pango_markup, "print(1 &lt; 2 &amp; 3)");
                assert_eq!(dark_pango_markup, "print(1 &lt; 2 &amp; 3)");
            }
            _ => panic!("expected code block"),
        }
    }

    #[test]
    fn finish_respects_skip_unclosed_highlight_when_code_block_still_open() {
        let mut cache = CodeHighlightCache::new();
        let mut builder = MarkdownBuilder::new("```rust\nlet x = a < b;", &mut cache, true);
        builder.in_code_block = true;
        builder.code_lang = Some("rust".to_string());
        builder.code_buf = "let x = a < b;\n".to_string();

        let blocks = builder.finish();
        assert!(cache.is_empty());
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            MarkdownBlock::Code {
                light_pango_markup,
                dark_pango_markup,
                ..
            } => {
                assert_eq!(light_pango_markup, "let x = a &lt; b;");
                assert_eq!(dark_pango_markup, "let x = a &lt; b;");
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
    fn reuses_prepopulated_code_highlight_cache() {
        let mut cache = CodeHighlightCache::new();
        cache.insert(
            (Some("rust".to_string()), "let x = 1;".to_string()),
            ("cached-light".to_string(), "cached-dark".to_string()),
        );
        let blocks = parse_markdown_blocks_cached("```rust\nlet x = 1;\n```", &mut cache);
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            MarkdownBlock::Code {
                light_pango_markup,
                dark_pango_markup,
                ..
            } => {
                assert_eq!(light_pango_markup, "cached-light");
                assert_eq!(dark_pango_markup, "cached-dark");
            }
            _ => panic!("expected code block"),
        }
    }

    #[test]
    fn closes_unclosed_inline_tags_and_handles_tilde_fences() {
        let blocks = parse_markdown_blocks("**bold\n\n~~~sh\necho hi\n~~~");
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            MarkdownBlock::Text(t) => assert!(t.contains("<b>bold</b>") || t.contains("**bold")),
            _ => panic!("expected text block"),
        }
        assert!(matches!(
            &blocks[1],
            MarkdownBlock::Code {
                language: Some(lang),
                code,
                ..
            } if lang == "sh" && code == "echo hi"
        ));
    }
}
