//! Model-facing self-learning tools.

use crate::learn::LearnStore;
use async_trait::async_trait;
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register_learn_tools(registry: &ToolRegistry, learn: Arc<LearnStore>) {
    registry.register(Arc::new(LearnRecallTool {
        learn: learn.clone(),
    }));
    registry.register(Arc::new(LearnWeightsTool { learn }));
}

struct LearnRecallTool {
    learn: Arc<LearnStore>,
}

#[async_trait]
impl ToolHandler for LearnRecallTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "learn_recall",
            "Recall prior successful/failed skill/plugin/tool episodes for a query (self-learning memory).",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["query"]
            }),
        )
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("query required".into()))?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let episodes = self.learn.recall(query, limit);
        serde_json::to_string_pretty(&episodes).map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct LearnWeightsTool {
    learn: Arc<LearnStore>,
}

#[async_trait]
impl ToolHandler for LearnWeightsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "learn_weights",
            "Show current self-learning routing weights for skills/plugins/tools.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let mut weights: Vec<(String, f32)> = self.learn.weights().into_iter().collect();
        weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        serde_json::to_string_pretty(&weights).map_err(|e| ToolError::Message(e.to_string()))
    }
}
