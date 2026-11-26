//! Permission-gated local file and web tool execution for chat agent steps.

use std::path::PathBuf;

use adw::prelude::*;

use crate::agent::parser::ParsedToolCall;
use crate::agent::permissions::{PermissionDecision, PermissionEngine};
use crate::agent::tools::{
    GrantedFolder, describe_tool_action, execute_tool_with_grants, is_write_already_granted,
    requested_ungranted_path,
};
use crate::agent::web_search::{
    execute_fetch_url, execute_web_search, inspect_fetch_url, inspect_search_query,
    validate_fetch_url,
};
use crate::core::runtime;
use crate::core::types::ToolCallStatus;
use crate::i18n::{gettext, tr_f};
use crate::storage::{Database, ToolCallRecord};
use crate::ui::message_object::MessageObject;
use crate::ui::permission_dialog::{
    present_fetch_url_confirm_dialog, present_tool_permission_dialog,
    present_web_search_confirm_dialog,
};

/// Request permission if needed, execute a single folder/file tool off-thread, and build its record.
pub(super) async fn run_chat_tool_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    permissions: &PermissionEngine,
    granted_folders: &[GrantedFolder],
    conv_id: &str,
    call: ParsedToolCall,
) -> (ToolCallRecord, String, Option<PathBuf>) {
    let ungranted_target =
        requested_ungranted_path(granted_folders, &call.name, &call.arguments);
    let decision = if ungranted_target.is_none()
        && (permissions.is_auto_allowed(conv_id, &call.name)
            || is_write_already_granted(granted_folders, &call.name, &call.arguments))
    {
        PermissionDecision::AllowOnce
    } else {
        let preview = describe_tool_action(granted_folders, &call.name, &call.arguments);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_tool_permission_dialog(parent, &call.name, &preview, move |d| {
            let _ = ptx.send(d);
        });
        let d = prx.await.unwrap_or(PermissionDecision::Deny);
        if ungranted_target.is_none() {
            permissions.record_decision(conv_id, &call.name, d);
        }
        d
    };

    let (status, output) = if decision == PermissionDecision::Deny {
        (ToolCallStatus::Denied, "User denied permission.".to_string())
    } else {
        stream_obj.set_status(Some(&tr_f("Running {tool}…", &[("tool", &call.name)])));
        let tname = call.name.clone();
        let targs = call.arguments.clone();
        let mut grants = granted_folders.to_vec();
        let needs_write = matches!(
            call.name.as_str(),
            "write_file"
                | "delete_file"
                | "create_directory"
                | "move_path"
                | "delete_directory"
        );
        if let Some(ref target) = ungranted_target {
            grants.push(GrantedFolder {
                path: target.clone(),
                allow_write: needs_write,
            });
        } else if needs_write {
            for g in &mut grants {
                g.allow_write = true;
            }
        }
        let (ttx, trx) = tokio::sync::oneshot::channel();
        runtime().spawn_blocking(move || {
            let _ = ttx.send(execute_tool_with_grants(&grants, &tname, &targs));
        });
        match trx.await {
            Ok(Ok(out)) => (ToolCallStatus::Ok, out),
            Ok(Err(err)) => (ToolCallStatus::Error, err.to_string()),
            Err(_) => (ToolCallStatus::Error, "Tool task cancelled".to_string()),
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output, ungranted_target)
}

/// Inspect, ask user approval (with editable query), and run `web_search` off-thread.
pub(super) async fn run_web_search_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    db: &Database,
    web_search_enabled: bool,
    local_texts: &[String],
    call: ParsedToolCall,
) -> (ToolCallRecord, String) {
    let proposed_query = call
        .arguments
        .get("query")
        .or_else(|| call.arguments.get("q"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
    let (status, output, decision) = if !global_enabled || !web_search_enabled {
        (
            ToolCallStatus::Denied,
            "Web search is disabled for this chat.".to_string(),
            PermissionDecision::Deny,
        )
    } else if proposed_query.is_empty() {
        (
            ToolCallStatus::Error,
            "Missing 'query' argument".to_string(),
            PermissionDecision::Deny,
        )
    } else {
        let engine = db.get_search_engine().unwrap_or_default();
        let searxng_url = db.get_searxng_url().ok().flatten();
        let warnings = inspect_search_query(&proposed_query, local_texts);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_web_search_confirm_dialog(
            parent,
            &engine.label(),
            &proposed_query,
            &warnings,
            move |approved_query| {
                let _ = ptx.send(approved_query);
            },
        );
        match prx.await.unwrap_or(None) {
            None => (
                ToolCallStatus::Denied,
                "User denied web search.".to_string(),
                PermissionDecision::Deny,
            ),
            Some(final_query) => {
                stream_obj.set_status(Some(&tr_f(
                    "Searching {engine}…",
                    &[("engine", &engine.label())],
                )));
                let (ttx, trx) = tokio::sync::oneshot::channel();
                runtime().spawn(async move {
                    let res =
                        execute_web_search(engine, searxng_url.as_deref(), &final_query).await;
                    let _ = ttx.send(res);
                });
                match trx.await {
                    Ok(Ok(out)) => (ToolCallStatus::Ok, out, PermissionDecision::AllowOnce),
                    Ok(Err(err)) => (
                        ToolCallStatus::Error,
                        err.to_string(),
                        PermissionDecision::AllowOnce,
                    ),
                    Err(_) => (
                        ToolCallStatus::Error,
                        "Web search task cancelled".to_string(),
                        PermissionDecision::AllowOnce,
                    ),
                }
            }
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output)
}

/// Validate, ask user approval (with editable HTTPS URL), and fetch a web page off-thread.
pub(super) async fn run_fetch_url_call(
    parent: &impl IsA<gtk::Widget>,
    stream_obj: &MessageObject,
    db: &Database,
    web_search_enabled: bool,
    local_texts: &[String],
    call: ParsedToolCall,
) -> (ToolCallRecord, String) {
    let proposed_url = call
        .arguments
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let global_enabled = db.get_web_search_global_enabled().unwrap_or(false);
    let (status, output, decision) = if !global_enabled || !web_search_enabled {
        (
            ToolCallStatus::Denied,
            "Web search is disabled for this chat.".to_string(),
            PermissionDecision::Deny,
        )
    } else if proposed_url.is_empty() {
        (
            ToolCallStatus::Error,
            "Missing 'url' argument".to_string(),
            PermissionDecision::Deny,
        )
    } else if let Err(err) = validate_fetch_url(&proposed_url) {
        (
            ToolCallStatus::Error,
            err.to_string(),
            PermissionDecision::Deny,
        )
    } else {
        let warnings = inspect_fetch_url(&proposed_url, local_texts);
        let (ptx, prx) = tokio::sync::oneshot::channel();
        present_fetch_url_confirm_dialog(parent, &proposed_url, &warnings, move |approved_url| {
            let _ = ptx.send(approved_url);
        });
        match prx.await.unwrap_or(None) {
            None => (
                ToolCallStatus::Denied,
                "User denied web page fetch.".to_string(),
                PermissionDecision::Deny,
            ),
            Some(final_url) => {
                if let Err(err) = validate_fetch_url(&final_url) {
                    (
                        ToolCallStatus::Error,
                        err.to_string(),
                        PermissionDecision::Deny,
                    )
                } else {
                    stream_obj.set_status(Some(&gettext("Fetching web page…")));
                    let (ttx, trx) = tokio::sync::oneshot::channel();
                    runtime().spawn(async move {
                        let res = execute_fetch_url(&final_url).await;
                        let _ = ttx.send(res);
                    });
                    match trx.await {
                        Ok(Ok(out)) => (ToolCallStatus::Ok, out, PermissionDecision::AllowOnce),
                        Ok(Err(err)) => (
                            ToolCallStatus::Error,
                            err.to_string(),
                            PermissionDecision::AllowOnce,
                        ),
                        Err(_) => (
                            ToolCallStatus::Error,
                            "Fetch task cancelled".to_string(),
                            PermissionDecision::AllowOnce,
                        ),
                    }
                }
            }
        }
    };

    let record = ToolCallRecord {
        id: uuid::Uuid::new_v4().to_string(),
        message_id: String::new(),
        tool_name: call.name,
        arguments_json: serde_json::to_string_pretty(&call.arguments)
            .unwrap_or_else(|_| call.arguments.to_string()),
        status,
        output: Some(output.clone()),
        permission_decision: decision,
        created_at: chrono::Utc::now().timestamp_millis(),
    };

    (record, output)
}
