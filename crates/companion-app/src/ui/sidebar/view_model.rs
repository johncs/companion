//! Pure, GTK-free state machine for sidebar search sequencing and running indicators.

use std::collections::HashSet;

/// Pure view-model tracking sidebar search generation tokens and running conversations.
#[derive(Debug, Default)]
pub(super) struct SidebarViewModel {
    pub(super) search_query: String,
    pub(super) search_seq: u64,
    pub(super) running_conversations: HashSet<String>,
}

impl SidebarViewModel {
    pub(super) fn begin_search(&mut self, query: String) -> u64 {
        self.search_query = query;
        self.search_seq = self.search_seq.wrapping_add(1);
        self.search_seq
    }

    pub(super) fn is_current_search(&self, seq: u64) -> bool {
        self.search_seq == seq
    }

    pub(super) fn set_running(&mut self, id: String, is_running: bool) -> bool {
        if is_running {
            self.running_conversations.insert(id)
        } else {
            self.running_conversations.remove(&id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SidebarViewModel;

    #[test]
    fn tracks_search_sequences_and_running_conversations() {
        let mut vm = SidebarViewModel::default();
        let s1 = vm.begin_search("rust".into());
        let s2 = vm.begin_search("gtk".into());
        assert!(!vm.is_current_search(s1));
        assert!(vm.is_current_search(s2));

        assert!(vm.set_running("c1".into(), true));
        assert!(!vm.set_running("c1".into(), true));
        assert!(vm.running_conversations.contains("c1"));
        assert!(vm.set_running("c1".into(), false));
        assert!(!vm.running_conversations.contains("c1"));
    }
}
