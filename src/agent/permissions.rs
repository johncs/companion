//! Per-chat session tool permission policy engine.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Deny,
    AllowOnce,
    AllowChat,
}

/// Evaluates and remembers whether an agent tool may run without prompting.
#[derive(Clone, Default)]
pub struct PermissionEngine {
    session_allows: Arc<Mutex<HashSet<(String, String)>>>,
}

impl PermissionEngine {
    pub fn new() -> Self {
        Self {
            session_allows: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Return `true` if `tool_name` is already authorized for `conv_id`.
    pub fn is_auto_allowed(&self, conv_id: &str, tool_name: &str) -> bool {
        // Read-only tools scoped to granted folders are safe by default.
        if matches!(tool_name, "list_files" | "read_file" | "search_files") {
            return true;
        }

        if let Ok(guard) = self.session_allows.lock()
            && guard.contains(&(conv_id.to_string(), tool_name.to_string()))
        {
            return true;
        }

        false
    }

    /// Record a user's permission decision for `(conv_id, tool_name)`.
    pub fn record_decision(
        &self,
        conv_id: &str,
        tool_name: &str,
        decision: PermissionDecision,
    ) {
        // Deletions must always be approved one-by-one.
        if matches!(tool_name, "delete_file" | "delete_directory") {
            return;
        }
        if decision == PermissionDecision::AllowChat
            && let Ok(mut guard) = self.session_allows.lock()
        {
            guard.insert((conv_id.to_string(), tool_name.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_auto_allow_and_session_policy_persists() {
        let conv_id = "conv-1";
        let engine = PermissionEngine::new();

        assert!(engine.is_auto_allowed(conv_id, "list_files"));
        assert!(engine.is_auto_allowed(conv_id, "read_file"));
        assert!(engine.is_auto_allowed(conv_id, "search_files"));
        assert!(!engine.is_auto_allowed(conv_id, "write_file"));
        assert!(!engine.is_auto_allowed(conv_id, "delete_file"));

        engine.record_decision(conv_id, "write_file", PermissionDecision::AllowOnce);
        assert!(!engine.is_auto_allowed(conv_id, "write_file"));

        engine.record_decision(conv_id, "write_file", PermissionDecision::AllowChat);
        assert!(engine.is_auto_allowed(conv_id, "write_file"));

        // delete_file and delete_directory remain always-ask even if AllowChat is passed.
        engine.record_decision(conv_id, "delete_file", PermissionDecision::AllowChat);
        assert!(!engine.is_auto_allowed(conv_id, "delete_file"));
        engine.record_decision(conv_id, "delete_directory", PermissionDecision::AllowChat);
        assert!(!engine.is_auto_allowed(conv_id, "delete_directory"));
    }
}
