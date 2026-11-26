//! Privacy-first web search tool (DuckDuckGo / custom SearXNG) with pre-flight leak detection.

mod html_parse;
mod privacy_guard;
#[cfg(test)]
mod tests;
mod transport;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use html_parse::{extract_readable_text, parse_duckduckgo_html};
use privacy_guard::{MAX_QUERY_CHARS, is_public_ip};
use transport::ReqwestTransport;

pub use companion_core::types::SearchEngine;
pub use privacy_guard::{inspect_fetch_url, inspect_search_query, validate_fetch_url};
#[cfg(test)]
pub(crate) use transport::HttpResponse;
pub(crate) use transport::HttpTransport;

pub(super) const MAX_RESULTS: usize = 6;
const MAX_FETCH_BYTES: usize = 512 * 1024;
const MAX_FETCH_TEXT_CHARS: usize = 12_000;
const MAX_FETCH_REDIRECTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Injectable web search and URL fetch service backed by an [`HttpTransport`].
#[derive(Clone)]
pub struct WebSearchService {
    transport: Arc<dyn HttpTransport>,
}

impl WebSearchService {
    pub fn new() -> Result<Self> {
        Ok(Self {
            transport: Arc::new(ReqwestTransport::new()?),
        })
    }

    pub fn with_transport(transport: Arc<dyn HttpTransport>) -> Self {
        Self { transport }
    }

    /// Run a tracker-free web search and return formatted results.
    pub async fn search(
        &self,
        engine: SearchEngine,
        searxng_url: Option<&str>,
        query: &str,
    ) -> Result<String> {
        let clean_query: String = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_QUERY_CHARS)
            .collect();
        if clean_query.is_empty() {
            bail!("Search query cannot be empty");
        }

        let results = match engine {
            SearchEngine::DuckDuckGo => {
                search_duckduckgo(self.transport.as_ref(), &clean_query).await?
            }
            SearchEngine::SearXNG => {
                let base = searxng_url
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .context("SearXNG URL is not configured in Settings → Agent & MCP")?;
                search_searxng(self.transport.as_ref(), base, &clean_query).await?
            }
        };

        if results.is_empty() {
            return Ok("No web search results found.".to_string());
        }

        let mut out = String::new();
        for (idx, r) in results.iter().take(MAX_RESULTS).enumerate() {
            if idx > 0 {
                out.push_str("\n\n");
            }
            out.push_str(&format!("{}. **{}**\n   {}\n", idx + 1, r.title, r.url));
            if !r.snippet.is_empty() {
                out.push_str(&format!("   {}", r.snippet));
            }
        }
        Ok(out)
    }

    /// Fetch an approved `https://` web page with pre-hop DNS-pinned SSRF checks.
    pub async fn fetch_url(&self, raw_url: &str) -> Result<String> {
        fetch_url_with_resolver(self.transport.as_ref(), raw_url, |host, port| async move {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map(|addrs| addrs.collect())
                .with_context(|| format!("Failed to resolve host: {host}"))
        })
        .await
    }
}

async fn search_duckduckgo(
    transport: &dyn HttpTransport,
    query: &str,
) -> Result<Vec<SearchResult>> {
    let resp = transport
        .get("https://html.duckduckgo.com/html/", &[("q", query)])
        .await
        .context("DuckDuckGo search request failed")?;
    if !(200..300).contains(&resp.status) {
        bail!("DuckDuckGo returned HTTP {}", resp.status);
    }
    let html = String::from_utf8_lossy(&resp.body);
    Ok(parse_duckduckgo_html(&html))
}

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngItem>,
}

#[derive(Deserialize)]
struct SearxngItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

async fn search_searxng(
    transport: &dyn HttpTransport,
    base_url: &str,
    query: &str,
) -> Result<Vec<SearchResult>> {
    let trimmed = base_url.trim_end_matches('/');
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        bail!("SearXNG URL must start with http:// or https://");
    }
    let endpoint = format!("{trimmed}/search");
    let resp = transport
        .get(&endpoint, &[("q", query), ("format", "json")])
        .await
        .with_context(|| format!("SearXNG request failed ({endpoint})"))?;
    if !(200..300).contains(&resp.status) {
        bail!("SearXNG returned HTTP {}", resp.status);
    }
    let parsed: SearxngResponse =
        serde_json::from_slice(&resp.body).context("Failed to parse SearXNG JSON response")?;
    Ok(parsed
        .results
        .into_iter()
        .filter(|r| !r.url.trim().is_empty() && !r.title.trim().is_empty())
        .take(MAX_RESULTS)
        .map(|r| SearchResult {
            title: r.title.trim().to_string(),
            url: r.url.trim().to_string(),
            snippet: r.content.split_whitespace().collect::<Vec<_>>().join(" "),
        })
        .collect())
}

async fn validate_and_resolve_public_url<R, Fut>(
    raw_url: &str,
    resolve: &mut R,
) -> Result<(reqwest::Url, SocketAddr)>
where
    R: FnMut(String, u16) -> Fut,
    Fut: Future<Output = Result<Vec<SocketAddr>>>,
{
    let url = validate_fetch_url(raw_url)?;
    let host = url.host_str().context("URL is missing a host")?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);

    // Verify DNS resolution does not point to loopback or private LAN addresses, and pin the first IP.
    let addrs = resolve(host, port).await?;
    let mut first_addr = None;
    for addr in addrs {
        if !is_public_ip(addr.ip()) {
            bail!("Blocked connection to private or loopback IP address");
        }
        if first_addr.is_none() {
            first_addr = Some(addr);
        }
    }
    let pinned = first_addr.context("Host did not resolve to any IP address")?;
    Ok((url, pinned))
}

async fn fetch_url_with_resolver<R, Fut>(
    transport: &dyn HttpTransport,
    raw_url: &str,
    mut resolve: R,
) -> Result<String>
where
    R: FnMut(String, u16) -> Fut,
    Fut: Future<Output = Result<Vec<SocketAddr>>>,
{
    let (mut current_url, mut pinned_addr) =
        validate_and_resolve_public_url(raw_url, &mut resolve).await?;

    let mut final_resp = None;
    for _hop in 0..=MAX_FETCH_REDIRECTS {
        let resp = transport
            .fetch_pinned_hop(&current_url, pinned_addr, MAX_FETCH_BYTES)
            .await?;
        if (300..400).contains(&resp.status) {
            let location = resp
                .location
                .as_deref()
                .context("Redirect response missing Location header")?;
            let next = current_url
                .join(location.trim())
                .context("Invalid redirect URL")?;
            (current_url, pinned_addr) =
                validate_and_resolve_public_url(next.as_str(), &mut resolve).await?;
        } else {
            final_resp = Some(resp);
            break;
        }
    }
    let resp = final_resp.context("Too many redirects while fetching web page")?;
    if !(200..300).contains(&resp.status) {
        bail!("Web page returned HTTP {}", resp.status);
    }

    let raw_text = String::from_utf8_lossy(&resp.body);
    let extracted = extract_readable_text(&raw_text);
    if extracted.is_empty() {
        return Ok(
            "The page did not contain readable text (it may require JavaScript).".to_string(),
        );
    }

    let truncated: String = extracted.chars().take(MAX_FETCH_TEXT_CHARS).collect();
    let suffix = if extracted.chars().count() > MAX_FETCH_TEXT_CHARS {
        "\n\n[…page text truncated…]"
    } else {
        ""
    };

    Ok(format!(
        "Fetched content from {}:\n\n{}{}",
        current_url, truncated, suffix
    ))
}
