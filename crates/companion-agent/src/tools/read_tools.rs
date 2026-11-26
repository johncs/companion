//! Read-only workspace tools: `list_files`, `read_file`, and `search_files`.

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{
    MAX_LIST_ENTRIES, list_granted_files, read_granted_file, search_granted_files,
};
use super::path_guard::{
    GrantedFolder, collect_allowed_roots, prepare_workspace_auth, resolve_granted_root,
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
    fn name(&self) -> &str {
        "list_files"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("List Files"),
            description: gettext(
                "List files and folders inside a granted directory (respecting .gitignore).",
            ),
            prompt_summary: "List files in a directory (respecting .gitignore).",
            parameter_example: r#"{"root": "/path/to/dir", "subdir": "."}"#,
            read_policy: "Allow",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed =
            serde_json::from_value::<ListFilesArgs>(args.clone()).unwrap_or(ListFilesArgs {
                root: None,
                subdir: None,
            });
        Box::pin(async move {
            let target = parsed
                .root
                .as_deref()
                .or(parsed.subdir.as_deref())
                .unwrap_or(".")
                .to_string();
            let raw_target = parsed.root.clone().or(parsed.subdir.clone());
            let preview = format!("List files in:\n\n{target}");
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                raw_target.as_deref(),
                None,
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
            let parsed: ListFilesArgs = prepared.downcast()?;
            run_blocking_tool(move || exec_list_files(&grants, parsed)).await
        })
    }
}

fn run_across_roots(
    granted_folders: &[GrantedFolder],
    empty_label: &str,
    include_empty_sections: bool,
    mut op: impl FnMut(&Path) -> Result<Vec<String>>,
) -> Result<String> {
    let roots = collect_allowed_roots(granted_folders, false);
    if roots.is_empty() {
        anyhow::bail!("No folders are granted for this chat");
    }
    if roots.len() == 1 {
        let lines = op(&roots[0])?;
        return Ok(if lines.is_empty() {
            empty_label.to_string()
        } else {
            lines.join("\n")
        });
    }
    let mut sections = Vec::new();
    for root in roots {
        let lines = op(&root)?;
        if lines.is_empty() {
            if include_empty_sections {
                sections.push(format!("[{}]\n{empty_label}", root.display()));
            }
        } else {
            sections.push(format!("[{}]\n{}", root.display(), lines.join("\n")));
        }
    }
    Ok(if sections.is_empty() {
        empty_label.to_string()
    } else {
        sections.join("\n\n")
    })
}

fn exec_list_files(granted_folders: &[GrantedFolder], parsed: ListFilesArgs) -> Result<String> {
    let subdir = parsed.subdir.as_deref().unwrap_or(".");
    let root_override = parsed.root.as_deref();

    if root_override.is_some() || subdir != "." {
        let (root, rel_sub) = resolve_granted_root(granted_folders, subdir, root_override, false)?;
        let files = list_granted_files(&root, &rel_sub, MAX_LIST_ENTRIES)?;
        return Ok(if files.is_empty() {
            "(empty directory)".to_string()
        } else {
            files.join("\n")
        });
    }

    run_across_roots(granted_folders, "(empty directory)", true, |root| {
        list_granted_files(root, ".", MAX_LIST_ENTRIES)
    })
}

/// Read a UTF-8 text file inside a granted folder or granted single file.
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn produces_sensitive_local_context(&self) -> bool {
        true
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Read File"),
            description: gettext(
                "Read UTF-8 text files up to 100 KB (prompts if the path is not granted yet).",
            ),
            prompt_summary: "Read a UTF-8 text file.",
            parameter_example: r#"{"path": "/path/to/file"}"#,
            read_policy: "Allow",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<ReadFileArgs>(args.clone());
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
            let preview = format!("Read file:\n\n{}", parsed.path);
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                Some(&raw_target),
                None,
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
            let parsed: ReadFileArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), false)?;
                read_granted_file(&root, &rel_path)
            })
            .await
        })
    }
}

/// Search for text across granted files/directories.
pub struct SearchFilesTool;

impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::ReadOnly
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Search Files"),
            description: gettext(
                "Search for matching text lines across granted files and folders.",
            ),
            prompt_summary: "Search for text across files/directories.",
            parameter_example: r#"{"root": "/path/to/dir", "query": "text"}"#,
            read_policy: "Allow",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<SearchFilesArgs>(args.clone());
        Box::pin(async move {
            let Some(parsed) = parsed.ok().filter(|a| !a.query.is_empty()) else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'query' argument".to_string(),
                };
            };
            let raw_target = parsed.root.clone();
            let root_label = raw_target.as_deref().unwrap_or("granted folders");
            let preview = format!("Search for \"{}\" in:\n\n{root_label}", parsed.query);
            prepare_workspace_auth(
                ctx,
                self.name(),
                self.risk(),
                raw_target.as_deref(),
                None,
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
            let parsed: SearchFilesArgs = prepared.downcast()?;
            run_blocking_tool(move || exec_search_files(&grants, parsed)).await
        })
    }
}

fn exec_search_files(granted_folders: &[GrantedFolder], parsed: SearchFilesArgs) -> Result<String> {
    if let Some(root_override) = parsed.root.as_deref() {
        let (root, _) = resolve_granted_root(granted_folders, ".", Some(root_override), false)?;
        let hits = search_granted_files(&root, &parsed.query)?;
        return Ok(if hits.is_empty() {
            "No matches found.".to_string()
        } else {
            hits.join("\n")
        });
    }

    run_across_roots(granted_folders, "No matches found.", false, |root| {
        search_granted_files(root, &parsed.query)
    })
}
