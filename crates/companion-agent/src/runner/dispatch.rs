//! Generic polymorphic tool dispatch, approval handshake, and DB folder-grant persistence.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::approval::ApprovalResponse;
use crate::parser::ParsedToolCall;
use crate::registry::ToolRegistry;
use crate::tool::{
    PreparedToolCall, ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk, WebToolConfig,
};
use crate::tools::GrantedFolder;
use companion_core::provider::{ChatRole, ChatTurn};
use companion_core::types::{FolderPermission, PermissionDecision, SearchEngine, ToolCallStatus};
use companion_storage::ToolCallRecord;

use super::{AgentEvent, AgentRunConfig, AgentRunner, build_tool_record};

struct PreparedExecInput {
    risk: ToolRisk,
    decision: PermissionDecision,
    approval: ApprovalResponse,
    ungranted_target: Option<PathBuf>,
    write_target_folder: Option<PathBuf>,
    prepared: PreparedToolCall,
    web_enabled: bool,
    engine: SearchEngine,
    searxng_url: Option<String>,
}

type AuthSuccess = (
    ApprovalResponse,
    Option<PathBuf>,
    Option<PathBuf>,
    PreparedToolCall,
);
type AuthOutcome = Result<AuthSuccess, (ToolCallStatus, String)>;

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
        let (web_enabled, engine, searxng_url, local_texts) = self
            .resolve_network_context(cfg, risk, recorded_tools)
            .await;
        let web = WebToolConfig {
            enabled: web_enabled,
            engine,
            searxng_url: searxng_url.as_deref(),
        };
        let auth_res = self
            .prepare_and_authorize(cfg, tool.as_ref(), &call.arguments, web, &local_texts)
            .await;
        self.finish_tool_dispatch(
            cfg,
            tool.as_ref(),
            call,
            auth_res,
            (web_enabled, engine, searxng_url),
            tx,
        )
        .await
    }

    async fn finish_tool_dispatch(
        &self,
        cfg: &mut AgentRunConfig,
        tool: &dyn crate::tool::Tool,
        call: ParsedToolCall,
        auth_res: AuthOutcome,
        web_cfg: (bool, SearchEngine, Option<String>),
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let (approval, ungranted_target, write_target_folder, prepared) = match auth_res {
            Ok(ok) => ok,
            Err((status, reason)) => {
                return (
                    build_tool_record(call, status, &reason, PermissionDecision::Deny),
                    reason,
                );
            }
        };
        let Some(decision) = approved_decision(&approval) else {
            let msg = tool.denied_message().to_string();
            return (
                build_tool_record(call, ToolCallStatus::Denied, &msg, PermissionDecision::Deny),
                msg,
            );
        };
        let input = PreparedExecInput {
            risk: tool.risk(),
            decision,
            approval,
            ungranted_target,
            write_target_folder,
            prepared,
            web_enabled: web_cfg.0,
            engine: web_cfg.1,
            searxng_url: web_cfg.2,
        };
        self.execute_prepared_tool(cfg, tool, call, input, tx).await
    }

    async fn prepare_and_authorize(
        &self,
        cfg: &AgentRunConfig,
        tool: &dyn crate::tool::Tool,
        arguments: &serde_json::Value,
        web: WebToolConfig<'_>,
        local_texts: &[String],
    ) -> Result<
        (
            ApprovalResponse,
            Option<PathBuf>,
            Option<PathBuf>,
            crate::tool::PreparedToolCall,
        ),
        (ToolCallStatus, String),
    > {
        let prep_ctx = ToolPrepareContext {
            conv_id: &cfg.conv_id,
            granted_folders: &cfg.granted_folders,
            permissions: &self.permissions,
            web,
            local_texts,
        };
        let auth = tool.prepare(&prep_ctx, arguments).await;
        self.authorize_tool(cfg, tool, tool.risk(), auth).await
    }

    async fn execute_prepared_tool(
        &self,
        cfg: &mut AgentRunConfig,
        tool: &dyn crate::tool::Tool,
        call: ParsedToolCall,
        input: PreparedExecInput,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> (ToolCallRecord, String) {
        let mut grants = cfg.granted_folders.clone();
        let override_in = GrantOverrideInput {
            risk: input.risk,
            decision: input.decision,
            needs_write: matches!(input.risk, ToolRisk::WriteWorkspace | ToolRisk::Destructive),
            ungranted_target: input.ungranted_target,
            write_target_folder: input.write_target_folder,
        };
        self.apply_grant_overrides(&mut grants, cfg, override_in, tx)
            .await;
        let exec_ctx = ToolContext {
            granted_folders: &grants,
            web: WebToolConfig {
                enabled: input.web_enabled,
                engine: input.engine,
                searxng_url: input.searxng_url.as_deref(),
            },
        };
        let _ = tx.send(AgentEvent::Status(tool.status_message(&exec_ctx)));
        if cfg.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            let msg = "Cancelled".to_string();
            return (
                build_tool_record(call, ToolCallStatus::Denied, &msg, PermissionDecision::Deny),
                msg,
            );
        }
        let (status, output) = match tool
            .execute_approved(&exec_ctx, input.prepared, &input.approval)
            .await
        {
            Ok(out) => (ToolCallStatus::Ok, out),
            Err(err) => (ToolCallStatus::Error, err.to_string()),
        };
        (
            build_tool_record(call, status, &output, input.decision),
            output,
        )
    }

    async fn resolve_network_context(
        &self,
        cfg: &AgentRunConfig,
        risk: ToolRisk,
        recorded_tools: &[ToolCallRecord],
    ) -> (bool, SearchEngine, Option<String>, Vec<String>) {
        if risk != ToolRisk::Network {
            return (false, SearchEngine::default(), None, Vec::new());
        }
        let (global_enabled, engine, url) = self.load_web_settings().await;
        let texts = collect_local_texts(&self.registry, recorded_tools, &cfg.turns);
        (global_enabled && cfg.web_search_enabled, engine, url, texts)
    }

    async fn authorize_tool(
        &self,
        cfg: &AgentRunConfig,
        tool: &dyn crate::tool::Tool,
        risk: ToolRisk,
        auth: ToolAuthorization,
    ) -> Result<
        (
            ApprovalResponse,
            Option<PathBuf>,
            Option<PathBuf>,
            crate::tool::PreparedToolCall,
        ),
        (ToolCallStatus, String),
    > {
        let ToolAuthorization::RequiresApproval {
            request,
            ungranted_path,
            write_target_folder,
            prepared,
        } = auth
        else {
            return match auth {
                ToolAuthorization::AutoAllowed(p) => Ok((
                    ApprovalResponse::Tool(PermissionDecision::AllowOnce),
                    None,
                    None,
                    p,
                )),
                ToolAuthorization::Rejected { status, reason } => Err((status, reason)),
                ToolAuthorization::RequiresApproval { .. } => unreachable!(),
            };
        };
        if cfg.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return Err((ToolCallStatus::Denied, "Cancelled".to_string()));
        }
        let resp = self.approver.request_approval(request).await;
        if cfg.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return Err((ToolCallStatus::Denied, "Cancelled".to_string()));
        }
        if ungranted_path.is_none()
            && let ApprovalResponse::Tool(d) = resp
        {
            let folder = write_target_folder.as_deref();
            self.permissions
                .record_decision(&cfg.conv_id, folder, tool.name(), risk, d);
        }
        Ok((resp, ungranted_path, write_target_folder, prepared))
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

    async fn apply_grant_overrides(
        &self,
        grants: &mut Vec<GrantedFolder>,
        cfg: &mut AgentRunConfig,
        input: GrantOverrideInput,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) {
        if let Some(ref target) = input.ungranted_target {
            grants.push(GrantedFolder {
                path: target.clone(),
                allow_write: input.needs_write,
            });
        } else if input.needs_write
            && let Some(ref exact_folder) = input.write_target_folder
        {
            for g in grants.iter_mut() {
                if &g.path == exact_folder {
                    g.allow_write = true;
                }
            }
        }

        // Destructive tools and ungranted paths never persist folder grants across the chat.
        if input.decision != PermissionDecision::AllowChat || input.risk == ToolRisk::Destructive {
            return;
        }
        if input.needs_write
            && let Some(exact_folder) = input.write_target_folder
        {
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
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let rec = db.add_conversation_folder(&conv, &folder.to_string_lossy())?;
            if allow_write {
                db.set_conversation_folder_permission(&rec.id, FolderPermission::ReadWrite)?;
            }
            Ok(())
        })
        .await;
        match res {
            Ok(Ok(())) => {
                let _ = tx.send(AgentEvent::FolderGrantsUpdated);
            }
            Ok(Err(err)) => {
                tracing::error!("Failed to persist granted folder: {err}");
                let _ = tx.send(AgentEvent::Error(companion_core::i18n::tr_f(
                    "Failed to save folder access: {err}",
                    &[("err", &err.to_string())],
                )));
            }
            Err(err) => {
                tracing::error!("Folder grant task failed: {err}");
            }
        }
    }
}

struct GrantOverrideInput {
    risk: ToolRisk,
    decision: PermissionDecision,
    needs_write: bool,
    ungranted_target: Option<PathBuf>,
    write_target_folder: Option<PathBuf>,
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

fn collect_local_texts(
    registry: &ToolRegistry,
    recorded_tools: &[ToolCallRecord],
    turns: &[ChatTurn],
) -> Vec<String> {
    let mut local_texts: Vec<String> = recorded_tools
        .iter()
        .filter(|t| {
            t.status == ToolCallStatus::Ok
                && registry
                    .get(&t.tool_name)
                    .is_some_and(|tool| tool.produces_sensitive_local_context())
        })
        .filter_map(|t| t.output.clone())
        .collect();
    local_texts.extend(
        turns
            .iter()
            .filter(|t| matches!(t.role, ChatRole::User) && t.content.contains("<attachment"))
            .map(|t| t.content.clone()),
    );
    local_texts
}
