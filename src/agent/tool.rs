//! Core tool abstractions (`Tool`, `ToolRisk`, `ToolContext`, `ApprovalRequest`).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

use crate::agent::tools::GrantedFolder;
use crate::core::types::SearchEngine;

/// Boxed future returned by async [`Tool::execute`] implementations.
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

/// Runtime context supplied to tools during preview and execution.
pub struct ToolContext<'a> {
    pub granted_folders: &'a [GrantedFolder],
    pub web_search_enabled: bool,
    pub search_engine: SearchEngine,
    pub searxng_url: Option<&'a str>,
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
    fn describe(&self, ctx: &ToolContext<'_>, args: &Value) -> String;

    /// Raw path/root argument used to detect ungranted absolute paths outside granted folders.
    fn raw_target_path<'a>(&self, _args: &'a Value) -> Option<&'a str> {
        None
    }

    /// Return `true` if the tool targets a folder/file already granted with write access.
    fn is_write_already_granted(&self, _granted_folders: &[GrantedFolder], _args: &Value) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, Result<String>>;
}
