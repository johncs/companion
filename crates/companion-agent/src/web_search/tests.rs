use std::collections::VecDeque;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::transport::HttpResponse;
use super::*;

struct MockTransport {
    responses: Mutex<VecDeque<Result<HttpResponse>>>,
    requests: Mutex<Vec<String>>,
}

impl MockTransport {
    fn with_responses(items: Vec<Result<HttpResponse>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(items.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl HttpTransport for MockTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        query: &'a [(&'a str, &'a str)],
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>> {
        let q = query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        self.requests.lock().unwrap().push(format!("{url}?{q}"));
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| anyhow::bail!("no mock response"));
        Box::pin(async move { next })
    }

    fn fetch_pinned_hop<'a>(
        &'a self,
        url: &'a reqwest::Url,
        _pinned_addr: SocketAddr,
        _max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>> {
        self.requests.lock().unwrap().push(url.to_string());
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| anyhow::bail!("no mock hop response"));
        Box::pin(async move { next })
    }
}

#[tokio::test]
async fn duckduckgo_search_parses_html_results() {
    let html = r#"
        <div class="result">
            <a class="result__a" href="https://example.com/rust">Rust Lang</a>
            <a class="result__snippet">Systems programming language.</a>
        </div>
    "#;
    let transport = MockTransport::with_responses(vec![Ok(HttpResponse {
        status: 200,
        location: None,
        body: html.as_bytes().to_vec(),
    })]);
    let svc = WebSearchService::with_transport(transport.clone());

    let out = svc
        .search(SearchEngine::DuckDuckGo, None, "rust   lang")
        .await
        .unwrap();
    assert!(out.contains("**Rust Lang**"));
    assert!(out.contains("https://example.com/rust"));
    assert!(out.contains("Systems programming language."));
    assert!(
        transport.requests.lock().unwrap()[0].contains("q=rust+lang")
            || transport.requests.lock().unwrap()[0].contains("q=rust lang")
    );
}

#[tokio::test]
async fn searxng_search_parses_json_and_handles_http_errors() {
    let json = serde_json::json!({
        "results": [
            { "title": "GTK4", "url": "https://gtk.org", "content": "Toolkit   docs" },
            { "title": "", "url": "https://skip.me", "content": "ignored" }
        ]
    })
    .to_string();
    let transport = MockTransport::with_responses(vec![
        Ok(HttpResponse {
            status: 200,
            location: None,
            body: json.into_bytes(),
        }),
        Ok(HttpResponse {
            status: 503,
            location: None,
            body: Vec::new(),
        }),
    ]);
    let svc = WebSearchService::with_transport(transport);

    let ok_out = svc
        .search(
            SearchEngine::SearXNG,
            Some("https://search.example/"),
            "gtk",
        )
        .await
        .unwrap();
    assert!(ok_out.contains("**GTK4**"));
    assert!(ok_out.contains("Toolkit docs"));

    let err = svc
        .search(SearchEngine::SearXNG, Some("https://search.example"), "gtk")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("503"));
}

#[tokio::test]
async fn fetch_url_rejects_redirect_to_private_ip_and_truncates_large_bodies() {
    let resolver = |host: String, port: u16| async move {
        let ip = if host == "internal.example" {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))
        };
        Ok(vec![SocketAddr::new(ip, port)])
    };

    let ssrf_transport = MockTransport::with_responses(vec![Ok(HttpResponse {
        status: 302,
        location: Some("https://internal.example/admin".to_string()),
        body: Vec::new(),
    })]);
    let err = fetch_url_with_resolver(
        ssrf_transport.as_ref(),
        "https://example.com/start",
        resolver,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("private or loopback"));

    let big_body = format!(
        "<html><body><p>{}</p></body></html>",
        "a".repeat(MAX_FETCH_TEXT_CHARS + 50)
    );
    let trunc_transport = MockTransport::with_responses(vec![Ok(HttpResponse {
        status: 200,
        location: None,
        body: big_body.into_bytes(),
    })]);
    let out = fetch_url_with_resolver(
        trunc_transport.as_ref(),
        "https://example.com/article",
        resolver,
    )
    .await
    .unwrap();
    assert!(out.contains("[…page text truncated…]"));
}
