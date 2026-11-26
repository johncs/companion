//! Stateful `pulldown-cmark` event builder using an explicit [`BlockState`] state machine.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::highlight::highlight_code_pango;
use super::inline::InlineState;
use super::table::TableBuilder;
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

pub(super) struct CodeBlockBuilder {
    pub(super) language: Option<String>,
    pub(super) buf: String,
}

pub(super) enum BlockState {
    Normal,
    Code(CodeBlockBuilder),
    Table(TableBuilder),
    Quote { depth: usize, buf: String },
}

pub(super) struct MarkdownBuilder<'a> {
    input: &'a str,
    cache: &'a mut CodeHighlightCache,
    skip_unclosed_highlight: bool,
    blocks: Vec<MarkdownBlock>,
    text_buf: String,
    pub(super) state: BlockState,
    inline: InlineState,
}

impl<'a> MarkdownBuilder<'a> {
    pub(super) fn new(
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
            state: BlockState::Normal,
            inline: InlineState::default(),
        }
    }

    fn push_event(&mut self, event: Event<'_>, range: Range<usize>) {
        if matches!(self.state, BlockState::Code(_)) {
            self.handle_code_event(event, range);
            return;
        }
        if let BlockState::Table(table) = &mut self.state
            && let Some(ended) = table.handle_event(&event, &mut self.inline.open_tags)
        {
            if ended {
                let finished = std::mem::replace(&mut self.state, BlockState::Normal);
                if let BlockState::Table(tb) = finished
                    && let Some(block) = tb.finish()
                {
                    self.blocks.push(block);
                }
            }
            return;
        }
        if self.handle_container_event(&event) {
            return;
        }
        let in_quote = matches!(self.state, BlockState::Quote { .. });
        let target = Self::active_target_buf(&mut self.state, &mut self.text_buf);
        let flush_after = self.inline.handle_event(event, target, in_quote);
        if flush_after {
            self.flush_normal_text();
        }
    }

    fn handle_code_event(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::End(TagEnd::CodeBlock) => {
                let finished = std::mem::replace(&mut self.state, BlockState::Normal);
                if let BlockState::Code(cb) = finished {
                    let slice = self.input.get(range.start..range.end).unwrap_or("");
                    let trimmed = slice.trim_end();
                    let is_opening_only = trimmed == "```" || trimmed == "~~~";
                    let is_unclosed_eof = self.skip_unclosed_highlight
                        && range.end == self.input.len()
                        && (is_opening_only
                            || (!trimmed.ends_with("```") && !trimmed.ends_with("~~~")));
                    let raw = cb.buf.trim_end_matches('\n').to_string();
                    let block = self.build_code_block(cb.language, raw, is_unclosed_eof);
                    self.blocks.push(block);
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let BlockState::Code(code) = &mut self.state {
                    code.buf.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let BlockState::Code(code) = &mut self.state {
                    code.buf.push('\n');
                }
            }
            _ => {}
        }
    }

    fn handle_container_event(&mut self, event: &Event<'_>) -> bool {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                self.flush_active_text_or_quote();
                let language = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let l = lang.split_whitespace().next().unwrap_or("").trim();
                        (!l.is_empty()).then(|| l.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.state = BlockState::Code(CodeBlockBuilder {
                    language,
                    buf: String::new(),
                });
                true
            }
            Event::Start(Tag::Table(_)) => {
                self.flush_active_text_or_quote();
                self.state = BlockState::Table(TableBuilder::default());
                true
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if let BlockState::Quote { depth, .. } = &mut self.state {
                    *depth += 1;
                } else {
                    self.flush_normal_text();
                    self.state = BlockState::Quote {
                        depth: 1,
                        buf: String::new(),
                    };
                }
                true
            }
            Event::End(TagEnd::BlockQuote(_)) => self.end_quote(),
            Event::Rule if !matches!(self.state, BlockState::Table(_)) => {
                self.flush_active_text_or_quote();
                self.blocks.push(MarkdownBlock::Rule);
                true
            }
            Event::Start(Tag::Paragraph) => true,
            Event::End(TagEnd::Paragraph) => self.end_paragraph(),
            _ => false,
        }
    }

    fn flush_active_text_or_quote(&mut self) {
        if let BlockState::Quote { .. } = self.state {
            let prev = std::mem::replace(&mut self.state, BlockState::Normal);
            if let BlockState::Quote { mut buf, .. } = prev {
                self.inline.close_all(&mut buf);
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    self.blocks.push(MarkdownBlock::Quote(trimmed.to_string()));
                }
            }
        } else {
            self.flush_normal_text();
        }
    }

    fn end_quote(&mut self) -> bool {
        if let BlockState::Quote { depth, .. } = &mut self.state {
            *depth = depth.saturating_sub(1);
            if *depth == 0 {
                let prev = std::mem::replace(&mut self.state, BlockState::Normal);
                if let BlockState::Quote { mut buf, .. } = prev {
                    self.inline.close_all(&mut buf);
                    let trimmed = buf.trim();
                    if !trimmed.is_empty() {
                        self.blocks.push(MarkdownBlock::Quote(trimmed.to_string()));
                    }
                }
            }
        }
        true
    }

    fn end_paragraph(&mut self) -> bool {
        if matches!(self.state, BlockState::Table(_)) {
            return true;
        }
        if !self.inline.in_list() && matches!(self.state, BlockState::Normal) {
            self.flush_normal_text();
        } else {
            let target = self.active_target();
            if !target.is_empty() && !target.ends_with('\n') {
                target.push('\n');
            }
        }
        true
    }

    fn active_target(&mut self) -> &mut String {
        Self::active_target_buf(&mut self.state, &mut self.text_buf)
    }

    fn active_target_buf<'b>(
        state: &'b mut BlockState,
        text_buf: &'b mut String,
    ) -> &'b mut String {
        match state {
            BlockState::Table(tb) if tb.is_in_cell() => tb.cell_buf_mut(),
            BlockState::Quote { buf, .. } => buf,
            _ => text_buf,
        }
    }

    fn flush_normal_text(&mut self) {
        self.inline.close_all(&mut self.text_buf);
        let trimmed = self.text_buf.trim();
        if !trimmed.is_empty() {
            self.blocks.push(MarkdownBlock::Text(trimmed.to_string()));
        }
        self.text_buf.clear();
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
        } else if let Some((light, dark)) = self.cache.get(language.as_deref(), &code) {
            (light.to_string(), dark.to_string())
        } else {
            let light = highlight_code_pango(&code, language.as_deref(), "InspiredGitHub");
            let dark = highlight_code_pango(&code, language.as_deref(), "base16-ocean.dark");
            self.cache
                .insert(language.as_deref(), &code, light.clone(), dark.clone());
            (light, dark)
        };
        MarkdownBlock::Code {
            language,
            code,
            light_pango_markup,
            dark_pango_markup,
        }
    }

    pub(super) fn finish(mut self) -> Vec<MarkdownBlock> {
        let state = std::mem::replace(&mut self.state, BlockState::Normal);
        match state {
            BlockState::Code(cb) if !cb.buf.is_empty() => {
                let code = cb.buf.trim_end_matches('\n').to_string();
                let block = self.build_code_block(cb.language, code, self.skip_unclosed_highlight);
                self.blocks.push(block);
            }
            BlockState::Table(tb) => {
                if let Some(block) = tb.finish() {
                    self.blocks.push(block);
                }
            }
            BlockState::Quote { mut buf, .. } if !buf.trim().is_empty() => {
                self.inline.close_all(&mut buf);
                self.blocks
                    .push(MarkdownBlock::Quote(buf.trim().to_string()));
            }
            _ => self.flush_normal_text(),
        }

        if self.blocks.is_empty() && !self.input.trim().is_empty() {
            self.blocks
                .push(MarkdownBlock::Text(escape_pango_markup(self.input.trim())));
        }
        self.blocks
    }
}
