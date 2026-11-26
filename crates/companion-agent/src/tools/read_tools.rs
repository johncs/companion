//! Read-only workspace tools: `list_files`, `read_file`, and `search_files`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{
    MAX_LIST_ENTRIES, list_granted_files, read_granted_file, search_granted_files,
};
use super::path_guard::{
    GrantedFolder, collect_allowed_roots, prepare_workspace_auth, resolve_granted_root,
};
use crate::approval::ApprovalResponse;
use crate::tool::{BoxFuture, Tool, ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk};

#[derive(Deserialize)]
struct ListFilesArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(
        default,
        alias = "path",
        alias = "directory",
        alias = "dir",
        alias = "folder"
    )]
    subdir: Option<String>,
}

#[derive(Deserialize)]
struct ReadFileArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(alias = "file_path", alias = "filepath", alias = "file")]
    path: String,
}

#[derive(Deserialize)]
struct SearchFilesArgs {
    #[serde(default)]
    root: Option<String>,
    query: String,
}

/// List files in a granted directory (respecting `.gitignore`).
pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn name(&self) -> &'static str {
        "list_files"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<ListFilesArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(a.subdir.as_deref()));
            let target = raw_target.unwrap_or(".");
            let preview = format!("List files in:\n\n{target}");
            prepare_workspace_auth(ctx, self.name(), self.risk(), raw_target, None, preview)
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
            tokio::task::spawn_blocking(move || exec_list_files(&grants, &args))
                .await
                .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

fn exec_list_files(granted_folders: &[GrantedFolder], args: &Value) -> Result<String> {
    let parsed: ListFilesArgs = serde_json::from_value(args.clone()).unwrap_or(ListFilesArgs {
        root: None,
        subdir: None,
    });
    let subdir = parsed.subdir.as_deref().unwrap_or(".");
    let root_override = parsed.root.as_deref();

    if root_override.is_some() || subdir != "." {
        let (root, rel_sub) = resolve_granted_root(granted_folders, subdir, root_override, false)?;
        let files = list_granted_files(&root, &rel_sub, MAX_LIST_ENTRIES)?;
        if files.is_empty() {
            return Ok("(empty directory)".to_string());
        }
        return Ok(files.join("\n"));
    }

    let roots = collect_allowed_roots(granted_folders, false);
    if roots.is_empty() {
        anyhow::bail!("No folders are granted for this chat");
    }
    if roots.len() == 1 {
        let files = list_granted_files(&roots[0], ".", MAX_LIST_ENTRIES)?;
        if files.is_empty() {
            return Ok("(empty directory)".to_string());
        }
        return Ok(files.join("\n"));
    }

    let mut sections = Vec::new();
    for root in roots {
        let files = list_granted_files(&root, ".", MAX_LIST_ENTRIES)?;
        let header = format!("[{}]", root.display());
        if files.is_empty() {
            sections.push(format!("{header}\n(empty directory)"));
        } else {
            sections.push(format!("{header}\n{}", files.join("\n")));
        }
    }
    Ok(sections.join("\n\n"))
}

/// Read a UTF-8 text file inside a granted folder or granted single file.
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<ReadFileArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed
                .as_ref()
                .and_then(|a| a.root.as_deref().or(Some(a.path.as_str())));
            let path_display = parsed.as_ref().map(|a| a.path.as_str()).unwrap_or("?");
            let preview = format!("Read file:\n\n{path_display}");
            prepare_workspace_auth(ctx, self.name(), self.risk(), raw_target, None, preview)
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
                let parsed: ReadFileArgs =
                    serde_json::from_value(args).context("Missing 'path' argument")?;
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), false)?;
                read_granted_file(&root, &rel_path)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

/// Search for text across granted files/directories.
pub struct SearchFilesTool;

impl Tool for SearchFilesTool {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<SearchFilesArgs>(args.clone()).ok();
        Box::pin(async move {
            let raw_target = parsed.as_ref().and_then(|a| a.root.as_deref());
            let query = parsed.as_ref().map(|a| a.query.as_str()).unwrap_or("");
            let root = raw_target.unwrap_or("granted folders");
            let preview = format!("Search for \"{query}\" in:\n\n{root}");
            prepare_workspace_auth(ctx, self.name(), self.risk(), raw_target, None, preview)
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
            tokio::task::spawn_blocking(move || exec_search_files(&grants, &args))
                .await
                .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

fn exec_search_files(granted_folders: &[GrantedFolder], args: &Value) -> Result<String> {
    let parsed: SearchFilesArgs =
        serde_json::from_value(args.clone()).context("Missing 'query' argument")?;
    let root_override = parsed.root.as_deref();

    if root_override.is_some() {
        let (root, _) = resolve_granted_root(granted_folders, ".", root_override, false)?;
        let hits = search_granted_files(&root, &parsed.query)?;
        return Ok(if hits.is_empty() {
            "No matches found.".to_string()
        } else {
            hits.join("\n")
        });
    }

    let roots = collect_allowed_roots(granted_folders, false);
    if roots.is_empty() {
        anyhow::bail!("No folders are granted for this chat");
    }
    if roots.len() == 1 {
        let hits = search_granted_files(&roots[0], &parsed.query)?;
        return Ok(if hits.is_empty() {
            "No matches found.".to_string()
        } else {
            hits.join("\n")
        });
    }

    let mut sections = Vec::new();
    for root in roots {
        let hits = search_granted_files(&root, &parsed.query)?;
        if !hits.is_empty() {
            sections.push(format!("[{}]\n{}", root.display(), hits.join("\n")));
        }
    }
    if sections.is_empty() {
        Ok("No matches found.".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}
