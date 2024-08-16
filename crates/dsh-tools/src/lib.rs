//! Tool registry and guarded execution pipeline.

mod registry;
mod types;

pub use registry::ToolRegistry;
pub use types::{
    ToolCall, ToolContext, ToolDefinition, ToolError, ToolHandler, ToolResult, ToolSchema,
};

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Executes a single tool call through pre/execute/post hooks.
#[async_trait]
pub trait ToolPipeline: Send + Sync {
    async fn execute(&self, call: &ToolCall, ctx: &ToolContext) -> ToolResult;
}

pub struct DefaultPipeline {
    registry: Arc<ToolRegistry>,
}

impl DefaultPipeline {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolPipeline for DefaultPipeline {
    async fn execute(&self, call: &ToolCall, ctx: &ToolContext) -> ToolResult {
        let Some(handler) = self.registry.get(&call.name) else {
            return ToolResult::error(format!("unknown tool: {}", call.name));
        };
        match handler.call(call.arguments.clone(), ctx).await {
            Ok(value) => ToolResult::ok(value),
            Err(err) => ToolResult::error(err.to_string()),
        }
    }
}

/// Helper to build OpenAI-style tool schema JSON.
pub fn openai_tool_schema(def: &ToolDefinition) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": def.name,
            "description": def.description,
            "parameters": def.parameters,
        }
    })
}
