use crate::registry::PluginRegistry;
use crate::router::{prompt_topk_section, rank_plugins, ranked_to_json};
use async_trait::async_trait;
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub type LearnWeightProvider = Arc<dyn Fn() -> HashMap<String, f32> + Send + Sync>;

/// Register without personalized routing; runtime hosts can provide gated
/// weights without introducing a dependency on dsh-core.
pub fn register_plugin_tools(registry: &ToolRegistry, plugins: Arc<PluginRegistry>) {
    register_plugin_tools_with_weights(registry, plugins, Arc::new(HashMap::new));
}

pub fn register_plugin_tools_with_weights(
    registry: &ToolRegistry,
    plugins: Arc<PluginRegistry>,
    weights: LearnWeightProvider,
) {
    registry.register(Arc::new(PluginListTool {
        plugins: plugins.clone(),
    }));
    registry.register(Arc::new(PluginSearchTool {
        plugins: plugins.clone(),
        weights,
    }));
    registry.register(Arc::new(PluginInstallTool {
        plugins: plugins.clone(),
    }));
    registry.register(Arc::new(PluginReloadTool {
        plugins: plugins.clone(),
    }));
    registry.register(Arc::new(PluginUnloadTool { plugins }));
}

struct PluginListTool {
    plugins: Arc<PluginRegistry>,
}

#[async_trait]
impl ToolHandler for PluginListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "plugin_list",
            "List mounted outer-layer plugins with tags and examples. Prefer plugin_search for task routing.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        serde_json::to_string_pretty(&self.plugins.list())
            .map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct PluginSearchTool {
    plugins: Arc<PluginRegistry>,
    weights: LearnWeightProvider,
}

#[async_trait]
impl ToolHandler for PluginSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "plugin_search",
            "Rank outer plugins by relevance to a query (tags/tools/learned weights). Then call plugin.<id>.<tool>.",
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
        let weights = (self.weights)();
        let ranked = rank_plugins(&self.plugins, query, &weights, limit);
        let brief = prompt_topk_section(&ranked);
        let json = ranked_to_json(&ranked);
        Ok(format!(
            "{brief}\n\n{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        ))
    }
}

struct PluginInstallTool {
    plugins: Arc<PluginRegistry>,
}

#[async_trait]
impl ToolHandler for PluginInstallTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "plugin_install",
            "Install an outer plugin from a local directory into the outer plugins home (never into core).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Source plugin directory" },
                    "dest": {
                        "type": "string",
                        "description": "Optional destination plugins root; defaults to outer_home/plugins"
                    }
                },
                "required": ["path"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("path required".into()))?;
        let src = resolve(ctx, path);
        let dest = if let Some(d) = args.get("dest").and_then(|v| v.as_str()) {
            resolve(ctx, d)
        } else {
            ctx.outer_home.join("plugins")
        };
        let dest_str = dest.to_string_lossy().replace('\\', "/");
        let outer = ctx.outer_home.to_string_lossy().replace('\\', "/");
        let ws_outer = ctx.workspace_outer.to_string_lossy().replace('\\', "/");
        if !(dest_str.starts_with(&outer) || dest_str.starts_with(&ws_outer)) {
            return Err(ToolError::Message(
                "plugin_install dest must be under outer_home or workspace outer".into(),
            ));
        }
        let id = self
            .plugins
            .install_from_path(&src, &dest)
            .map_err(|e| ToolError::Message(e.to_string()))?;
        Ok(format!(
            "installed and mounted plugin `{id}` into {}. Tools: plugin.{id}.*",
            dest.display()
        ))
    }
}

struct PluginReloadTool {
    plugins: Arc<PluginRegistry>,
}

#[async_trait]
impl ToolHandler for PluginReloadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "plugin_reload",
            "Reload all outer plugins from outer_home/plugins and workspace outer/plugins.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let roots = vec![
            ctx.outer_home.join("plugins"),
            ctx.workspace_outer.join("plugins"),
        ];
        self.plugins.reload_all(&roots);
        Ok(format!(
            "reloaded plugins: {}",
            serde_json::to_string(&self.plugins.list()).unwrap_or_default()
        ))
    }
}

struct PluginUnloadTool {
    plugins: Arc<PluginRegistry>,
}

#[async_trait]
impl ToolHandler for PluginUnloadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "plugin_unload",
            "Unload an outer plugin by id (reversible; core remains intact).",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }),
        )
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("id required".into()))?;
        if self.plugins.unload(id) {
            Ok(format!("unloaded plugin `{id}`"))
        } else {
            Err(ToolError::Message(format!("plugin not found: {id}")))
        }
    }
}

fn resolve(ctx: &ToolContext, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        ctx.cwd.join(p)
    }
}
