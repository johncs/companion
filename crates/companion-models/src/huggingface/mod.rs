//! Hugging Face Hub GGUF repository search, matching, and resumable downloads.

mod client;
mod matcher;
#[cfg(test)]
mod tests;

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
