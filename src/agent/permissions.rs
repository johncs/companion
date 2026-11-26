//! Per-chat session tool permission policy engine.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::agent::tool::ToolRisk;
pub use crate::core::types::PermissionDecision;

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

    /// Return `true` if `tool_name` (with `risk`) is already authorized for `conv_id`.
    pub fn is_auto_allowed(&self, conv_id: &str, tool_name: &str, risk: ToolRisk) -> bool {
        match risk {
            ToolRisk::ReadOnly => true,
            ToolRisk::Destructive | ToolRisk::Network => false,
            ToolRisk::WriteWorkspace => self
                .session_allows
                .lock()
                .map(|guard| guard.contains(&(conv_id.to_string(), tool_name.to_string())))
                .unwrap_or(false),
        }
    }

    /// Record a user's permission decision for `(conv_id, tool_name)` according to `risk`.
    pub fn record_decision(
        &self,
        conv_id: &str,
        tool_name: &str,
        risk: ToolRisk,
        decision: PermissionDecision,
    ) {
        if matches!(risk, ToolRisk::Destructive | ToolRisk::Network) {
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

        assert!(engine.is_auto_allowed(conv_id, "list_files", ToolRisk::ReadOnly));
        assert!(engine.is_auto_allowed(conv_id, "read_file", ToolRisk::ReadOnly));
        assert!(engine.is_auto_allowed(conv_id, "search_files", ToolRisk::ReadOnly));
        assert!(!engine.is_auto_allowed(conv_id, "write_file", ToolRisk::WriteWorkspace));
        assert!(!engine.is_auto_allowed(conv_id, "delete_file", ToolRisk::Destructive));
        assert!(!engine.is_auto_allowed(conv_id, "web_search", ToolRisk::Network));

        engine.record_decision(
            conv_id,
            "write_file",
            ToolRisk::WriteWorkspace,
            PermissionDecision::AllowOnce,
        );
        assert!(!engine.is_auto_allowed(conv_id, "write_file", ToolRisk::WriteWorkspace));

        engine.record_decision(
            conv_id,
            "write_file",
            ToolRisk::WriteWorkspace,
            PermissionDecision::AllowChat,
        );
        assert!(engine.is_auto_allowed(conv_id, "write_file", ToolRisk::WriteWorkspace));

        // Destructive and Network tools remain always-ask even if AllowChat is passed.
        engine.record_decision(
            conv_id,
            "delete_file",
            ToolRisk::Destructive,
            PermissionDecision::AllowChat,
        );
        assert!(!engine.is_auto_allowed(conv_id, "delete_file", ToolRisk::Destructive));

        engine.record_decision(
            conv_id,
            "web_search",
            ToolRisk::Network,
            PermissionDecision::AllowChat,
        );
        assert!(!engine.is_auto_allowed(conv_id, "web_search", ToolRisk::Network));
    }
}
