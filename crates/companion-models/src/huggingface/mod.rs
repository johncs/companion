//! Hugging Face Hub GGUF repository search, matching, and resumable downloads.

mod client;
mod matcher;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

pub use client::{HfClient, part_file_path, remove_part_file};
pub use matcher::{
    derive_hf_search_query, score_repo_match, select_best_mmproj, select_starter_gguf,
};

pub const RECOMMENDED_STARTER_REPO: &str = "google/gemma-4-E4B-it-qat-q4_0-gguf";
pub const RECOMMENDED_STARTER_NAME: &str = "Gemma 4 E4B";

#[derive(Debug, Clone)]
pub struct HfRepoSummary {
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
}

#[derive(Debug, Clone)]
pub struct HfGgufFile {
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct MmprojMatch {
    pub repo_id: String,
    pub file: HfGgufFile,
}

/// Search Hugging Face for GGUF repositories matching `query`.
pub async fn search_gguf_repos(query: &str, limit: usize) -> Result<Vec<HfRepoSummary>> {
    HfClient::new().search_gguf_repos(query, limit).await
}

/// List `.gguf` files inside a Hugging Face model repository.
pub async fn list_repo_gguf_files(repo_id: &str) -> Result<Vec<HfGgufFile>> {
    HfClient::new().list_repo_gguf_files(repo_id).await
}

/// Download a GGUF file to `dest_path` using a `.part` temporary file with HTTP Range resume.
pub async fn download_gguf_file<F>(
    repo_id: &str,
    filename: &str,
    dest_path: &Path,
    cancel_flag: Arc<AtomicBool>,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>) + Send,
{
    HfClient::new()
        .download_gguf_file(repo_id, filename, dest_path, cancel_flag, on_progress)
        .await
}

/// Search Hugging Face for a repository matching `filename_or_name` that contains an
/// `mmproj` vision projector GGUF file.
pub async fn find_matching_mmproj(filename_or_name: &str) -> Result<Option<MmprojMatch>> {
    matcher::find_matching_mmproj_with(&HfClient::new(), filename_or_name).await
}
