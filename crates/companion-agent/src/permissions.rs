//! Per-chat session tool permission policy engine.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::tool::ToolRisk;
pub use companion_core::types::PermissionDecision;

/// Evaluates and remembers whether an agent tool may run without prompting.
/// Write permissions are always scoped to `(conv_id, canonical_folder_path, tool_name)`.
#[derive(Clone, Default)]
pub struct PermissionEngine {
    session_allows: Arc<Mutex<HashSet<(String, PathBuf, String)>>>,
}

impl PermissionEngine {
    pub fn new() -> Self {
        Self {
            session_allows: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Return `true` if `tool_name` (with `risk`) is already authorized for `conv_id` and `folder_path`.
    pub fn is_auto_allowed(
        &self,
        conv_id: &str,
        folder_path: Option<&Path>,
        tool_name: &str,
        risk: ToolRisk,
    ) -> bool {
        match risk {
            ToolRisk::ReadOnly => true,
            ToolRisk::Destructive | ToolRisk::Network => false,
            ToolRisk::WriteWorkspace => {
                let Some(folder) = folder_path else {
                    return false;
                };
                let canon = canonicalize_scope(folder);
                self.session_allows
                    .lock()
                    .map(|guard| {
                        guard.contains(&(conv_id.to_string(), canon, tool_name.to_string()))
                    })
                    .unwrap_or(false)
            }
        }
    }

    /// Record a user's permission decision for `(conv_id, folder_path, tool_name)` according to `risk`.
    pub fn record_decision(
        &self,
        conv_id: &str,
        folder_path: Option<&Path>,
        tool_name: &str,
        risk: ToolRisk,
        decision: PermissionDecision,
    ) {
        if matches!(risk, ToolRisk::Destructive | ToolRisk::Network) {
            return;
        }
        if decision == PermissionDecision::AllowChat
            && let Some(folder) = folder_path
            && let Ok(mut guard) = self.session_allows.lock()
        {
            guard.insert((
                conv_id.to_string(),
                canonicalize_scope(folder),
                tool_name.to_string(),
            ));
        }
    }
}

fn canonicalize_scope(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_auto_allow_and_session_policy_is_folder_scoped() {
        let conv_id = "conv-1";
        let folder_a = Path::new("/tmp/companion_perm_a");
        let folder_b = Path::new("/tmp/companion_perm_b");
        let engine = PermissionEngine::new();

        assert!(engine.is_auto_allowed(conv_id, None, "list_files", ToolRisk::ReadOnly));
        assert!(engine.is_auto_allowed(conv_id, None, "read_file", ToolRisk::ReadOnly));
        assert!(engine.is_auto_allowed(conv_id, None, "search_files", ToolRisk::ReadOnly));
        assert!(!engine.is_auto_allowed(
            conv_id,
            Some(folder_a),
            "write_file",
            ToolRisk::WriteWorkspace
        ));
        assert!(!engine.is_auto_allowed(conv_id, None, "write_file", ToolRisk::WriteWorkspace));
        assert!(!engine.is_auto_allowed(
            conv_id,
            Some(folder_a),
            "delete_file",
            ToolRisk::Destructive
        ));
        assert!(!engine.is_auto_allowed(conv_id, None, "web_search", ToolRisk::Network));

        engine.record_decision(
            conv_id,
            Some(folder_a),
            "write_file",
            ToolRisk::WriteWorkspace,
            PermissionDecision::AllowOnce,
        );
        assert!(!engine.is_auto_allowed(
            conv_id,
            Some(folder_a),
            "write_file",
            ToolRisk::WriteWorkspace
        ));

        // AllowChat for folder_a must NOT escalate folder_b.
        engine.record_decision(
            conv_id,
            Some(folder_a),
            "write_file",
            ToolRisk::WriteWorkspace,
            PermissionDecision::AllowChat,
        );
        assert!(engine.is_auto_allowed(
            conv_id,
            Some(folder_a),
            "write_file",
            ToolRisk::WriteWorkspace
        ));
        assert!(!engine.is_auto_allowed(
            conv_id,
            Some(folder_b),
            "write_file",
            ToolRisk::WriteWorkspace
        ));
        assert!(!engine.is_auto_allowed(conv_id, None, "write_file", ToolRisk::WriteWorkspace));

        // Destructive and Network tools remain always-ask even if AllowChat is passed.
        engine.record_decision(
            conv_id,
            Some(folder_a),
            "delete_file",
            ToolRisk::Destructive,
            PermissionDecision::AllowChat,
        );
        assert!(!engine.is_auto_allowed(
            conv_id,
            Some(folder_a),
            "delete_file",
            ToolRisk::Destructive
        ));

        engine.record_decision(
            conv_id,
            None,
            "web_search",
            ToolRisk::Network,
            PermissionDecision::AllowChat,
        );
        assert!(!engine.is_auto_allowed(conv_id, None, "web_search", ToolRisk::Network));
    }
}
