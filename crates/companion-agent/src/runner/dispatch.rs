//! Generic polymorphic tool dispatch, approval handshake, and DB folder-grant persistence.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::approval::ApprovalResponse;
use crate::parser::ParsedToolCall;
use crate::tool::{ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk};
use crate::tools::GrantedFolder;
use companion_core::provider::{ChatRole, ChatTurn};
use companion_core::types::{FolderPermission, PermissionDecision, SearchEngine, ToolCallStatus};
use companion_storage::ToolCallRecord;

use super::{AgentEvent, AgentRunConfig, AgentRunner};

impl AgentRunner {
    pub(super) async fn dispatch_tool(
        &self,
        cfg: &mut AgentRunConfig,
        recorded_tools: &[ToolCallRecord],
        call: ParsedToolCall,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let Some(tool) = self.registry.get(&call.name).cloned() else {
            let out = format!("Unknown tool: {}", call.name);
            return (
                build_tool_record(call, ToolCallStatus::Error, &out, PermissionDecision::Deny),
                out,
            );
        };

        let risk = tool.risk();
        let (web_enabled, engine, searxng_url, local_texts) = if risk == ToolRisk::Network {
            let (global_enabled, engine, url) = self.load_web_settings().await;
            let texts = collect_local_texts(recorded_tools, &cfg.turns);
            (global_enabled && cfg.web_search_enabled, engine, url, texts)
        } else {
            (false, SearchEngine::default(), None, Vec::new())
        };

        let prep_ctx = ToolPrepareContext {
            conv_id: &cfg.conv_id,
            granted_folders: &cfg.granted_folders,
            permissions: &self.permissions,
            web_search_enabled: web_enabled,
            search_engine: engine,
            searxng_url: searxng_url.as_deref(),
            local_texts: &local_texts,
        };
        let auth = tool.prepare(&prep_ctx, &call.arguments).await;

        let (approval, ungranted_target, write_target_folder) = match auth {
            ToolAuthorization::AutoAllowed => (
                ApprovalResponse::Tool(PermissionDecision::AllowOnce),
                None,
                None,
            ),
            ToolAuthorization::Rejected { status, reason } => {
                return (
                    build_tool_record(call, status, &reason, PermissionDecision::Deny),
                    reason,
                );
            }
            ToolAuthorization::RequiresApproval {
                request,
                ungranted_path,
                write_target_folder,
            } => {
                let resp = self.approver.request_approval(request).await;
                if ungranted_path.is_none()
                    && let ApprovalResponse::Tool(d) = resp
                {
                    self.permissions.record_decision(
                        &cfg.conv_id,
                        write_target_folder.as_deref(),
                        tool.name(),
                        risk,
                        d,
                    );
                }
                (resp, ungranted_path, write_target_folder)
            }
        };

        let Some(decision) = approved_decision(&approval) else {
            let msg = tool.denied_message().to_string();
            return (
                build_tool_record(call, ToolCallStatus::Denied, &msg, PermissionDecision::Deny),
                msg,
            );
        };

        let needs_write = matches!(risk, ToolRisk::WriteWorkspace | ToolRisk::Destructive);
        let mut grants = cfg.granted_folders.clone();
        self.apply_grant_overrides(
            &mut grants,
            cfg,
            decision,
            needs_write,
            ungranted_target,
            write_target_folder,
            tx,
        )
        .await;

        let exec_ctx = ToolContext {
            granted_folders: &grants,
            web_search_enabled: web_enabled,
            search_engine: engine,
            searxng_url: searxng_url.as_deref(),
        };
        let _ = tx.send(AgentEvent::Status(tool.status_message(&exec_ctx)));
        let (status, output) = match tool
            .execute_approved(&exec_ctx, &call.arguments, &approval)
            .await
        {
            Ok(out) => (ToolCallStatus::Ok, out),
            Err(err) => (ToolCallStatus::Error, err.to_string()),
        };

        let rec = build_tool_record(call, status, &output, decision);
        (rec, output)
    }

    async fn load_web_settings(&self) -> (bool, SearchEngine, Option<String>) {
        let db_cfg = self.db.clone();
        tokio::task::spawn_blocking(move || {
            (
                db_cfg.get_web_search_global_enabled().unwrap_or(false),
                db_cfg.get_search_engine().unwrap_or_default(),
                db_cfg.get_searxng_url().ok().flatten(),
            )
        })
        .await
        .unwrap_or((false, SearchEngine::default(), None))
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_grant_overrides(
        &self,
        grants: &mut Vec<GrantedFolder>,
        cfg: &mut AgentRunConfig,
        decision: PermissionDecision,
        needs_write: bool,
        ungranted_target: Option<PathBuf>,
        write_target_folder: Option<PathBuf>,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if let Some(ref target) = ungranted_target {
            grants.push(GrantedFolder {
                path: target.clone(),
                allow_write: needs_write,
            });
        } else if needs_write && let Some(ref exact_folder) = write_target_folder {
            for g in grants.iter_mut() {
                if &g.path == exact_folder {
                    g.allow_write = true;
                }
            }
        }

        if decision != PermissionDecision::AllowChat {
            return;
        }
        if let Some(target) = ungranted_target {
            cfg.granted_folders.push(GrantedFolder {
                path: target.clone(),
                allow_write: needs_write,
            });
            self.persist_folder_grant(&cfg.conv_id, target, needs_write, tx)
                .await;
        } else if needs_write && let Some(exact_folder) = write_target_folder {
            for g in &mut cfg.granted_folders {
                if g.path == exact_folder {
                    g.allow_write = true;
                }
            }
            self.persist_folder_grant(&cfg.conv_id, exact_folder, true, tx)
                .await;
        }
    }

    async fn persist_folder_grant(
        &self,
        conv_id: &str,
        folder: PathBuf,
        allow_write: bool,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        let db = self.db.clone();
        let conv = conv_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(rec) = db.add_conversation_folder(&conv, &folder.to_string_lossy())
                && allow_write
            {
                let _ = db.set_conversation_folder_permission(&rec.id, FolderPermission::ReadWrite);
            }
        })
        .await;
        let _ = tx.send(AgentEvent::FolderGrantsUpdated);
    }
}

fn approved_decision(resp: &ApprovalResponse) -> Option<PermissionDecision> {
    match resp {
        ApprovalResponse::Tool(PermissionDecision::Deny) => None,
        ApprovalResponse::Tool(d) => Some(*d),
        ApprovalResponse::WebSearch(Some(_)) | ApprovalResponse::FetchUrl(Some(_)) => {
            Some(PermissionDecision::AllowOnce)
        }
        ApprovalResponse::WebSearch(None) | ApprovalResponse::FetchUrl(None) => None,
    }
}

fn collect_local_texts(recorded_tools: &[ToolCallRecord], turns: &[ChatTurn]) -> Vec<String> {
    let mut local_texts: Vec<String> = recorded_tools
        .iter()
        .filter(|t| t.tool_name == "read_file" && t.status == ToolCallStatus::Ok)
        .filter_map(|t| t.output.clone())
        .collect();
    for t in turns {
        if matches!(t.role, ChatRole::User) && t.content.contains("<attachment") {
            local_texts.push(t.content.clone());
        }
    }
    local_texts
}

fn build_tool_record(
    call: ParsedToolCall,
    status: ToolCallStatus,
    output: &str,
    decision: PermissionDecision,
) -> ToolCallRecord {
    ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.to_string()),
        permission_decision: decision,
        created_at: chrono::Utc::now().timestamp_millis(),
    }
}
