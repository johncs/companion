//! Markdown table state machine (`TableBuilder`).

use pulldown_cmark::{Event, Tag, TagEnd};

use super::MarkdownBlock;
use super::inline::close_open_tags;

#[derive(Default)]
pub(super) struct TableBuilder {
    in_head: bool,
    in_cell: bool,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    cell_buf: String,
}

impl TableBuilder {
    pub(super) fn is_in_cell(&self) -> bool {
        self.in_cell
    }

    pub(super) fn cell_buf_mut(&mut self) -> &mut String {
        &mut self.cell_buf
    }

    /// Consume a table lifecycle event. Returns `Some(true)` if the table ended.
    pub(super) fn handle_event(
        &mut self,
        event: &Event<'_>,
        open_tags: &mut Vec<&'static str>,
    ) -> Option<bool> {
        match event {
            Event::End(TagEnd::Table) => {
                if !self.current_row.is_empty() {
                    self.rows.push(std::mem::take(&mut self.current_row));
                }
                Some(true)
            }
            Event::Start(Tag::TableHead) => {
                self.in_head = true;
                self.current_row.clear();
                Some(false)
            }
            Event::End(TagEnd::TableHead) => {
                self.in_head = false;
                self.headers = std::mem::take(&mut self.current_row);
                Some(false)
            }
            Event::Start(Tag::TableRow) => {
                self.current_row.clear();
                Some(false)
            }
            Event::End(TagEnd::TableRow) => {
                if !self.in_head {
                    self.rows.push(std::mem::take(&mut self.current_row));
                }
                Some(false)
            }
            Event::Start(Tag::TableCell) => {
                self.in_cell = true;
                self.cell_buf.clear();
                Some(false)
            }
            Event::End(TagEnd::TableCell) => {
                close_open_tags(&mut self.cell_buf, open_tags);
                self.in_cell = false;
                self.current_row.push(self.cell_buf.trim().to_string());
                self.cell_buf.clear();
                Some(false)
            }
            _ => None,
        }
    }

    pub(super) fn finish(mut self) -> Option<MarkdownBlock> {
        if self.in_cell {
            let cell = self.cell_buf.trim();
            if !cell.is_empty() {
                self.current_row.push(cell.to_string());
            }
            self.cell_buf.clear();
            self.in_cell = false;
        }
        if !self.current_row.is_empty() {
            self.rows.push(self.current_row);
        }
        (!self.headers.is_empty() || !self.rows.is_empty()).then_some(MarkdownBlock::Table {
            headers: self.headers,
            rows: self.rows,
        })
    }
}
