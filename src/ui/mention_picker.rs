//! Cursor-anchored `@`-mention autocomplete popover for granted files.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

const MAX_MENTION_RESULTS: usize = 20;

struct MentionState {
    granted_files: Vec<String>,
    matches: Vec<String>,
    start_offset: Option<i32>,
}

/// Reusable `@`-mention file and granted-folder picker attached to a [`gtk::TextView`].
#[derive(Clone)]
pub struct MentionPicker {
    text_view: gtk::TextView,
    popover: gtk::Popover,
    list_box: gtk::ListBox,
    state: Rc<RefCell<MentionState>>,
}

impl MentionPicker {
    /// Attach a cursor-anchored `@`-mention popover to `text_view`.
    pub fn attach(text_view: &gtk::TextView) -> Self {
        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["navigation-sidebar"])
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .min_content_width(360)
            .max_content_width(560)
            .max_content_height(240)
            .child(&list_box)
            .build();

        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_focus(false)
            .has_arrow(true)
            .position(gtk::PositionType::Top)
            .child(&scroller)
            .build();
        popover.set_parent(text_view);

        let picker = Self {
            text_view: text_view.clone(),
            popover,
            list_box,
            state: Rc::new(RefCell::new(MentionState {
                granted_files: Vec::new(),
                matches: Vec::new(),
                start_offset: None,
            })),
        };

        let picker_row = picker.clone();
        picker.list_box.connect_row_activated(move |_, row| {
            picker_row.accept_at(row.index().max(0) as usize);
        });

        picker
    }

    /// Replace the cached project file list and refresh if the popover is open.
    pub fn set_files(&self, files: Vec<String>) {
        self.state.borrow_mut().granted_files = files;
        if self.popover.is_visible() {
            self.update_from_buffer();
        }
    }

    /// Dismiss the popover without inserting a mention.
    pub fn dismiss(&self) {
        {
            let mut st = self.state.borrow_mut();
            st.start_offset = None;
            st.matches.clear();
        }
        if self.popover.is_visible() {
            self.popover.popdown();
        }
    }

    /// Re-evaluate the buffer text around the cursor and show/update/hide the popover.
    pub fn update_from_buffer(&self) {
        let buf = self.text_view.buffer();
        let insert_mark = buf.get_insert();
        let cursor_iter = buf.iter_at_mark(&insert_mark);
        let start_iter = buf.start_iter();
        let before_cursor = buf.text(&start_iter, &cursor_iter, false);

        let (start_offset, matches) = {
            let st = self.state.borrow();
            if st.granted_files.is_empty() {
                (None, Vec::new())
            } else if let Some((offset, query)) = extract_mention_query(&before_cursor) {
                (Some(offset), filter_mention_matches(&st.granted_files, query))
            } else {
                (None, Vec::new())
            }
        };

        let (Some(start_offset), matches) = (start_offset, matches) else {
            self.dismiss();
            return;
        };
        if matches.is_empty() {
            self.dismiss();
            return;
        }

        let matches_unchanged = self.state.borrow().matches == matches;
        if !matches_unchanged {
            while let Some(child) = self.list_box.first_child() {
                self.list_box.remove(&child);
            }
            for path in &matches {
                let row_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .margin_top(6)
                    .margin_bottom(6)
                    .margin_start(8)
                    .margin_end(8)
                    .build();
                let icon_name = if Path::new(path).is_dir() {
                    "folder-symbolic"
                } else {
                    "text-x-generic-symbolic"
                };
                let icon = gtk::Image::from_icon_name(icon_name);
                icon.add_css_class("dim-label");
                let label = gtk::Label::builder()
                    .label(path)
                    .xalign(0.0)
                    .hexpand(true)
                    .max_width_chars(60)
                    .ellipsize(gtk::pango::EllipsizeMode::Start)
                    .tooltip_text(path)
                    .css_classes(["monospace", "caption"])
                    .build();
                row_box.append(&icon);
                row_box.append(&label);
                self.list_box.append(&row_box);
            }

            if let Some(first_row) = self.list_box.row_at_index(0) {
                self.list_box.select_row(Some(&first_row));
            }
        }

        {
            let mut st = self.state.borrow_mut();
            st.matches = matches;
            st.start_offset = Some(start_offset);
        }

        let loc = self.text_view.iter_location(&cursor_iter);
        let (wx, wy) = self.text_view.buffer_to_window_coords(
            gtk::TextWindowType::Widget,
            loc.x(),
            loc.y(),
        );
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            wx,
            wy,
            1,
            loc.height().max(16),
        )));
        if !self.popover.is_visible() {
            self.popover.popup();
        }
    }

    /// Handle navigation/acceptance keys when the popover is open.
    /// Returns `Some(Propagation::Stop)` if consumed by the picker.
    pub fn handle_key(&self, key: gdk::Key) -> Option<glib::Propagation> {
        if !self.popover.is_visible() {
            return None;
        }
        match key {
            gdk::Key::Down => {
                self.move_selection(1);
                Some(glib::Propagation::Stop)
            }
            gdk::Key::Up => {
                self.move_selection(-1);
                Some(glib::Propagation::Stop)
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                let idx = self
                    .list_box
                    .selected_row()
                    .map(|r| r.index().max(0) as usize)
                    .unwrap_or(0);
                self.accept_at(idx);
                Some(glib::Propagation::Stop)
            }
            gdk::Key::Escape => {
                self.dismiss();
                Some(glib::Propagation::Stop)
            }
            _ => None,
        }
    }

    fn move_selection(&self, delta: i32) {
        let count = self.state.borrow().matches.len() as i32;
        if count <= 0 {
            return;
        }
        let current = self
            .list_box
            .selected_row()
            .map(|r| r.index())
            .unwrap_or(0);
        let next = (current + delta).clamp(0, count - 1);
        if let Some(row) = self.list_box.row_at_index(next) {
            self.list_box.select_row(Some(&row));
            row.grab_focus();
            self.text_view.grab_focus();
        }
    }

    fn accept_at(&self, index: usize) {
        let (path, start_offset) = {
            let st = self.state.borrow();
            (st.matches.get(index).cloned(), st.start_offset)
        };
        let (Some(path), Some(start_offset)) = (path, start_offset) else {
            self.dismiss();
            return;
        };

        let buf = self.text_view.buffer();
        let insert_mark = buf.get_insert();
        let mut cursor_iter = buf.iter_at_mark(&insert_mark);
        let mut start_iter = buf.iter_at_offset(start_offset);

        buf.delete(&mut start_iter, &mut cursor_iter);
        buf.insert(&mut start_iter, &format!("@{path} "));
        self.dismiss();
        self.text_view.grab_focus();
    }
}

