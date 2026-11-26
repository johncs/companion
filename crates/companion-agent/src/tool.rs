//! Core tool abstractions (`Tool`, `ToolRisk`, `ToolMetadata`, `PreparedToolCall`).

use std::any::Any;
use std::borrow::Cow;
use std::fmt;
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

/// Type-erased, single-pass validated argument payload passed from [`Tool::prepare`] to [`Tool::execute_approved`].
pub struct PreparedToolCall(Box<dyn Any + Send + Sync>);

impl PreparedToolCall {
    /// Wrap a validated tool-specific argument struct.
    pub fn new<T: Send + Sync + 'static>(val: T) -> Self {
        Self(Box::new(val))
    }

    /// Recover the concrete argument struct produced by [`Tool::prepare`].
    pub fn downcast<T: 'static>(self) -> Result<T> {
        self.0
            .downcast::<T>()
            .map(|b| *b)
            .map_err(|_| anyhow::anyhow!("Internal tool state mismatch"))
    }
}

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

/// Declarative metadata exposed by each [`Tool`] for prompt generation and UI capability tables.
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub display_name: String,
    pub description: String,
    pub prompt_summary: &'static str,
    pub parameter_example: &'static str,
    pub read_policy: &'static str,
    pub rw_policy: &'static str,
}

/// Web search / fetch runtime configuration passed to tools.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebToolConfig<'a> {
    pub enabled: bool,
    pub engine: SearchEngine,
    pub searxng_url: Option<&'a str>,
}

/// Pre-flight context supplied to [`Tool::prepare`] before prompting the user.
pub struct ToolPrepareContext<'a> {
    pub conv_id: &'a str,
    pub granted_folders: &'a [GrantedFolder],
    pub permissions: &'a PermissionEngine,
    pub web: WebToolConfig<'a>,
    pub local_texts: &'a [String],
}

/// Runtime context supplied to [`Tool::execute_approved`].
pub struct ToolContext<'a> {
    pub granted_folders: &'a [GrantedFolder],
    pub web: WebToolConfig<'a>,
}

/// Pre-flight authorization decision returned by [`Tool::prepare`].
pub enum ToolAuthorization {
    /// Allowed immediately without prompting.
    AutoAllowed(PreparedToolCall),
    /// Rejected before prompting (e.g., disabled feature, invalid URL, malformed args).
    Rejected {
        status: ToolCallStatus,
        reason: String,
    },
    /// Requires user confirmation via `ApprovalHandler`.
    RequiresApproval {
        request: ApprovalRequest,
        ungranted_path: Option<PathBuf>,
        write_target_folder: Option<PathBuf>,
        prepared: PreparedToolCall,
    },
}

impl fmt::Debug for ToolAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AutoAllowed(_) => f.debug_tuple("AutoAllowed").finish(),
            Self::Rejected { status, reason } => f
                .debug_struct("Rejected")
                .field("status", status)
                .field("reason", reason)
                .finish(),
            Self::RequiresApproval {
                request,
                ungranted_path,
                write_target_folder,
                ..
            } => f
                .debug_struct("RequiresApproval")
                .field("request", request)
                .field("ungranted_path", ungranted_path)
                .field("write_target_folder", write_target_folder)
                .finish(),
        }
    }
}

/// Structured user approval request emitted before executing sensitive tools.
#[derive(Debug, Clone)]
pub enum ApprovalRequest {
    ToolAction {
        tool_name: String,
        preview: String,
        risk: ToolRisk,
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
    fn name(&self) -> &str;
    fn risk(&self) -> ToolRisk;
    fn metadata(&self) -> ToolMetadata;

    /// Whether outputs from this tool should be scanned by `privacy_guard` before web requests.
    fn produces_sensitive_local_context(&self) -> bool {
        false
    }

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
    fn denied_message(&self) -> Cow<'static, str> {
        Cow::Borrowed("User denied permission.")
    }

    /// Execute the tool after authorization (using the prepared payload and any user edits).
    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        prepared: PreparedToolCall,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>>;
}
