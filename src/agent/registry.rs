//! Extensible registry of built-in and MCP agent tools.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::bail;
use serde_json::Value;

use crate::agent::tool::{BoxFuture, Tool, ToolContext, ToolRisk};
use crate::agent::tools::{
    GrantedFolder, describe_tool_action, execute_tool_with_grants, extract_raw_target_path,
    first_str_arg, is_write_already_granted,
};
use crate::agent::web_search::{execute_fetch_url, execute_web_search};

/// Registry mapping tool names to [`Tool`] implementations.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Construct a registry pre-populated with all built-in workspace and web tools.
    pub fn with_builtins() -> Self {
        let mut reg = Self::default();
        for (name, risk) in [
            ("list_files", ToolRisk::ReadOnly),
            ("read_file", ToolRisk::ReadOnly),
            ("search_files", ToolRisk::ReadOnly),
            ("write_file", ToolRisk::WriteWorkspace),
            ("create_directory", ToolRisk::WriteWorkspace),
            ("move_path", ToolRisk::WriteWorkspace),
            ("delete_file", ToolRisk::Destructive),
            ("delete_directory", ToolRisk::Destructive),
        ] {
            reg.register(Arc::new(BuiltinFileTool { name, risk }));
        }
        reg.register(Arc::new(WebSearchTool));
        reg.register(Arc::new(FetchUrlTool));
        reg
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn risk_of(&self, name: &str) -> Option<ToolRisk> {
        self.tools.get(name).map(|t| t.risk())
    }
}

struct BuiltinFileTool {
    name: &'static str,
    risk: ToolRisk,
}

impl Tool for BuiltinFileTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn risk(&self) -> ToolRisk {
        self.risk
    }

    fn describe(&self, ctx: &ToolContext<'_>, args: &Value) -> String {
        describe_tool_action(ctx.granted_folders, self.name, args)
    }

    fn raw_target_path<'a>(&self, args: &'a Value) -> Option<&'a str> {
        extract_raw_target_path(self.name, args)
    }

    fn is_write_already_granted(&self, granted_folders: &[GrantedFolder], args: &Value) -> bool {
        is_write_already_granted(granted_folders, self.name, args)
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        let grants = ctx.granted_folders.to_vec();
        let name = self.name;
        let args = args.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || execute_tool_with_grants(&grants, name, &args))
                .await
                .map_err(|e| anyhow::anyhow!("Tool task failed: {e}"))?
        })
    }
}

struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn describe(&self, ctx: &ToolContext<'_>, args: &Value) -> String {
        let q = first_str_arg(args, &["query", "q"]).unwrap_or("");
        format!("Search {} for:\n\n{q}", ctx.search_engine.label())
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        let query = first_str_arg(args, &["query", "q"])
            .unwrap_or("")
            .trim()
            .to_string();
        let engine = ctx.search_engine;
        let searxng = ctx.searxng_url.map(str::to_string);
        let enabled = ctx.web_search_enabled;
        Box::pin(async move {
            if !enabled {
                bail!("Web search is disabled for this chat.");
            }
            execute_web_search(engine, searxng.as_deref(), &query).await
        })
    }
}

struct FetchUrlTool;

impl Tool for FetchUrlTool {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Network
    }

    fn describe(&self, _ctx: &ToolContext<'_>, args: &Value) -> String {
        let url = first_str_arg(args, &["url"]).unwrap_or("");
        format!("Fetch web page:\n\n{url}")
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext<'a>,
        args: &'a Value,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        let url = first_str_arg(args, &["url"]).unwrap_or("").trim().to_string();
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
    use crate::core::types::SearchEngine;
    use serde_json::json;

    #[test]
    fn builtins_registry_assigns_expected_risks_and_gates_network_tools() {
        let reg = ToolRegistry::with_builtins();

        assert_eq!(reg.risk_of("list_files"), Some(ToolRisk::ReadOnly));
        assert_eq!(reg.risk_of("read_file"), Some(ToolRisk::ReadOnly));
        assert_eq!(reg.risk_of("search_files"), Some(ToolRisk::ReadOnly));
        assert_eq!(reg.risk_of("write_file"), Some(ToolRisk::WriteWorkspace));
        assert_eq!(
            reg.risk_of("create_directory"),
            Some(ToolRisk::WriteWorkspace)
        );
        assert_eq!(reg.risk_of("move_path"), Some(ToolRisk::WriteWorkspace));
        assert_eq!(reg.risk_of("delete_file"), Some(ToolRisk::Destructive));
        assert_eq!(reg.risk_of("delete_directory"), Some(ToolRisk::Destructive));
        assert_eq!(reg.risk_of("web_search"), Some(ToolRisk::Network));
        assert_eq!(reg.risk_of("fetch_url"), Some(ToolRisk::Network));
        assert_eq!(reg.risk_of("unknown_tool"), None);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ctx = ToolContext {
            granted_folders: &[],
            web_search_enabled: false,
            search_engine: SearchEngine::DuckDuckGo,
            searxng_url: None,
            local_texts: &[],
        };
        let ws_err = rt
            .block_on(
                reg.get("web_search")
                    .unwrap()
                    .execute(&ctx, &json!({ "query": "rust" })),
            )
            .unwrap_err();
        assert!(ws_err.to_string().contains("disabled"));

        let fu_err = rt
            .block_on(
                reg.get("fetch_url")
                    .unwrap()
                    .execute(&ctx, &json!({ "url": "https://example.com" })),
            )
            .unwrap_err();
        assert!(fu_err.to_string().contains("disabled"));
    }
}
