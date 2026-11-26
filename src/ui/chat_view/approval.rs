//! GTK/libadwaita implementation of [`ApprovalHandler`].

use adw::prelude::*;
use gtk::glib;

use crate::agent::approval::{ApprovalHandler, ApprovalResponse};
use crate::agent::permissions::PermissionDecision;
use crate::agent::tool::{ApprovalRequest, BoxFuture};
use crate::ui::permission_dialog::{
    present_fetch_url_confirm_dialog, present_tool_permission_dialog,
    present_web_search_confirm_dialog,
};

/// Prompts the user on the GTK main thread via `AdwAlertDialog` and resolves over a `oneshot`.
pub(super) struct GtkApprovalHandler {
    parent: glib::SendWeakRef<gtk::ScrolledWindow>,
}

impl GtkApprovalHandler {
    pub(super) fn new(parent: &gtk::ScrolledWindow) -> Self {
        Self {
            parent: parent.downgrade().into(),
        }
    }
}

impl ApprovalHandler for GtkApprovalHandler {
    fn request_approval<'a>(&'a self, req: ApprovalRequest) -> BoxFuture<'a, ApprovalResponse> {
        let parent_weak = self.parent.clone();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            glib::MainContext::default().invoke(move || {
                let Some(parent) = parent_weak.upgrade() else {
                    let _ = tx.send(default_deny(&req));
                    return;
                };
                match req {
                    ApprovalRequest::ToolAction {
                        tool_name, preview, ..
                    } => {
                        present_tool_permission_dialog(&parent, &tool_name, &preview, move |d| {
                            let _ = tx.send(ApprovalResponse::Tool(d));
                        });
                    }
                    ApprovalRequest::WebSearch {
                        engine_label,
                        proposed_query,
                        warnings,
                    } => {
                        present_web_search_confirm_dialog(
                            &parent,
                            &engine_label,
                            &proposed_query,
                            &warnings,
                            move |q| {
                                let _ = tx.send(ApprovalResponse::WebSearch(q));
                            },
                        );
                    }
                    ApprovalRequest::FetchUrl {
                        proposed_url,
                        warnings,
                    } => {
                        present_fetch_url_confirm_dialog(
                            &parent,
                            &proposed_url,
                            &warnings,
                            move |u| {
                                let _ = tx.send(ApprovalResponse::FetchUrl(u));
                            },
                        );
                    }
                }
            });
            rx.await.unwrap_or_else(|_| ApprovalResponse::Tool(PermissionDecision::Deny))
        })
    }
}

fn default_deny(req: &ApprovalRequest) -> ApprovalResponse {
    match req {
        ApprovalRequest::ToolAction { .. } => ApprovalResponse::Tool(PermissionDecision::Deny),
        ApprovalRequest::WebSearch { .. } => ApprovalResponse::WebSearch(None),
        ApprovalRequest::FetchUrl { .. } => ApprovalResponse::FetchUrl(None),
    }
}
