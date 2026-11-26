//! Non-destructive workspace write tools: `write_file`, `create_directory`, and `move_path`.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::fs_ops::{create_granted_directory, move_granted_path, write_granted_file};
use super::path_guard::{
    GrantedFolder, prepare_workspace_auth, resolve_granted_path, resolve_granted_root,
    resolve_write_folder_for_raw,
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
    fn name(&self) -> &str {
        "write_file"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Write File"),
            description: gettext("Create or overwrite files inside a granted directory."),
            prompt_summary: "Create or overwrite a file in a [read & write] directory (requires user approval).",
            parameter_example: r#"{"root": "/granted/dir", "path": "relative/path", "content": "..."}"#,
            read_policy: "Ask",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<WriteFileArgs>(args.clone());
        Box::pin(async move {
            let Some(parsed) = parsed.ok().filter(|a| !a.path.trim().is_empty()) else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'path' or 'content' argument".to_string(),
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
            let preview = describe_write(ctx.granted_folders, &parsed);
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
            let parsed: WriteFileArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                write_granted_file(&root, &rel_path, &parsed.content)
            })
            .await
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
    fn name(&self) -> &str {
        "create_directory"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Create Directory"),
            description: gettext("Create subdirectories inside a granted directory."),
            prompt_summary: "Create a directory (and parents) in a [read & write] directory (requires user approval).",
            parameter_example: r#"{"root": "/granted/dir", "path": "relative/dir"}"#,
            read_policy: "Ask",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<CreateDirArgs>(args.clone());
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
            let preview = format!("Create directory:\n\n{}", parsed.path);
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
            let parsed: CreateDirArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_path) =
                    resolve_granted_root(&grants, &parsed.path, parsed.root.as_deref(), true)?;
                create_granted_directory(&root, &rel_path)
            })
            .await
        })
    }
}

/// Move or rename a file/directory inside a granted `[read & write]` directory.
pub struct MovePathTool;

impl Tool for MovePathTool {
    fn name(&self) -> &str {
        "move_path"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::WriteWorkspace
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Move / Rename"),
            description: gettext("Move or rename files and folders within a granted directory."),
            prompt_summary: "Move or rename a file or directory inside a [read & write] directory (requires user approval).",
            parameter_example: r#"{"root": "/granted/dir", "from": "old/path", "to": "new/path"}"#,
            read_policy: "Ask",
            rw_policy: "Allow",
        }
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<MovePathArgs>(args.clone());
        Box::pin(async move {
            let Some(parsed) = parsed
                .ok()
                .filter(|a| !a.from.trim().is_empty() && !a.to.trim().is_empty())
            else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'from' or 'to' argument".to_string(),
                };
            };
            let raw_target = parsed
                .root
                .clone()
                .unwrap_or_else(|| pick_move_target(ctx.granted_folders, &parsed));
            let write_folder = resolve_write_folder_for_raw(
                ctx.granted_folders,
                &parsed.from,
                parsed.root.as_deref(),
            )
            .or_else(|| {
                resolve_write_folder_for_raw(
                    ctx.granted_folders,
                    &parsed.to,
                    parsed.root.as_deref(),
                )
            });
            let preview = format!("Move / rename:\n\n{}\n→ {}", parsed.from, parsed.to);
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
            let parsed: MovePathArgs = prepared.downcast()?;
            run_blocking_tool(move || {
                let (root, rel_from) =
                    resolve_granted_root(&grants, &parsed.from, parsed.root.as_deref(), true)?;
                let to_is_abs = std::path::Path::new(&parsed.to).is_absolute();
                let to_override = (!to_is_abs).then(|| root.to_string_lossy().into_owned());
                let (to_root, rel_to) =
                    resolve_granted_root(&grants, &parsed.to, to_override.as_deref(), true)?;
                if to_root != root {
                    anyhow::bail!("Cannot move across different granted folders");
                }
                move_granted_path(&root, &rel_from, &rel_to)
            })
            .await
        })
    }
}

fn pick_move_target(granted_folders: &[GrantedFolder], parsed: &MovePathArgs) -> String {
    if super::path_guard::resolve_ungranted_raw(granted_folders, Some(&parsed.from), true).is_some()
    {
        parsed.from.clone()
    } else if super::path_guard::resolve_ungranted_raw(granted_folders, Some(&parsed.to), true)
        .is_some()
    {
        parsed.to.clone()
    } else {
        parsed.from.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::WebToolConfig;
    use companion_core::types::PermissionDecision;

    #[tokio::test]
    async fn move_path_allows_same_root_absolute_to_and_rejects_cross_root() {
        let base = std::env::temp_dir().join(format!("companion_mv_{}", uuid::Uuid::new_v4()));
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let canon_a = std::fs::canonicalize(&dir_a).unwrap();
        let canon_b = std::fs::canonicalize(&dir_b).unwrap();

        let src = canon_a.join("from.txt");
        let dst_same = canon_a.join("to.txt");
        let dst_cross = canon_b.join("to.txt");
        std::fs::write(&src, "hello").unwrap();

        let grants = vec![
            GrantedFolder {
                path: canon_a.clone(),
                allow_write: true,
            },
            GrantedFolder {
                path: canon_b,
                allow_write: true,
            },
        ];
        let ctx = ToolContext {
            granted_folders: &grants,
            web: WebToolConfig::default(),
        };
        let tool = MovePathTool;
        let approval = ApprovalResponse::Tool(PermissionDecision::AllowOnce);

        let same_prepared = PreparedToolCall::new(MovePathArgs {
            root: None,
            from: src.to_string_lossy().into(),
            to: dst_same.to_string_lossy().into(),
        });
        assert!(
            tool.execute_approved(&ctx, same_prepared, &approval)
                .await
                .is_ok()
        );
        assert!(dst_same.exists());

        let cross_prepared = PreparedToolCall::new(MovePathArgs {
            root: None,
            from: dst_same.to_string_lossy().into(),
            to: dst_cross.to_string_lossy().into(),
        });
        let err = tool
            .execute_approved(&ctx, cross_prepared, &approval)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("across different granted folders"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
