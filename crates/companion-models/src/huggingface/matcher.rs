//! Pure Hugging Face repository query derivation, scoring, and GGUF/mmproj selection.

use anyhow::Result;

use super::{HfClient, HfGgufFile, MmprojMatch};

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
            if let Some(prefix) = lower.strip_suffix(suffix)
                && (prefix.is_empty()
                    || prefix.ends_with('-')
                    || prefix.ends_with('_')
                    || prefix.ends_with('.'))
            {
                current = current[..prefix.len()].trim_end_matches(['-', '_', '.']);
                stripped = true;
                break;
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

pub(super) async fn find_matching_mmproj_with(
    client: &HfClient,
    filename_or_name: &str,
) -> Result<Option<MmprojMatch>> {
    let query = derive_hf_search_query(filename_or_name);
    if query.is_empty() {
        return Ok(None);
    }

    let mut repos = client.search_gguf_repos(&query, 8).await?;
    if repos.is_empty() {
        let friendly = crate::friendly_model_name(filename_or_name);
        if !friendly.is_empty() && !friendly.eq_ignore_ascii_case(&query) {
            repos = client.search_gguf_repos(&friendly, 8).await?;
        }
    }

    repos.sort_by(|a, b| {
        let sa = score_repo_match(&a.id, &query);
        let sb = score_repo_match(&b.id, &query);
        sb.cmp(&sa).then_with(|| b.downloads.cmp(&a.downloads))
    });

    for repo in repos.into_iter().take(5) {
        if let Ok(files) = client.list_repo_gguf_files(&repo.id).await
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
