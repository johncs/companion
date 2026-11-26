//! Privacy-first network tools: `web_search` and `fetch_url`.

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::approval::ApprovalResponse;
use crate::tool::{
    ApprovalRequest, BoxFuture, Tool, ToolAuthorization, ToolContext, ToolPrepareContext, ToolRisk,
};
use crate::web_search::{
    execute_fetch_url, execute_web_search, inspect_fetch_url, inspect_search_query,
    validate_fetch_url,
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
pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn status_message(&self, ctx: &ToolContext<'_>) -> String {
        tr_f(
            "Searching {engine}…",
            &[("engine", &ctx.search_engine.label())],
        )
    }

    fn denied_message(&self) -> &'static str {
        "User denied web search."
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let proposed_query = serde_json::from_value::<WebSearchArgs>(args.clone())
            .map(|a| a.query.trim().to_string())
            .unwrap_or_default();
        Box::pin(async move {
            if !ctx.web_search_enabled {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Denied,
                    reason: "Web search is disabled for this chat.".to_string(),
                };
            }
            if proposed_query.is_empty() {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'query' argument".to_string(),
                };
            }
            let warnings = inspect_search_query(&proposed_query, ctx.local_texts);
            ToolAuthorization::RequiresApproval {
                request: ApprovalRequest::WebSearch {
                    engine_label: ctx.search_engine.label(),
                    proposed_query,
                    warnings,
                },
                ungranted_path: None,
                write_target_folder: None,
            }
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let query = match approval {
            ApprovalResponse::WebSearch(Some(q)) => q.trim().to_string(),
            _ => serde_json::from_value::<WebSearchArgs>(args.clone())
                .map(|a| a.query.trim().to_string())
                .unwrap_or_default(),
        };
        let enabled = ctx.web_search_enabled;
        let engine = ctx.search_engine;
        let searxng_url = ctx.searxng_url.map(str::to_string);
        Box::pin(async move {
            if !enabled {
                bail!("Web search is disabled for this chat.");
            }
            execute_web_search(engine, searxng_url.as_deref(), &query).await
        })
    }
}

/// Fetch readable text from a public `https://` URL with SSRF and privacy guards.
pub struct FetchUrlTool;

impl Tool for FetchUrlTool {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn status_message(&self, _ctx: &ToolContext<'_>) -> String {
        gettext("Fetching web page…")
    }

    fn denied_message(&self) -> &'static str {
        "User denied web page fetch."
    }

    fn prepare<'a>(
        &'a self,
        ctx: &'a ToolPrepareContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, ToolAuthorization> {
        let proposed_url = serde_json::from_value::<FetchUrlArgs>(args.clone())
            .map(|a| a.url.trim().to_string())
            .unwrap_or_default();
        Box::pin(async move {
            if !ctx.web_search_enabled {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Denied,
                    reason: "Web search is disabled for this chat.".to_string(),
                };
            }
            if proposed_url.is_empty() {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: "Missing 'url' argument".to_string(),
                };
            }
            if let Err(err) = validate_fetch_url(&proposed_url) {
                return ToolAuthorization::Rejected {
                    status: ToolCallStatus::Error,
                    reason: err.to_string(),
                };
            }
            let warnings = inspect_fetch_url(&proposed_url, ctx.local_texts);
            ToolAuthorization::RequiresApproval {
                request: ApprovalRequest::FetchUrl {
                    proposed_url,
                    warnings,
                },
                ungranted_path: None,
                write_target_folder: None,
            }
        })
    }

    fn execute_approved<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
        approval: &'a ApprovalResponse,
    ) -> BoxFuture<'a, Result<String>> {
        let url = match approval {
            ApprovalResponse::FetchUrl(Some(u)) => u.trim().to_string(),
            _ => serde_json::from_value::<FetchUrlArgs>(args.clone())
                .map(|a| a.url.trim().to_string())
                .unwrap_or_default(),
        };
        let enabled = ctx.web_search_enabled;
        Box::pin(async move {
            if !enabled {
                bail!("Web search is disabled for this chat.");
            }
            execute_fetch_url(&url).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionEngine;
    use companion_core::types::SearchEngine;
    use serde_json::json;

    #[tokio::test]
    async fn fetch_url_prepare_rejects_disabled_empty_and_unsafe_urls() {
        let perms = PermissionEngine::new();
        let tool = FetchUrlTool;

        let disabled_ctx = ToolPrepareContext {
            conv_id: "c1",
            granted_folders: &[],
            permissions: &perms,
            web_search_enabled: false,
            search_engine: SearchEngine::default(),
            searxng_url: None,
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
            web_search_enabled: true,
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
