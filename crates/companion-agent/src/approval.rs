//! UI-agnostic approval handler trait for tool, web search, and URL fetch prompts.

use crate::tool::{ApprovalRequest, BoxFuture};
use companion_core::types::PermissionDecision;

/// User response to an [`ApprovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResponse {
    Tool(PermissionDecision),
    WebSearch(Option<String>),
    FetchUrl(Option<String>),
}

/// Async bridge between the pure agent runner and whichever UI (or test mock) prompts the user.
pub trait ApprovalHandler: Send + Sync {
    fn request_approval<'a>(&'a self, req: ApprovalRequest) -> BoxFuture<'a, ApprovalResponse>;
}
