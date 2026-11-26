//! GTK/libadwaita implementation of [`ApprovalHandler`].

use adw::prelude::*;
use gtk::glib;

use crate::agent::approval::{ApprovalHandler, ApprovalResponse};
use crate::agent::permissions::PermissionDecision;
use crate::agent::registry::ToolRegistry;
use crate::agent::tool::{ApprovalRequest, BoxFuture};
use crate::ui::permission_dialog::{
    present_fetch_url_confirm_dialog, present_tool_permission_dialog,
    present_web_search_confirm_dialog,
};

/// Prompts the user on the GTK main thread via `AdwAlertDialog` and resolves over a `oneshot`.
pub(super) struct GtkApprovalHandler {
    parent: glib::SendWeakRef<gtk::ScrolledWindow>,
    registry: ToolRegistry,
}

impl GtkApprovalHandler {
    pub(super) fn new(parent: &gtk::ScrolledWindow, registry: ToolRegistry) -> Self {
        Self {
            parent: parent.downgrade().into(),
            registry,
        }
    }
}

impl ApprovalHandler for GtkApprovalHandler {
    fn request_approval<'a>(&'a self, req: ApprovalRequest) -> BoxFuture<'a, ApprovalResponse> {
        let parent_weak = self.parent.clone();
        let display_name = match &req {
            ApprovalRequest::ToolAction { tool_name, .. } => {
                self.registry.display_name_of(tool_name)
            }
            _ => String::new(),
        };
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let fallback = default_deny(&req);
            glib::MainContext::default().invoke(move || {
                let Some(parent) = parent_weak.upgrade() else {
                    let _ = tx.send(default_deny(&req));
                    return;
                };
                dispatch_approval_dialog(&parent, req, display_name, tx);
            });
            rx.await.unwrap_or(fallback)
        })
    }
}

fn dispatch_approval_dialog(
    parent: &gtk::ScrolledWindow,
    req: ApprovalRequest,
    display_name: String,
    tx: tokio::sync::oneshot::Sender<ApprovalResponse>,
) {
    match req {
        ApprovalRequest::ToolAction {
            preview,
            risk,
            ungranted_path,
            ..
        } => {
            present_tool_permission_dialog(
                parent,
                &display_name,
                &preview,
                risk,
                ungranted_path.is_none(),
                move |d| drop(tx.send(ApprovalResponse::Tool(d))),
            );
        }
        ApprovalRequest::WebSearch {
            engine_label,
            proposed_query,
            warnings,
        } => {
            present_web_search_confirm_dialog(
                parent,
                &engine_label,
                &proposed_query,
                &warnings,
                move |q| drop(tx.send(ApprovalResponse::WebSearch(q))),
            );
        }
        ApprovalRequest::FetchUrl {
            proposed_url,
            warnings,
        } => {
            present_fetch_url_confirm_dialog(parent, &proposed_url, &warnings, move |u| {
                drop(tx.send(ApprovalResponse::FetchUrl(u)))
            });
        }
    }
}

fn default_deny(req: &ApprovalRequest) -> ApprovalResponse {
    match req {
        ApprovalRequest::ToolAction { .. } => ApprovalResponse::Tool(PermissionDecision::Deny),
        ApprovalRequest::WebSearch { .. } => ApprovalResponse::WebSearch(None),
        ApprovalRequest::FetchUrl { .. } => ApprovalResponse::FetchUrl(None),
    }
}
