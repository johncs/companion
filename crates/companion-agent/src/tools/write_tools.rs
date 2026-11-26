//! Non-destructive workspace write tools: `write_file`, `create_directory`, and `move_path`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{create_granted_directory, move_granted_path, write_granted_file};
use super::path_guard::{
    GrantedFolder, prepare_workspace_auth, resolve_granted_path, resolve_granted_root,
    resolve_write_folder_for_raw,
};
use crate::approval::ApprovalResponse;
use crate::tool::{BoxFuture, Tool, ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk};

#[derive(Deserialize)]
struct WriteFileArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(alias = "file_path", alias = "filepath", alias = "file")]
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct CreateDirArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(alias = "directory", alias = "dir", alias = "folder", alias = "subdir")]
    path: String,
}

#[derive(Deserialize)]
struct MovePathArgs {
    #[serde(default)]
    root: Option<String>,
    from: String,
    to: String,
}

/// Create or overwrite a UTF-8 file inside a granted `[read & write]` directory.
pub struct WriteFileTool;

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<WriteFileArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(Some(a.path.as_str())));
            let write_folder = parsed.as_ref().and_then(|a| {
                resolve_write_folder_for_raw(ctx.granted_folders, &a.path, a.root.as_deref())
            });
            let preview = parsed
                .as_ref()
                .map(|a| describe_write(ctx.granted_folders, a))
                .unwrap_or_else(|| "Write file".to_string());
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
                let parsed: WriteFileArgs =
                    serde_json::from_value(args).context("Missing 'path' or 'content' argument")?;
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                write_granted_file(&root, &rel_path, &parsed.content)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

fn describe_write(granted_folders: &[GrantedFolder], args: &WriteFileArgs) -> String {
    let exists = resolve_granted_root(granted_folders, &args.path, args.root.as_deref(), true)
        .and_then(|(r, rel)| resolve_granted_path(&r, &rel))
        .map(|p| p.exists())
        .unwrap_or(false);
    let action = if exists { "Overwrite" } else { "Create" };
    let preview: String = args.content.lines().take(12).collect::<Vec<_>>().join("\n");
    format!("{action} file: {}\n\n{preview}", args.path)
}

/// Create a directory (and parents) inside a granted `[read & write]` directory.
pub struct CreateDirectoryTool;

impl Tool for CreateDirectoryTool {
    fn name(&self) -> &'static str {
        "create_directory"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<CreateDirArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(Some(a.path.as_str())));
            let write_folder = parsed.as_ref().and_then(|a| {
                resolve_write_folder_for_raw(ctx.granted_folders, &a.path, a.root.as_deref())
            });
            let path_display = parsed.as_ref().map(|a| a.path.as_str()).unwrap_or("?");
            let preview = format!("Create directory:\n\n{path_display}");
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
                let parsed: CreateDirArgs =
                    serde_json::from_value(args).context("Missing 'path' argument")?;
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                create_granted_directory(&root, &rel_path)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

/// Move or rename a file/directory inside a granted `[read & write]` directory.
pub struct MovePathTool;

impl Tool for MovePathTool {
    fn name(&self) -> &'static str {
        "move_path"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<MovePathArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed.as_ref().and_then(|a| a.root.as_deref());
            let write_folder = parsed.as_ref().and_then(|a| {
                resolve_write_folder_for_raw(ctx.granted_folders, &a.from, a.root.as_deref())
            });
            let preview = parsed
                .as_ref()
                .map(|a| format!("Move / rename:\n\n{}\n→ {}", a.from, a.to))
                .unwrap_or_else(|| "Move / rename".to_string());
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
                let parsed: MovePathArgs =
                    serde_json::from_value(args).context("Missing 'from' or 'to' argument")?;
                let (root, rel_from) =
                    resolve_granted_root(&grants, &parsed.from, parsed.root.as_deref(), true)?;
                let (_, rel_to) =
                    resolve_granted_root(&grants, &parsed.to, Some(&root.to_string_lossy()), true)?;
                move_granted_path(&root, &rel_from, &rel_to)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}
