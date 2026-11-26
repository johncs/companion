//! Injectable HTTP client for Hugging Face Hub API queries and Range-resumable downloads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, ETAG, IF_RANGE, RANGE};
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

fn etag_file_path(dest_path: &Path) -> PathBuf {
    let mut os = dest_path.as_os_str().to_os_string();
    os.push(".part.etag");
    PathBuf::from(os)
}

/// Remove the temporary `.part` file (and `.part.etag`) for `dest_path` if present,
/// and prune the parent folder if it is now empty.
pub async fn remove_part_file(dest_path: &Path) {
    let _ = tokio::fs::remove_file(part_file_path(dest_path)).await;
    let _ = tokio::fs::remove_file(etag_file_path(dest_path)).await;
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
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn find_matching_mmproj(
        &self,
        filename_or_name: &str,
    ) -> Result<Option<super::MmprojMatch>> {
        super::matcher::find_matching_mmproj_with(self, filename_or_name).await
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
        let resp = self
            .http
            .get(&url)
            .query(&[("recursive", "true")])
            .send()
            .await?
            .error_for_status()?;
        let entries: Vec<ApiTreeEntry> = resp.json().await?;

        let mut files: Vec<HfGgufFile> = entries
            .into_iter()
            .filter(|e| {
                e.entry_type == "file"
                    && e.path
                        .rsplit('.')
                        .next()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
            })
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
        let etag_path = etag_file_path(dest_path);
        let url = format!("{}/{repo_id}/resolve/main/{filename}", self.base_url);

        let Some((resp, existing_bytes)) = self
            .open_resumable_response(&url, &part_path, &etag_path, dest_path)
            .await?
        else {
            report_already_complete(dest_path, &mut on_progress).await;
            return Ok(());
        };

        save_resumable_response(
            resp,
            existing_bytes,
            &part_path,
            &etag_path,
            dest_path,
            &cancel_flag,
            &mut on_progress,
        )
        .await
    }
}

async fn save_resumable_response<F: FnMut(u64, Option<u64>) + Send>(
    resp: reqwest::Response,
    existing_bytes: u64,
    part_path: &Path,
    etag_path: &Path,
    dest_path: &Path,
    cancel_flag: &AtomicBool,
    on_progress: &mut F,
) -> Result<()> {
    let is_partial = resp.status() == StatusCode::PARTIAL_CONTENT;
    validate_partial_range(&resp, is_partial, existing_bytes, etag_path).await?;
    let total_bytes = resp.content_length().map(|len| {
        if is_partial {
            len.saturating_add(existing_bytes)
        } else {
            len
        }
    });
    let mut file = open_part_file(part_path, is_partial).await?;
    let mut downloaded = if is_partial { existing_bytes } else { 0 };
    on_progress(downloaded, total_bytes);
    stream_response_to_file(
        resp,
        &mut file,
        &mut downloaded,
        total_bytes,
        cancel_flag,
        on_progress,
    )
    .await?;
    finalize_part_file(
        file,
        downloaded,
        total_bytes,
        part_path,
        etag_path,
        dest_path,
    )
    .await
}

async fn report_already_complete<F: FnMut(u64, Option<u64>)>(
    dest_path: &Path,
    on_progress: &mut F,
) {
    let done_len = tokio::fs::metadata(dest_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    on_progress(done_len, Some(done_len));
}

async fn finalize_part_file(
    mut file: tokio::fs::File,
    downloaded: u64,
    total_bytes: Option<u64>,
    part_path: &Path,
    etag_path: &Path,
    dest_path: &Path,
) -> Result<()> {
    if let Some(expected) = total_bytes
        && expected > 0
        && downloaded != expected
    {
        bail!("Download incomplete: received {downloaded} of {expected} bytes");
    }
    file.flush().await?;
    drop(file);
    let _ = tokio::fs::remove_file(etag_path).await;
    tokio::fs::rename(part_path, dest_path).await?;
    Ok(())
}

async fn validate_partial_range(
    resp: &reqwest::Response,
    is_partial: bool,
    existing_bytes: u64,
    etag_path: &Path,
) -> Result<()> {
    if is_partial {
        let valid_range = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|cr| cr.starts_with(&format!("bytes {existing_bytes}-")));
        if !valid_range {
            bail!("Server returned unexpected Content-Range for resumed download");
        }
    }
    if let Some(etag) = resp.headers().get(ETAG).and_then(|v| v.to_str().ok()) {
        let _ = tokio::fs::write(etag_path, etag).await;
    }
    Ok(())
}

async fn stream_response_to_file<F>(
    resp: reqwest::Response,
    file: &mut tokio::fs::File,
    downloaded: &mut u64,
    total_bytes: Option<u64>,
    cancel_flag: &AtomicBool,
    on_progress: &mut F,
) -> Result<()>
where
    F: FnMut(u64, Option<u64>) + Send,
{
    let mut stream = resp.bytes_stream();
    let mut cancel_tick = tokio::time::interval(Duration::from_millis(100));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            bail!("Download cancelled");
        }
        let next_item = tokio::select! {
            item = stream.next() => item,
            _ = cancel_tick.tick() => continue,
        };
        let Some(chunk) = next_item else {
            break;
        };
        let bytes = chunk?;
        file.write_all(&bytes).await?;
        *downloaded = downloaded.saturating_add(bytes.len() as u64);
        on_progress(*downloaded, total_bytes);
    }
    Ok(())
}

impl HfClient {
    async fn open_resumable_response(
        &self,
        url: &str,
        part_path: &Path,
        etag_path: &Path,
        dest_path: &Path,
    ) -> Result<Option<(reqwest::Response, u64)>> {
        let mut existing_bytes = tokio::fs::metadata(part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let saved_etag = tokio::fs::read_to_string(etag_path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        loop {
            let mut req = self.http.get(url);
            if existing_bytes > 0 {
                req = req.header(RANGE, format!("bytes={existing_bytes}-"));
                if let Some(ref etag) = saved_etag {
                    req = req.header(IF_RANGE, etag);
                }
            }
            let r = req.send().await?;
            if r.status() == StatusCode::RANGE_NOT_SATISFIABLE && existing_bytes > 0 {
                if let Ok(head) = self.http.head(url).send().await
                    && let Some(remote_len) = head.content_length()
                    && remote_len == existing_bytes
                {
                    let _ = tokio::fs::remove_file(etag_path).await;
                    tokio::fs::rename(part_path, dest_path).await?;
                    return Ok(None);
                }
                let _ = tokio::fs::remove_file(part_path).await;
                let _ = tokio::fs::remove_file(etag_path).await;
                existing_bytes = 0;
                continue;
            }
            return Ok(Some((r.error_for_status()?, existing_bytes)));
        }
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
