//! Bounded, path-jailed local tools and privacy-guarded web tools.

mod destructive_tools;
mod fs_ops;
mod path_guard;
mod read_tools;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_path_guard;
mod web_tools;
mod write_tools;

pub use destructive_tools::{DeleteDirectoryTool, DeleteFileTool};
pub use fs_ops::list_granted_files;
pub use path_guard::{GrantedFolder, find_covering_granted_folder};
pub use read_tools::{ListFilesTool, ReadFileTool, SearchFilesTool};
pub use web_tools::{FetchUrlTool, WebSearchTool};
pub use write_tools::{CreateDirectoryTool, MovePathTool, WriteFileTool};

/// Run a synchronous filesystem tool closure on Tokio's blocking pool.
pub(super) async fn run_blocking_tool<F>(f: F) -> anyhow::Result<String>
where
    F: FnOnce() -> anyhow::Result<String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
}
