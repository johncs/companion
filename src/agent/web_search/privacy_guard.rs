//! Pre-flight privacy leak detection and SSRF URL/IP validation for web search and fetch.

use std::collections::HashSet;
use std::net::IpAddr;

use anyhow::{Context, Result, bail};

use crate::i18n::{gettext, tr_f};

use super::html_parse::percent_decode;

pub(super) const MAX_QUERY_CHARS: usize = 160;
const OVERLAP_NGRAM_WORDS: usize = 5;

/// Inspect a proposed web search query for potential leaks of local files, paths, or secrets.
pub fn inspect_search_query(query: &str, local_texts: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    let trimmed = query.trim();

    if trimmed.contains('\n') || trimmed.contains('\r') {
        warnings.push(gettext(
            "Query contains multiple lines; review carefully before searching.",
        ));
    }
    if trimmed.chars().count() > MAX_QUERY_CHARS {
        warnings.push(tr_f(
            "Query is unusually long ({len} characters).",
            &[("len", &trimmed.chars().count().to_string())],
        ));
    }
    if contains_local_path(trimmed) {
        warnings.push(gettext(
            "Query appears to contain a local file path.",
        ));
    }
    if contains_secret_or_email(trimmed) {
        warnings.push(gettext(
            "Query may contain an email address, API key, or token.",
        ));
    }
    if has_local_text_overlap(trimmed, local_texts) {
        warnings.push(gettext(
            "Query matches text from a local file or attachment in this chat.",
        ));
    }

    warnings
}

fn contains_local_path(text: &str) -> bool {
    text.split_whitespace()
        .flat_map(|tok| tok.split(['&', '=']))
        .any(|tok| {
            let t = tok.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')'));
            t.starts_with("/home/")
                || t.starts_with("/var/home/")
                || t.starts_with("/Users/")
                || t.starts_with("/etc/")
                || t.starts_with("/tmp/")
                || t.starts_with("~/")
        })
}

fn contains_secret_or_email(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("bearer ")
        || lower.contains("api_key=")
        || lower.contains("token=")
        || lower.contains("password=")
    {
        return true;
    }
    for part in text
        .split_whitespace()
        .flat_map(|tok| tok.split(['&', '=']))
    {
        let clean = part.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';'));
        if clean.contains('@') && clean.contains('.') && !clean.starts_with('@') {
            return true;
        }
        if clean.starts_with("sk-")
            || clean.starts_with("ghp_")
            || clean.starts_with("AIza")
            || clean.starts_with("xoxb-")
        {
            return true;
        }
    }
    false
}

fn normalize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn has_local_text_overlap(query: &str, local_texts: &[String]) -> bool {
    let q_words = normalize_words(query);
    if q_words.len() < OVERLAP_NGRAM_WORDS || local_texts.is_empty() {
        return false;
    }

    let mut ngrams: HashSet<String> = HashSet::new();
    for win in q_words.windows(OVERLAP_NGRAM_WORDS) {
        ngrams.insert(win.join(" "));
    }

    for doc in local_texts {
        let d_words = normalize_words(doc);
        if d_words.len() < OVERLAP_NGRAM_WORDS {
            continue;
        }
        for win in d_words.windows(OVERLAP_NGRAM_WORDS) {
            if ngrams.contains(&win.join(" ")) {
                return true;
            }
        }
    }
    false
}

pub(super) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_private()
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_broadcast()
                && !v4.is_documentation()
                && !v4.is_unspecified()
                && !v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            let is_unique_local = (seg0 & 0xfe00) == 0xfc00;
            let is_link_local = (seg0 & 0xffc0) == 0xfe80;
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !v6.is_multicast()
                && !is_unique_local
                && !is_link_local
        }
    }
}

/// Validate that `raw_url` is a public `https://` URL (blocking `http://`, `file://`, localhost, and LAN IPs).
pub fn validate_fetch_url(raw_url: &str) -> Result<reqwest::Url> {
    let trimmed = raw_url.trim();
    let url = reqwest::Url::parse(trimmed).context("Invalid URL")?;
    if url.scheme() != "https" {
        bail!("Only https:// URLs are allowed");
    }
    let host = url
        .host_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .context("URL is missing a host")?;
    let host_lower = host.trim_end_matches('.').to_ascii_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".internal")
    {
        bail!("Local or private network hosts are not allowed");
    }
    let unbracketed = host_lower.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = unbracketed.parse::<IpAddr>()
        && !is_public_ip(ip)
    {
        bail!("Private or loopback IP addresses are not allowed");
    }
    Ok(url)
}

/// Inspect a proposed `fetch_url` target for potential leaks in query/path parameters.
pub fn inspect_fetch_url(raw_url: &str, local_texts: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Ok(url) = reqwest::Url::parse(raw_url.trim()) {
        if let Some(q) = url.query() {
            let decoded = percent_decode(q);
            warnings.extend(inspect_search_query(&decoded, local_texts));
        }
    } else {
        warnings.push(gettext("URL could not be parsed; check it carefully."));
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_query_flags_local_overlap_paths_and_secrets() {
        let local_docs = vec![
            "Confidential project roadmap for Aetherial Exorover launch in Q4.".to_string(),
        ];
        let leak = inspect_search_query("project roadmap for Aetherial Exorover launch", &local_docs);
        assert!(leak.iter().any(|w| w.contains("local file or attachment")));

        let path_warn = inspect_search_query("read /var/home/user/secret.txt", &[]);
        assert!(path_warn.iter().any(|w| w.contains("local file path")));

        let secret_warn = inspect_search_query("sk-1234567890abcdef", &[]);
        assert!(secret_warn.iter().any(|w| w.contains("API key, or token")));

        let clean = inspect_search_query("gtk4 rust listview example", &local_docs);
        assert!(clean.is_empty());
    }

    #[test]
    fn validate_fetch_url_enforces_https_and_blocks_private_hosts() {
        assert!(validate_fetch_url("https://www.espn.com/soccer").is_ok());
        assert!(validate_fetch_url("http://www.espn.com/soccer").is_err());
        assert!(validate_fetch_url("file:///etc/passwd").is_err());
        assert!(validate_fetch_url("https://localhost/admin").is_err());
        assert!(validate_fetch_url("https://127.0.0.1:8080/").is_err());
        assert!(validate_fetch_url("https://192.168.1.1/").is_err());
        assert!(validate_fetch_url("https://printer.local/").is_err());
    }

    #[test]
    fn inspect_fetch_url_detects_encoded_leaks_and_invalid_urls() {
        let secret_url = "https://example.com/search?q=sk-1234567890";
        let secret_warnings = inspect_fetch_url(secret_url, &[]);
        assert!(secret_warnings.iter().any(|w| w.contains("API key, or token")));

        let path_url = "https://example.com/lookup?path=%2Fhome%2Falice%2Fnotes.txt";
        let path_warnings = inspect_fetch_url(path_url, &[]);
        assert!(path_warnings.iter().any(|w| w.contains("local file path")));

        let bad_warnings = inspect_fetch_url("not a valid url", &[]);
        assert!(bad_warnings.iter().any(|w| w.contains("could not be parsed")));
    }

    #[test]
    fn is_public_ip_blocks_ipv6_private_and_loopback() {
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(!is_public_ip("fc00::1".parse().unwrap()));
        assert!(!is_public_ip("fe80::1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
