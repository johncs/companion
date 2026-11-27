//! Cursor-anchored `@`-mention autocomplete popover for granted files.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

const MAX_MENTION_RESULTS: usize = 20;

/// Pre-indexed file or directory candidate for `@`-mention autocomplete (zero `stat()` on keystrokes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionEntry {
    pub path: String,
    pub is_dir: bool,
}

struct MentionState {
    granted_files: Vec<MentionEntry>,
    matches: Vec<MentionEntry>,
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
        let (list_box, popover) = build_mention_popover(text_view);
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
        let activated_id = picker.list_box.connect_row_activated(move |_, row| {
            picker_row.accept_at(row.index().max(0) as usize);
        });
        // `list_box -> closure -> picker -> list_box` would otherwise be a strong cycle; break it
        // (and detach the popover) when the owning text view is destroyed.
        let (popover_destroy, list_box_destroy) = (picker.popover.clone(), picker.list_box.clone());
        let activated_id = std::cell::RefCell::new(Some(activated_id));
        text_view.connect_destroy(move |_| {
            if let Some(id) = activated_id.borrow_mut().take() {
                list_box_destroy.disconnect(id);
            }
            popover_destroy.unparent();
        });
        picker
    }

    /// Replace the cached project file list and refresh if the popover is open.
    pub fn set_files(&self, files: Vec<MentionEntry>) {
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

        let (Some(start_offset), matches) = self.compute_matches(&before_cursor) else {
            self.dismiss();
            return;
        };
        if matches.is_empty() {
            self.dismiss();
            return;
        }
        if self.state.borrow().matches != matches {
            rebuild_mention_rows(&self.list_box, &matches);
        }
        {
            let mut st = self.state.borrow_mut();
            st.matches = matches;
            st.start_offset = Some(start_offset);
        }
        let loc = self.text_view.iter_location(&cursor_iter);
        let (wx, wy) =
            self.text_view
                .buffer_to_window_coords(gtk::TextWindowType::Widget, loc.x(), loc.y());
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(wx, wy, 1, loc.height().max(16))));
        if !self.popover.is_visible() {
            self.popover.popup();
        }
    }

    fn compute_matches(&self, before_cursor: &str) -> (Option<i32>, Vec<MentionEntry>) {
        let st = self.state.borrow();
        if st.granted_files.is_empty() {
            return (None, Vec::new());
        }
        if let Some((offset, query)) = extract_mention_query(before_cursor) {
            (
                Some(offset),
                filter_mention_entries(&st.granted_files, query),
            )
        } else {
            (None, Vec::new())
        }
    }
}

fn build_mention_popover(text_view: &gtk::TextView) -> (gtk::ListBox, gtk::Popover) {
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
    (list_box, popover)
}

fn rebuild_mention_rows(list_box: &gtk::ListBox, matches: &[MentionEntry]) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for entry in matches {
        let row_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        let icon = gtk::Image::from_icon_name(if entry.is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        });
        icon.add_css_class("dim-label");
        let label = gtk::Label::builder()
            .label(&entry.path)
            .xalign(0.0)
            .hexpand(true)
            .max_width_chars(60)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .tooltip_text(&entry.path)
            .css_classes(["monospace", "caption"])
            .build();
        row_box.append(&icon);
        row_box.append(&label);
        list_box.append(&row_box);
    }
    if let Some(first_row) = list_box.row_at_index(0) {
        list_box.select_row(Some(&first_row));
    }
}

impl MentionPicker {
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
        let current = self.list_box.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (current + delta).clamp(0, count - 1);
        if let Some(row) = self.list_box.row_at_index(next) {
            self.list_box.select_row(Some(&row));
        }
    }

    fn accept_at(&self, index: usize) {
        let (path, start_offset) = {
            let st = self.state.borrow();
            (
                st.matches.get(index).map(|m| m.path.clone()),
                st.start_offset,
            )
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

fn mention_rank(path: &str, query_lower: &str) -> u8 {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if name.starts_with(query_lower) {
        0
    } else if name.contains(query_lower) {
        1
    } else {
        2
    }
}

/// Filter and rank pre-indexed [`MentionEntry`] items without performing filesystem `stat()` calls.
pub fn filter_mention_entries(files: &[MentionEntry], query: &str) -> Vec<MentionEntry> {
    let query_lower = query.to_lowercase();
    let mut matches: Vec<MentionEntry> = files
        .iter()
        .filter(|e| query_lower.is_empty() || e.path.to_lowercase().contains(&query_lower))
        .cloned()
        .collect();

    if !query_lower.is_empty() {
        matches.sort_by_key(|e| mention_rank(&e.path, &query_lower));
    }
    matches.truncate(MAX_MENTION_RESULTS);
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> MentionEntry {
        MentionEntry {
            path: path.to_string(),
            is_dir: false,
        }
    }

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
            entry("docs/readme.md"),
            entry("src/ui/window.rs"),
            entry("window_notes.txt"),
        ];
        let matches: Vec<String> = filter_mention_entries(&files, "win")
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(matches, vec!["src/ui/window.rs", "window_notes.txt"]);
    }

    #[test]
    fn ranks_before_truncating_when_many_candidates() {
        // 40 weak (directory-only) matches precede the one strong filename-prefix match.
        let mut files: Vec<MentionEntry> = (0..40)
            .map(|i| entry(&format!("win_dir/sub{i}/file.txt")))
            .collect();
        files.push(entry("src/window.rs"));
        let matches = filter_mention_entries(&files, "win");
        assert_eq!(matches.len(), MAX_MENTION_RESULTS);
        assert_eq!(matches[0].path, "src/window.rs");
    }
}
