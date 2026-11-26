//! Hugging Face Hub GGUF repository search and resumable model downloader.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("Companion-GNOME/0.1")
        .build()
        .expect("failed to build reqwest client")
});

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::header::RANGE;
use serde::Deserialize;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

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

/// Search Hugging Face for GGUF repositories matching `query`.
pub async fn search_gguf_repos(query: &str, limit: usize) -> Result<Vec<HfRepoSummary>> {
    let search = if query.trim().is_empty() {
        "gguf"
    } else {
        query.trim()
    };

    let resp = HTTP_CLIENT
        .get("https://huggingface.co/api/models")
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

/// List `.gguf` files inside a Hugging Face model repository.
pub async fn list_repo_gguf_files(repo_id: &str) -> Result<Vec<HfGgufFile>> {
    let url = format!("https://huggingface.co/api/models/{repo_id}/tree/main");
    let resp = HTTP_CLIENT.get(&url).send().await?.error_for_status()?;
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

/// Download a GGUF file to `dest_path` using a `.part` temporary file with HTTP Range resume.
pub async fn download_gguf_file<F>(
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

    let part_path = dest_path.with_extension("gguf.part");
    let existing_bytes = tokio::fs::metadata(&part_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let url = format!("https://huggingface.co/{repo_id}/resolve/main/{filename}");
    let mut req = HTTP_CLIENT.get(&url);
    if existing_bytes > 0 {
        req = req.header(RANGE, format!("bytes={existing_bytes}-"));
    }

    let resp = req.send().await?.error_for_status()?;
    let is_partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let total_bytes = resp
        .content_length()
        .map(|len| if is_partial { len + existing_bytes } else { len });

    let mut file = if is_partial {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
            .await
            .context("Failed to open .part file for append")?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part_path)
            .await
            .context("Failed to create .part file")?
    };

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

/// Remove the temporary `.gguf.part` file for `dest_path` if present, and prune the parent
/// folder if it is now empty.
pub async fn remove_part_file(dest_path: &Path) {
    let part_path = dest_path.with_extension("gguf.part");
    let _ = tokio::fs::remove_file(part_path).await;
    if let Some(parent) = dest_path.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
}

/// Derive a clean Hugging Face search query from a local `.gguf` filename or model name
/// by removing the `.gguf` extension and trailing quantization/packaging tags.
pub fn derive_hf_search_query(filename_or_name: &str) -> String {
    let trimmed = filename_or_name.trim();
    let stem = trimmed
        .strip_suffix(".gguf")
        .or_else(|| trimmed.strip_suffix(".GGUF"))
        .unwrap_or(trimmed);

    let without_quant = if let Some(quant) = crate::infer_quantization(stem) {
        let lower_stem = stem.to_ascii_lowercase();
        let lower_q = quant.to_ascii_lowercase();
        if let Some(pos) = lower_stem.rfind(&lower_q) {
            stem[..pos].trim_end_matches(['-', '_', '.'])
        } else {
            stem
        }
    } else {
        stem
    };

    let mut current = without_quant.trim_end_matches(['-', '_', '.']);
    let extra_suffixes = ["ud", "imatrix", "qat", "fp16", "bf16", "f16", "f32"];
    loop {
        let lower = current.to_ascii_lowercase();
        let mut stripped = false;
        for suffix in extra_suffixes {
            if let Some(prefix) = lower.strip_suffix(suffix) {
                if prefix.is_empty()
                    || prefix.ends_with('-')
                    || prefix.ends_with('_')
                    || prefix.ends_with('.')
                {
                    current = current[..prefix.len()].trim_end_matches(['-', '_', '.']);
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }

    current.trim().to_string()
}

fn tokenize_model_ident(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .filter(|t| !t.is_empty())
        .map(|t| t.trim_matches('.').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

fn is_size_token(token: &str) -> bool {
    let body = token
        .strip_prefix('e')
        .or_else(|| token.strip_prefix('a'))
        .unwrap_or(token);
    let Some(num) = body.strip_suffix('b') else {
        return false;
    };
    !num.is_empty() && num.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Score how well a Hugging Face `repo_id` matches a derived model `query`.
/// Parameter-size tokens (e.g. `e4b`, `8b`, `12b`) are weighted heavily so an exact
/// size match beats more popular sibling sizes.
pub fn score_repo_match(repo_id: &str, query: &str) -> i32 {
    let query_tokens = tokenize_model_ident(query);
    if query_tokens.is_empty() {
        return 0;
    }

    let repo_name = repo_id.split('/').next_back().unwrap_or(repo_id);
    let repo_tokens = tokenize_model_ident(repo_name);
    let repo_has_any_size = repo_tokens.iter().any(|t| is_size_token(t));
    let mut score = 0i32;

    for token in &query_tokens {
        if is_size_token(token) {
            if repo_tokens.iter().any(|rt| rt == token) {
                score += 50;
            } else if repo_has_any_size {
                score -= 40;
            }
        } else if repo_tokens.iter().any(|rt| rt == token) {
            score += 10;
        }
    }

    let query_lower = query.to_ascii_lowercase();
    if repo_name.to_ascii_lowercase().contains(&query_lower) {
        score += 20;
    }

    score
}

/// Pick the best `mmproj` file from a repository's GGUF listing, preferring `BF16`/`F16`
/// over `Q8_0` or oversized `F32` builds.
pub fn select_best_mmproj(files: &[HfGgufFile]) -> Option<HfGgufFile> {
    files
        .iter()
        .filter(|f| f.filename.to_ascii_lowercase().contains("mmproj"))
        .min_by_key(|f| (crate::mmproj_precision_rank(&f.filename), f.size_bytes))
        .cloned()
}

/// Select the best general-purpose starter GGUF model and optional `mmproj` projector
/// from a repository's file list (preferring `Q4_K_M`, `Q4_K_XL`, `Q4_K_S`, `Q4_0`).
pub fn select_starter_gguf(files: &[HfGgufFile]) -> Option<(HfGgufFile, Option<HfGgufFile>)> {
    let mmproj = select_best_mmproj(files);
    let model = files
        .iter()
        .filter(|f| !crate::is_auxiliary_gguf_name(&f.filename))
        .min_by_key(|f| {
            let quant = crate::infer_quantization(&f.filename).unwrap_or_default();
            let rank = match quant.as_str() {
                "Q4_K_M" => 0u8,
                "Q4_K_XL" => 1,
                "Q4_K_S" => 2,
                "Q4_0" => 3,
                "IQ4_XS" => 4,
                "Q5_K_M" => 5,
                _ => 6,
            };
            (rank, f.size_bytes)
        })
        .cloned()?;
    Some((model, mmproj))
}

/// Search Hugging Face for a repository matching `filename_or_name` that contains an
/// `mmproj` vision projector GGUF file.
pub async fn find_matching_mmproj(filename_or_name: &str) -> Result<Option<MmprojMatch>> {
    let query = derive_hf_search_query(filename_or_name);
    if query.is_empty() {
        return Ok(None);
    }

    let mut repos = search_gguf_repos(&query, 8).await?;
    if repos.is_empty() {
        let friendly = crate::friendly_model_name(filename_or_name);
        if !friendly.is_empty() && !friendly.eq_ignore_ascii_case(&query) {
            repos = search_gguf_repos(&friendly, 8).await?;
        }
    }

    repos.sort_by(|a, b| {
        let sa = score_repo_match(&a.id, &query);
        let sb = score_repo_match(&b.id, &query);
        sb.cmp(&sa).then_with(|| b.downloads.cmp(&a.downloads))
    });

    for repo in repos.into_iter().take(5) {
        if let Ok(files) = list_repo_gguf_files(&repo.id).await
            && let Some(file) = select_best_mmproj(&files)
        {
            return Ok(Some(MmprojMatch {
                repo_id: repo.id,
                file,
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_query_strips_extension_and_quantization() {
        assert_eq!(
            derive_hf_search_query("gemma-4-e4b-it-q4_k_m.gguf"),
            "gemma-4-e4b-it"
        );
        assert_eq!(
            derive_hf_search_query("Qwen3-VL-8B-Instruct-UD-Q4_K_XL.gguf"),
            "Qwen3-VL-8B-Instruct"
        );
        assert_eq!(
            derive_hf_search_query("Llama-3.2-11B-Vision-Instruct.Q4_K_M.gguf"),
            "Llama-3.2-11B-Vision-Instruct"
        );
    }

    #[test]
    fn score_prefers_matching_parameter_size() {
        let query = "gemma-4-e4b-it";
        let exact = score_repo_match("unsloth/gemma-4-e4b-it-GGUF", query);
        let wrong_size = score_repo_match("unsloth/gemma-4-27b-it-GGUF", query);
        assert!(exact > wrong_size);
    }

    #[test]
    fn select_best_mmproj_prefers_bf16_or_f16_over_f32() {
        let files = vec![
            HfGgufFile {
                filename: "gemma-4-e4b-it-Q4_K_M.gguf".to_string(),
                size_bytes: 2_500_000_000,
            },
            HfGgufFile {
                filename: "mmproj-F32.gguf".to_string(),
                size_bytes: 1_700_000_000,
            },
            HfGgufFile {
                filename: "mmproj-BF16.gguf".to_string(),
                size_bytes: 850_000_000,
            },
        ];
        let best = select_best_mmproj(&files).expect("expected mmproj");
        assert_eq!(best.filename, "mmproj-BF16.gguf");
    }

    #[test]
    fn select_starter_gguf_prefers_q4_k_m_and_pairs_mmproj() {
        let files = vec![
            HfGgufFile {
                filename: "mtp-gemma-4-E2B-it-Q4_K_M.gguf".to_string(),
                size_bytes: 500_000_000,
            },
            HfGgufFile {
                filename: "gemma-4-E2B-it-Q8_0.gguf".to_string(),
                size_bytes: 5_000_000_000,
            },
            HfGgufFile {
                filename: "gemma-4-E2B-it-Q4_K_M.gguf".to_string(),
                size_bytes: 3_100_000_000,
            },
            HfGgufFile {
                filename: "mmproj-gemma-4-E2B-it-Q8_0.gguf".to_string(),
                size_bytes: 450_000_000,
            },
        ];
        let (model, mmproj) = select_starter_gguf(&files).expect("expected starter pair");
        assert_eq!(model.filename, "gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            mmproj.map(|m| m.filename).as_deref(),
            Some("mmproj-gemma-4-E2B-it-Q8_0.gguf")
        );
    }
}
