//! Extensible registry of built-in and MCP agent tools.

use std::collections::HashMap;
use std::sync::Arc;

use crate::tool::Tool;
#[cfg(test)]
use crate::tool::ToolRisk;
use crate::tools::{
    CreateDirectoryTool, DeleteDirectoryTool, DeleteFileTool, FetchUrlTool, ListFilesTool,
    MovePathTool, ReadFileTool, SearchFilesTool, WebSearchTool, WriteFileTool,
};

/// Registry mapping tool names to [`Tool`] implementations.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Construct a registry pre-populated with all built-in workspace and web tools.
    pub fn with_builtins() -> Self {
        let mut reg = Self::default();
        reg.register(Arc::new(ListFilesTool));
        reg.register(Arc::new(ReadFileTool));
        reg.register(Arc::new(SearchFilesTool));
        reg.register(Arc::new(WriteFileTool));
        reg.register(Arc::new(CreateDirectoryTool));
        reg.register(Arc::new(MovePathTool));
        reg.register(Arc::new(DeleteFileTool));
        reg.register(Arc::new(DeleteDirectoryTool));
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

    #[cfg(test)]
    pub fn risk_of(&self, name: &str) -> Option<ToolRisk> {
        self.tools.get(name).map(|t| t.risk())
    }
}
