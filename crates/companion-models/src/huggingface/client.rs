//! Injectable HTTP client for Hugging Face Hub API queries and Range-resumable downloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::header::RANGE;
use serde::Deserialize;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

use super::{HfGgufFile, HfRepoSummary};

#[derive(Debug, Deserialize)]
struct ApiRepo {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
}

#[derive(Debug, Deserialize)]
struct ApiTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    #[serde(default)]
    size: u64,
}

/// Compute the temporary `.part` path for `dest_path` by appending `.part` to the full filename.
pub fn part_file_path(dest_path: &Path) -> PathBuf {
    let mut os = dest_path.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

/// Remove the temporary `.part` file for `dest_path` if present, and prune the parent
/// folder if it is now empty.
pub async fn remove_part_file(dest_path: &Path) {
    let part_path = part_file_path(dest_path);
    let _ = tokio::fs::remove_file(part_path).await;
    if let Some(parent) = dest_path.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
}

/// Injectable Hugging Face Hub HTTP client.
#[derive(Clone)]
pub struct HfClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for HfClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HfClient {
    pub fn new() -> Self {
        Self::with_base_url("https://huggingface.co")
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("Companion-GNOME/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn search_gguf_repos(&self, query: &str, limit: usize) -> Result<Vec<HfRepoSummary>> {
        let search = if query.trim().is_empty() {
            "gguf"
        } else {
            query.trim()
        };
        let url = format!("{}/api/models", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("search", search),
                ("filter", "gguf"),
                ("sort", "downloads"),
                ("direction", "-1"),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?;

        let repos: Vec<ApiRepo> = resp.json().await?;
        Ok(repos
            .into_iter()
            .map(|r| HfRepoSummary {
                id: r.id,
                downloads: r.downloads,
                likes: r.likes,
            })
            .collect())
    }

    pub async fn list_repo_gguf_files(&self, repo_id: &str) -> Result<Vec<HfGgufFile>> {
        let url = format!("{}/api/models/{repo_id}/tree/main", self.base_url);
        let resp = self.http.get(&url).send().await?.error_for_status()?;
        let entries: Vec<ApiTreeEntry> = resp.json().await?;

        let mut files: Vec<HfGgufFile> = entries
            .into_iter()
            .filter(|e| e.entry_type == "file" && e.path.ends_with(".gguf"))
            .map(|e| HfGgufFile {
                filename: e.path,
                size_bytes: e.size,
            })
            .collect();

        files.sort_by_key(|f| f.size_bytes);
        Ok(files)
    }

    pub async fn download_gguf_file<F>(
        &self,
        repo_id: &str,
        filename: &str,
        dest_path: &Path,
        cancel_flag: Arc<AtomicBool>,
        mut on_progress: F,
    ) -> Result<()>
    where
        F: FnMut(u64, Option<u64>) + Send,
    {
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let part_path = part_file_path(dest_path);
        let existing_bytes = tokio::fs::metadata(&part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let url = format!("{}/{repo_id}/resolve/main/{filename}", self.base_url);
        let mut req = self.http.get(&url);
        if existing_bytes > 0 {
            req = req.header(RANGE, format!("bytes={existing_bytes}-"));
        }

        let resp = req.send().await?.error_for_status()?;
        let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let total_bytes = resp.content_length().map(|len| {
            if is_partial {
                len + existing_bytes
            } else {
                len
            }
        });

        let mut file = open_part_file(&part_path, is_partial).await?;
        let mut downloaded = if is_partial { existing_bytes } else { 0 };
        on_progress(downloaded, total_bytes);

        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel_flag.load(Ordering::Relaxed) {
                bail!("Download cancelled");
            }
            let bytes = chunk?;
            file.write_all(&bytes).await?;
            downloaded += bytes.len() as u64;
            on_progress(downloaded, total_bytes);
        }

        file.flush().await?;
        tokio::fs::rename(&part_path, dest_path).await?;
        Ok(())
    }
}

async fn open_part_file(part_path: &Path, append: bool) -> Result<tokio::fs::File> {
    if append {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(part_path)
            .await
            .context("Failed to open .part file for append")
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(part_path)
            .await
            .context("Failed to create .part file")
    }
}
