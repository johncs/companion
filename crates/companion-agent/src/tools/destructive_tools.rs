//! Destructive workspace tools: `delete_file` and `delete_directory`.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{delete_granted_directory, delete_granted_file};
use super::path_guard::{
    prepare_workspace_auth, resolve_granted_root, resolve_write_folder_for_raw,
};
use super::run_blocking_tool;
use crate::approval::ApprovalResponse;
use crate::tool::{
    BoxFuture, PreparedToolCall, Tool, ToolAuthorization, ToolContext, ToolMetadata,
    ToolPrepareContext, ToolRisk,
};
use companion_core::i18n::gettext;
use companion_core::types::ToolCallStatus;

#[derive(Deserialize)]
struct DeleteFileArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(alias = "file_path", alias = "filepath", alias = "file")]
    path: String,
}

#[derive(Deserialize)]
struct DeleteDirArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(alias = "directory", alias = "dir", alias = "folder", alias = "subdir")]
    path: String,
}

/// Delete a single file inside a granted `[read & write]` directory (always prompted).
pub struct DeleteFileTool;

impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Delete File"),
            description: gettext("Delete a single file inside a granted directory."),
            prompt_summary: "Delete a single file in a [read & write] directory (requires user approval).",
            parameter_example: r#"{"root": "/granted/dir", "path": "relative/path"}"#,
            read_policy: "Ask",
            rw_policy: "Ask",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<DeleteFileArgs>(args.clone());
        Box::pin(async move {
            let Some(parsed) = parsed.ok().filter(|a| !a.path.trim().is_empty()) else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'path' argument".to_string(),
                };
            };
            let raw_target = parsed
                .root
                .as_deref()
                .unwrap_or(parsed.path.as_str())
                .to_string();
            let write_folder = resolve_write_folder_for_raw(
                ctx.granted_folders,
                &parsed.path,
                parsed.root.as_deref(),
            );
            let preview = format!("Delete file:\n\n{}", parsed.path);
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                Some(&raw_target),
                write_folder,
                preview,
                PreparedToolCall::new(parsed),
            )
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        prepared: PreparedToolCall,
        _approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let grants = ctx.granted_folders.to_vec();
        Box::pin(async move {
            let parsed: DeleteFileArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                delete_granted_file(&root, &rel_path)
            })
            .await
        })
    }
}

/// Recursively delete a subdirectory inside a granted `[read & write]` directory (always prompted).
pub struct DeleteDirectoryTool;

impl Tool for DeleteDirectoryTool {
    fn name(&self) -> &str {
        "delete_directory"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Delete Directory"),
            description: gettext("Delete a subdirectory inside a granted directory."),
            prompt_summary: "Recursively delete a subdirectory in a [read & write] directory (requires user approval).",
            parameter_example: r#"{"root": "/granted/dir", "path": "relative/dir"}"#,
            read_policy: "Ask",
            rw_policy: "Ask",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<DeleteDirArgs>(args.clone());
        Box::pin(async move {
            let Some(parsed) = parsed.ok().filter(|a| !a.path.trim().is_empty()) else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'path' argument".to_string(),
                };
            };
            let raw_target = parsed
                .root
                .as_deref()
                .unwrap_or(parsed.path.as_str())
                .to_string();
            let write_folder = resolve_write_folder_for_raw(
                ctx.granted_folders,
                &parsed.path,
                parsed.root.as_deref(),
            );
            let preview = format!("Delete directory (and its contents):\n\n{}", parsed.path);
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                Some(&raw_target),
                write_folder,
                preview,
                PreparedToolCall::new(parsed),
            )
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        prepared: PreparedToolCall,
        _approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let grants = ctx.granted_folders.to_vec();
        Box::pin(async move {
            let parsed: DeleteDirArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                delete_granted_directory(&root, &rel_path)
            })
            .await
        })
    }
}
