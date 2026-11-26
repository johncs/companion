//! Core tool abstractions (`Tool`, `ToolRisk`, `ToolContext`, `ToolAuthorization`).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

use crate::approval::ApprovalResponse;
use crate::permissions::PermissionEngine;
use crate::tools::GrantedFolder;
use companion_core::i18n::tr_f;
use companion_core::types::{SearchEngine, ToolCallStatus};

/// Boxed future returned by async [`Tool`] methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Risk classification controlling permission prompts and session auto-allow rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRisk {
    /// Read-only inspection scoped to granted folders (auto-allowed on granted roots).
    ReadOnly,
    /// Non-destructive workspace writes (may be allowed for the chat session).
    WriteWorkspace,
    /// Irreversible deletions (always prompted one-by-one).
    Destructive,
    /// Outbound network requests (always reviewed with privacy/leak warnings).
    Network,
}

/// Pre-flight context supplied to [`Tool::prepare`] before prompting the user.
pub struct ToolPrepareContext<'a> {
    pub conv_id: &'a str,
    pub granted_folders: &'a [GrantedFolder],
    pub permissions: &'a PermissionEngine,
    pub web_search_enabled: bool,
    pub search_engine: SearchEngine,
    pub searxng_url: Option<&'a str>,
    pub local_texts: &'a [String],
}

/// Runtime context supplied to [`Tool::execute_approved`].
pub struct ToolContext<'a> {
    pub granted_folders: &'a [GrantedFolder],
    pub web_search_enabled: bool,
    pub search_engine: SearchEngine,
    pub searxng_url: Option<&'a str>,
}

/// Pre-flight authorization decision returned by [`Tool::prepare`].
#[derive(Debug, Clone)]
pub enum ToolAuthorization {
    /// Allowed immediately without prompting.
    AutoAllowed,
    /// Rejected before prompting (e.g., disabled feature, invalid URL, missing arg).
    Rejected {
        status: ToolCallStatus,
        reason: String,
    },
    /// Requires user confirmation via `ApprovalHandler`.
    RequiresApproval {
        request: ApprovalRequest,
        ungranted_path: Option<PathBuf>,
        write_target_folder: Option<PathBuf>,
    },
}

/// Structured user approval request emitted before executing sensitive tools.
#[derive(Debug, Clone)]
pub enum ApprovalRequest {
    ToolAction {
        tool_name: String,
        preview: String,
        #[allow(dead_code)]
        risk: ToolRisk,
        #[allow(dead_code)]
        ungranted_path: Option<PathBuf>,
    },
    WebSearch {
        engine_label: String,
        proposed_query: String,
        warnings: Vec<String>,
    },
    FetchUrl {
        proposed_url: String,
        warnings: Vec<String>,
    },
}

/// Unified interface implemented by built-in file tools, web tools, and future MCP tools.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn risk(&self) -> ToolRisk;

    /// Validate arguments and compute authorization/approval requirements before execution.
    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization>;

    /// Human-readable status banner emitted while executing this tool.
    fn status_message(&self, _ctx: &ToolContext<'_>) -> String {
        tr_f("Running {tool}…", &[("tool", self.name())])
    }

    /// Message returned to the model if the user denies approval.
    fn denied_message(&self) -> &'static str {
        "User denied permission."
    }

    /// Execute the tool after authorization (using any user-edited approval payload).
    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>>;
}