/// Extract `(at_char_offset, query)` if the cursor is currently inside an `@query` token.
pub fn extract_mention_query(before_cursor: &str) -> Option<(i32, &str)> {
    let at_byte_idx = before_cursor.rfind('@')?;
    let prefix = &before_cursor[..at_byte_idx];
    if let Some(prev_ch) = prefix.chars().next_back()
        && !prev_ch.is_whitespace()
    {
        return None;
    }
    let query = &before_cursor[at_byte_idx + 1..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some((prefix.chars().count() as i32, query))
}

/// Filter and rank project file paths for a case-insensitive `@query`.
pub fn filter_mention_matches(files: &[String], query: &str) -> Vec<String> {
    let query_lower = query.to_lowercase();
    let mut matches: Vec<String> = files
        .iter()
        .filter(|f| query_lower.is_empty() || f.to_lowercase().contains(&query_lower))
        .take(MAX_MENTION_RESULTS)
        .cloned()
        .collect();

    if !query_lower.is_empty() {
        matches.sort_by_key(|p| {
            let name = p.rsplit('/').next().unwrap_or(p).to_lowercase();
            if name.starts_with(&query_lower) {
                0
            } else if name.contains(&query_lower) {
                1
            } else {
                2
            }
        });
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_valid_at_mentions() {
        assert_eq!(extract_mention_query("@src/ui"), Some((0, "src/ui")));
        assert_eq!(extract_mention_query("read @REAL"), Some((5, "REAL")));
        assert_eq!(extract_mention_query("user@example.com"), None);
        assert_eq!(extract_mention_query("@foo bar"), None);
    }

    #[test]
    fn ranks_filename_prefix_matches_first() {
        let files = vec![
            "docs/readme.md".to_string(),
            "src/ui/window.rs".to_string(),
            "window_notes.txt".to_string(),
        ];
        let matches = filter_mention_matches(&files, "win");
        assert_eq!(matches, vec!["src/ui/window.rs", "window_notes.txt"]);
    }
}
