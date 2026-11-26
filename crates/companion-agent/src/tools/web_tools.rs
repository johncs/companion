//! Privacy-first network tools: `web_search` and `fetch_url`.

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::approval::ApprovalResponse;
use crate::tool::{
    ApprovalRequest, BoxFuture, PreparedToolCall, Tool, ToolAuthorization, ToolContext,
    ToolMetadata, ToolPrepareContext, ToolRisk,
};
use crate::web_search::{
    WebSearchService, inspect_fetch_url, inspect_search_query, validate_fetch_url,
};
use companion_core::i18n::{gettext, tr_f};
use companion_core::types::ToolCallStatus;

#[derive(Deserialize)]
struct WebSearchArgs {
    #[serde(default, alias = "q")]
    query: String,
}

#[derive(Deserialize)]
struct FetchUrlArgs {
    #[serde(default)]
    url: String,
}

/// Search the web via DuckDuckGo or SearXNG with pre-flight privacy inspection.
pub struct WebSearchTool {
    service: Arc<WebSearchService>,
}

impl WebSearchTool {
    pub fn new(service: Arc<WebSearchService>) -> Self {
        Self { service }
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Web Search"),
            description: gettext(
                "Search the web via DuckDuckGo or SearXNG when Web Search is enabled.",
            ),
            prompt_summary: "Search the web for up-to-date public information (always requires user approval).",
            parameter_example: r#"{"query": "concise search terms"}"#,
            read_policy: "Ask",
            rw_policy: "Ask",
        }
    }

    fn status_message(&self, ctx: &ToolContext<'_>) -> String {
        tr_f(
            "Searching {engine}…",
            &[("engine", &ctx.web.engine.label())],
        )
    }

    fn denied_message(&self) -> Cow<'static, str> {
        Cow::Borrowed("User denied web search.")
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<WebSearchArgs>(args.clone());
        Box::pin(async move {
            if !ctx.web.enabled {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Denied,
                    reason: "Web search is disabled for this chat.".to_string(),
                };
            }
            let Ok(mut parsed) = parsed else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'query' argument".to_string(),
                };
            };
            parsed.query = parsed.query.trim().to_string();
            if parsed.query.is_empty() {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'query' argument".to_string(),
                };
            }
            let warnings = inspect_search_query(&parsed.query, ctx.local_texts);
            let proposed_query = parsed.query.clone();
            ToolAuthorization::RequiresApproval {
                request: ApprovalRequest::WebSearch {
                    engine_label: ctx.web.engine.label(),
                    proposed_query,
                    warnings,
                },
                ungranted_path: None,
                write_target_folder: None,
                prepared: PreparedToolCall::new(parsed),
            }
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        prepared: PreparedToolCall,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let enabled = ctx.web.enabled;
        let engine = ctx.web.engine;
        let searxng_url = ctx.web.searxng_url.map(str::to_string);
        let service = self.service.clone();
        let override_query = match approval {
            ApprovalResponse::WebSearch(Some(q)) => Some(q.trim().to_string()),
            _ => None,
        };
        Box::pin(async move {
            if !enabled {
                bail!("Web search is disabled for this chat.");
            }
            let parsed: WebSearchArgs = prepared.downcast()?;
            let query = override_query.unwrap_or(parsed.query);
            service.search(engine, searxng_url.as_deref(), &query).await
        })
    }
}

/// Fetch readable text from a public `https://` URL with SSRF and privacy guards.
pub struct FetchUrlTool {
    service: Arc<WebSearchService>,
}

impl FetchUrlTool {
    pub fn new(service: Arc<WebSearchService>) -> Self {
        Self { service }
    }
}

impl Tool for FetchUrlTool {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            display_name: gettext("Fetch Web Page"),
            description: gettext(
                "Read plain text from an approved HTTPS web page when Web Search is enabled.",
            ),
            prompt_summary: "Read plain text from an approved HTTPS web page (always requires user approval).",
            parameter_example: r#"{"url": "https://example.com"}"#,
            read_policy: "Ask",
            rw_policy: "Ask",
        }
    }

    fn status_message(&self, _ctx: &ToolContext<'_>) -> String {
        gettext("Fetching web page…")
    }

    fn denied_message(&self) -> Cow<'static, str> {
        Cow::Borrowed("User denied web page fetch.")
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let parsed = serde_json::from_value::<FetchUrlArgs>(args.clone());
        Box::pin(async move {
            if !ctx.web.enabled {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Denied,
                    reason: "Web search is disabled for this chat.".to_string(),
                };
            }
            let Ok(mut parsed) = parsed else {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'url' argument".to_string(),
                };
            };
            parsed.url = parsed.url.trim().to_string();
            if parsed.url.is_empty() {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'url' argument".to_string(),
                };
            }
            if let Err(err) = validate_fetch_url(&parsed.url) {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: err.to_string(),
                };
            }
            let warnings = inspect_fetch_url(&parsed.url, ctx.local_texts);
            let proposed_url = parsed.url.clone();
            ToolAuthorization::RequiresApproval {
                request: ApprovalRequest::FetchUrl {
                    proposed_url,
                    warnings,
                },
                ungranted_path: None,
                write_target_folder: None,
                prepared: PreparedToolCall::new(parsed),
            }
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        prepared: PreparedToolCall,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let enabled = ctx.web.enabled;
        let service = self.service.clone();
        let override_url = match approval {
            ApprovalResponse::FetchUrl(Some(u)) => Some(u.trim().to_string()),
            _ => None,
        };
        Box::pin(async move {
            if !enabled {
                bail!("Web search is disabled for this chat.");
            }
            let parsed: FetchUrlArgs = prepared.downcast()?;
            let url = override_url.unwrap_or(parsed.url);
            service.fetch_url(&url).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionEngine;
    use crate::tool::WebToolConfig;
    use serde_json::json;

    #[tokio::test]
    async fn fetch_url_prepare_rejects_disabled_empty_and_unsafe_urls() {
        let perms = PermissionEngine::new();
        let tool = FetchUrlTool::new(Arc::new(WebSearchService::new().expect("web search")));

        let disabled_ctx = ToolPrepareContext {
            conv_id: "c1",
            granted_folders: &[],
            permissions: &perms,
            web: WebToolConfig::default(),
            local_texts: &[],
        };
        let res = tool
            .prepare(&disabled_ctx, &json!({"url": "https://example.com"}))
            .await;
        assert!(matches!(
            res,
            ToolAuthorization::Rejected {
                status: ToolCallStatus::Denied,
                ..
            }
        ));

        let enabled_ctx = ToolPrepareContext {
            web: WebToolConfig {
                enabled: true,
                ..Default::default()
            },
            ..disabled_ctx
        };
        let empty_res = tool.prepare(&enabled_ctx, &json!({"url": "  "})).await;
        assert!(matches!(
            empty_res,
            ToolAuthorization::Rejected {
                status: ToolCallStatus::Error,
                ..
            }
        ));

        let ssrf_res = tool
            .prepare(&enabled_ctx, &json!({"url": "http://127.0.0.1/"}))
            .await;
        assert!(matches!(
            ssrf_res,
            ToolAuthorization::Rejected {
                status: ToolCallStatus::Error,
                ..
            }
        ));
    }
}
