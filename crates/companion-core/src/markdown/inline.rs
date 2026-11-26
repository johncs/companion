//! Inline Pango tag stack, headings, lists, links, and HTML break handling.

use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd};

use super::escape_pango_markup;

#[derive(Default)]
pub(super) struct InlineState {
    pub(super) list_stack: Vec<Option<u64>>,
    pub(super) open_tags: Vec<&'static str>,
}

impl InlineState {
    pub(super) fn in_list(&self) -> bool {
        !self.list_stack.is_empty()
    }

    pub(super) fn close_all(&mut self, buf: &mut String) {
        close_open_tags(buf, &mut self.open_tags);
    }

    /// Process an inline/list/heading event into `target`.
    /// Returns `true` if a top-level block boundary (heading or outer list end) was reached.
    pub(super) fn handle_event(
        &mut self,
        event: Event<'_>,
        target: &mut String,
        in_quote: bool,
    ) -> bool {
        match event {
            Event::Start(Tag::Heading { level, .. }) => self.start_heading(level, target),
            Event::End(TagEnd::Heading(_)) => return self.end_heading(target, in_quote),
            Event::Start(Tag::Strong) => self.push_tag(target, "<b>", "</b>"),
            Event::End(TagEnd::Strong) => pop_tag(target, &mut self.open_tags, "</b>"),
            Event::Start(Tag::Emphasis) => self.push_tag(target, "<i>", "</i>"),
            Event::End(TagEnd::Emphasis) => pop_tag(target, &mut self.open_tags, "</i>"),
            Event::Start(Tag::Strikethrough) => self.push_tag(target, "<s>", "</s>"),
            Event::End(TagEnd::Strikethrough) => pop_tag(target, &mut self.open_tags, "</s>"),
            Event::Start(Tag::List(start)) => self.list_stack.push(start),
            Event::End(TagEnd::List(_)) => return self.end_list(target, in_quote),
            Event::Start(Tag::Item) => self.start_item(target),
            Event::End(TagEnd::Item) => {
                if !target.ends_with('\n') {
                    target.push('\n');
                }
            }
            Event::TaskListMarker(checked) => {
                if target.ends_with("•  ") {
                    target.truncate(target.len() - "•  ".len());
                }
                target.push_str(if checked { "☑  " } else { "☐  " });
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let escaped = escape_pango_markup(&dest_url);
                target.push_str(&format!("<a href=\"{escaped}\">"));
                self.open_tags.push("</a>");
            }
            Event::End(TagEnd::Link) => pop_tag(target, &mut self.open_tags, "</a>"),
            Event::Code(inline) => {
                let escaped = escape_pango_markup(&inline);
                target.push_str(&format!("<tt><span size=\"94%\">{escaped}</span></tt>"));
            }
            Event::Text(t) => target.push_str(&escape_pango_markup(&t)),
            Event::Html(raw) | Event::InlineHtml(raw) => push_html(target, &raw),
            Event::SoftBreak => target.push(' '),
            Event::HardBreak => target.push('\n'),
            _ => {}
        }
        false
    }

    fn start_heading(&mut self, level: HeadingLevel, target: &mut String) {
        let scale = match level {
            HeadingLevel::H1 => "xx-large",
            HeadingLevel::H2 => "x-large",
            HeadingLevel::H3 => "large",
            _ => "medium",
        };
        target.push_str(&format!("<span size=\"{scale}\" weight=\"bold\">"));
        self.open_tags.push("</span>");
    }

    fn end_heading(&mut self, target: &mut String, in_quote: bool) -> bool {
        pop_tag(target, &mut self.open_tags, "</span>");
        if !in_quote && self.list_stack.is_empty() {
            true
        } else {
            if !target.ends_with("\n\n") {
                target.push_str("\n\n");
            }
            false
        }
    }

    fn end_list(&mut self, target: &mut String, in_quote: bool) -> bool {
        self.list_stack.pop();
        if self.list_stack.is_empty() && !in_quote {
            return true;
        }
        if self.list_stack.is_empty() && !target.ends_with("\n\n") {
            target.push('\n');
        }
        false
    }

    fn start_item(&mut self, target: &mut String) {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = if let Some(Some(idx)) = self.list_stack.last_mut() {
            let s = format!("{indent}<b>{idx}.</b>  ");
            *idx += 1;
            s
        } else {
            format!("{indent}•  ")
        };

        if !target.is_empty() && !target.ends_with("\n\n") {
            if !target.ends_with('\n') {
                target.push('\n');
            }
            target.push_str("<span size=\"4096\">\n</span>");
        }
        target.push_str(&marker);
    }

    fn push_tag(&mut self, target: &mut String, open: &str, close: &'static str) {
        target.push_str(open);
        self.open_tags.push(close);
    }
}

fn push_html(target: &mut String, raw: &str) {
    let trimmed = raw.trim();
    let is_br = trimmed.eq_ignore_ascii_case("<br>")
        || trimmed.eq_ignore_ascii_case("<br/>")
        || trimmed.eq_ignore_ascii_case("<br />");
    if is_br {
        target.push('\n');
    } else {
        target.push_str(&escape_pango_markup(raw));
    }
}

pub(super) fn close_open_tags(buf: &mut String, open_tags: &mut Vec<&'static str>) {
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
