//! Pure, GTK-free sequence guards for conversation loading and pagination.

/// Pure state helper validating async conversation load and older-page responses.
#[derive(Debug, Default)]
pub(super) struct ChatViewState {
    pub(super) active_conversation_id: Option<String>,
    pub(super) load_seq: u64,
    pub(super) page_seq: u64,
}

impl ChatViewState {
    pub(super) fn begin_load(&mut self, conv_id: String) -> u64 {
        self.active_conversation_id = Some(conv_id);
        self.load_seq = self.load_seq.wrapping_add(1);
        self.page_seq = self.page_seq.wrapping_add(1);
        self.load_seq
    }

    pub(super) fn begin_older_page(&mut self) -> u64 {
        self.page_seq = self.page_seq.wrapping_add(1);
        self.page_seq
    }

    pub(super) fn clear(&mut self) {
        self.active_conversation_id = None;
        self.load_seq = self.load_seq.wrapping_add(1);
        self.page_seq = self.page_seq.wrapping_add(1);
    }

    pub(super) fn is_current_load(&self, conv_id: &str, seq: u64) -> bool {
        self.load_seq == seq && self.active_conversation_id.as_deref() == Some(conv_id)
    }

    pub(super) fn is_current_page(&self, conv_id: &str, seq: u64) -> bool {
        self.page_seq == seq && self.active_conversation_id.as_deref() == Some(conv_id)
    }
}

#[cfg(test)]
mod tests {
    use super::ChatViewState;

    #[test]
    fn rejects_stale_conversation_load_and_page_responses() {
        let mut state = ChatViewState::default();
        let l1 = state.begin_load("c1".into());
        let p1 = state.begin_older_page();
        assert!(state.is_current_load("c1", l1));
        assert!(state.is_current_page("c1", p1));

        let l2 = state.begin_load("c2".into());
        assert!(!state.is_current_load("c1", l1));
        assert!(!state.is_current_page("c1", p1));
        assert!(state.is_current_load("c2", l2));
    }
}
