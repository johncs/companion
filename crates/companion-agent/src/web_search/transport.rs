use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};

const USER_AGENT: &str = "Companion/0.1 (Privacy-First Local AI)";

pub struct HttpResponse {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

pub trait HttpTransport: Send + Sync {
    fn get<'a>(
        &'a self,
        url: &'a str,
        query: &'a [(&'a str, &'a str)],
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>>;

    fn fetch_pinned_hop<'a>(
        &'a self,
        url: &'a reqwest::Url,
        pinned_addr: SocketAddr,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>>;
}

pub struct ReqwestTransport {
    search_client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        let search_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(4))
            .build()
            .context("Failed to initialize HTTP client")?;
        Ok(Self { search_client })
    }
}

impl HttpTransport for ReqwestTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        query: &'a [(&'a str, &'a str)],
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self
                .search_client
                .get(url)
                .query(query)
                .send()
                .await
                .context("HTTP request failed")?;
            let status = resp.status().as_u16();
            let body = resp
                .bytes()
                .await
                .context("Failed to read HTTP response")?
                .to_vec();
            Ok(HttpResponse {
                status,
                location: None,
                body,
            })
        })
    }

    fn fetch_pinned_hop<'a>(
        &'a self,
        url: &'a reqwest::Url,
        pinned_addr: SocketAddr,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            let host = url.host_str().context("URL is missing a host")?;
            // Pin the exact public SocketAddr we just validated to prevent DNS-rebinding TOCTOU.
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(12))
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .resolve(host, pinned_addr)
                .build()
                .context("Failed to initialize pinned HTTP fetch client")?;

            let mut resp = client
                .get(url.clone())
                .send()
                .await
                .context("Failed to fetch web page")?;
            let status = resp.status().as_u16();
            if resp.status().is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                return Ok(HttpResponse {
                    status,
                    location,
                    body: Vec::new(),
                });
            }

            let mut body = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .context("Failed while reading web page")?
            {
                let remaining = max_bytes.saturating_sub(body.len());
                if remaining == 0 {
                    break;
                }
                let take = chunk.len().min(remaining);
                body.extend_from_slice(&chunk[..take]);
                if body.len() >= max_bytes {
                    break;
                }
            }
            Ok(HttpResponse {
                status,
                location: None,
                body,
            })
        })
    }
}
