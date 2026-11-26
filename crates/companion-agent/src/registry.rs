//! Extensible registry of built-in and MCP agent tools.

use std::collections::HashMap;
use std::sync::Arc;

use crate::tool::{Tool, ToolRisk};
use crate::tools::{
    CreateDirectoryTool, DeleteDirectoryTool, DeleteFileTool, FetchUrlTool, ListFilesTool,
    MovePathTool, ReadFileTool, SearchFilesTool, WebSearchTool, WriteFileTool,
};

/// Registry mapping tool names to [`Tool`] implementations in deterministic registration order.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    order: Vec<String>,
}

impl ToolRegistry {
    /// Construct a registry pre-populated with all built-in workspace and web tools.
    pub fn with_builtins(web_search: Arc<crate::web_search::WebSearchService>) -> Self {
        let mut reg = Self::default();
        reg.register(Arc::new(ListFilesTool));
        reg.register(Arc::new(ReadFileTool));
        reg.register(Arc::new(SearchFilesTool));
        reg.register(Arc::new(WriteFileTool));
        reg.register(Arc::new(CreateDirectoryTool));
        reg.register(Arc::new(MovePathTool));
        reg.register(Arc::new(DeleteFileTool));
        reg.register(Arc::new(DeleteDirectoryTool));
        reg.register(Arc::new(WebSearchTool::new(web_search.clone())));
        reg.register(Arc::new(FetchUrlTool::new(web_search)));
        reg
    }

    /// Register (or replace) a tool implementation.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Iterate over registered tools in deterministic registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.order.iter().filter_map(|name| self.tools.get(name))
    }

    /// Return the human-readable display name for `name`, falling back to `name`.
    pub fn display_name_of(&self, name: &str) -> String {
        self.tools
            .get(name)
            .map(|t| t.metadata().display_name)
            .unwrap_or_else(|| name.to_string())
    }

    /// Render the workspace tools section for the system prompt from registered non-network tools.
    pub fn render_workspace_instructions(&self) -> String {
        let mut lines = String::from(
            "You have tool access to local files and folders. To invoke a tool, output a <tool_call> block with valid JSON:\n\
             <tool_call>{\"name\": \"tool_name\", \"arguments\": {...}}</tool_call>\n\n\
             Available tools:\n",
        );
        for tool in self.iter() {
            if tool.risk() == ToolRisk::Network {
                continue;
            }
            let meta = tool.metadata();
            lines.push_str(&format!(
                "- {}: {} — {}\n",
                tool.name(),
                meta.parameter_example,
                meta.prompt_summary
            ));
        }
        lines.push_str(
            "\nGuidelines:\n\
             - CRITICAL: Whenever an action requires a tool, you MUST output the <tool_call>...</tool_call> block in your response. Never claim you are performing, deleting, creating, or checking something (e.g. \"I'm deleting it now!\") without including the <tool_call> block—prose alone does not execute anything.\n\
             - Output <tool_call> directly when you need a tool—do not narrate that you are about to call a tool, and do not thank the user for tool results.\n\
             - When asked to read, inspect, or search a specific local file or folder path—even if it is not in the granted list yet—call read_file, list_files, or search_files with that exact absolute path. Companion will prompt the user for permission if needed.\n\
             - When asked to inspect a folder, list its files first and then read relevant text/Markdown/code files automatically before giving your final answer.\n\
             - When the user mentions a path with @/path/..., use read_file or list_files on that exact path if you need its contents.\n\
             - Clearly note any image or binary files found without attempting to read them with read_file.\n\
             - Directory/file write tools (write_file, delete_file, create_directory, move_path, delete_directory) are blocked on [read-only] items unless the user grants Read & Write access.",
        );
        lines
    }

    /// Render the web tools section for the system prompt from registered network tools.
    pub fn render_web_instructions(&self) -> String {
        let mut lines = String::from("Web Search is enabled for this chat:\n");
        for tool in self.iter() {
            if tool.risk() != ToolRisk::Network {
                continue;
            }
            let meta = tool.metadata();
            lines.push_str(&format!(
                "- {}: {} — {}\n",
                tool.name(),
                meta.parameter_example,
                meta.prompt_summary
            ));
        }
        lines.trim_end().to_string()
    }

    #[cfg(test)]
    pub fn risk_of(&self, name: &str) -> Option<ToolRisk> {
        self.tools.get(name).map(|t| t.risk())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalResponse;
    use crate::tool::{
        BoxFuture, PreparedToolCall, ToolAuthorization, ToolContext, ToolMetadata,
        ToolPrepareContext,
    };
    use serde_json::Value;

    struct DynamicTool {
        name: String,
    }

    impl Tool for DynamicTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn risk(&self) -> ToolRisk {
            ToolRisk::ReadOnly
        }

        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                display_name: "Dynamic MCP Tool".to_string(),
                description: "Custom MCP tool.".to_string(),
                prompt_summary: "Run dynamic MCP action.",
                parameter_example: r#"{"arg": "val"}"#,
                read_policy: "Allow",
                rw_policy: "Allow",
            }
        }

        fn prepare<'a>(
            &'a self,
            _ctx: &'a ToolPrepareContext<'a>,
            _args: &'a Value,
        ) -> BoxFuture<'a, ToolAuthorization> {
            Box::pin(async { ToolAuthorization::AutoAllowed(PreparedToolCall::new(())) })
        }

        fn execute_approved<'a>(
            &'a self,
            _ctx: &'a ToolContext<'a>,
            _prepared: PreparedToolCall,
            _approval: &'a ApprovalResponse,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            Box::pin(async { Ok("ok".to_string()) })
        }
    }

    #[test]
    fn registry_generates_instructions_and_supports_dynamic_tools() {
        let mut reg = ToolRegistry::with_builtins(Arc::new(
            crate::web_search::WebSearchService::new().expect("web search"),
        ));
        reg.register(Arc::new(DynamicTool {
            name: "mcp__github__list_issues".to_string(),
        }));

        assert_eq!(reg.display_name_of("read_file"), "Read File");
        assert_eq!(
            reg.display_name_of("mcp__github__list_issues"),
            "Dynamic MCP Tool"
        );
        assert_eq!(reg.display_name_of("unknown_tool"), "unknown_tool");

        let ws = reg.render_workspace_instructions();
        assert!(ws.contains("- read_file:"));
        assert!(
            ws.contains("- mcp__github__list_issues: {\"arg\": \"val\"} — Run dynamic MCP action.")
        );
        assert!(!ws.contains("- web_search:"));

        let web = reg.render_web_instructions();
        assert!(web.contains("- web_search:"));
        assert!(web.contains("- fetch_url:"));
    }
}
