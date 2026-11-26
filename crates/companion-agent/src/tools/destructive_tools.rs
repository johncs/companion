//! Destructive workspace tools: `delete_file` and `delete_directory`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{delete_granted_directory, delete_granted_file};
use super::path_guard::{
    prepare_workspace_auth, resolve_granted_root, resolve_write_folder_for_raw,
};
use crate::approval::ApprovalResponse;
use crate::tool::{BoxFuture, Tool, ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk};

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
    fn name(&self) -> &'static str {
        "delete_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<DeleteFileArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(Some(a.path.as_str())));
            let write_folder = parsed.as_ref().and_then(|a| {
                resolve_write_folder_for_raw(ctx.granted_folders, &a.path, a.root.as_deref())
            });
            let path_display = parsed.as_ref().map(|a| a.path.as_str()).unwrap_or("?");
            let preview = format!("Delete file:\n\n{path_display}");
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                raw_target,
                write_folder,
                preview,
            )
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
        _approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let grants = ctx.granted_folders.to_vec();
        let args = args.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let parsed: DeleteFileArgs =
                    serde_json::from_value(args).context("Missing 'path' argument")?;
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                delete_granted_file(&root, &rel_path)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

/// Recursively delete a subdirectory inside a granted `[read & write]` directory (always prompted).
pub struct DeleteDirectoryTool;

impl Tool for DeleteDirectoryTool {
    fn name(&self) -> &'static str {
        "delete_directory"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<DeleteDirArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(Some(a.path.as_str())));
            let write_folder = parsed.as_ref().and_then(|a| {
                resolve_write_folder_for_raw(ctx.granted_folders, &a.path, a.root.as_deref())
            });
            let path_display = parsed.as_ref().map(|a| a.path.as_str()).unwrap_or("?");
            let preview = format!("Delete directory (and its contents):\n\n{path_display}");
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                raw_target,
                write_folder,
                preview,
            )
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
        _approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let grants = ctx.granted_folders.to_vec();
        let args = args.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let parsed: DeleteDirArgs =
                    serde_json::from_value(args).context("Missing 'path' argument")?;
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                delete_granted_directory(&root, &rel_path)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}
