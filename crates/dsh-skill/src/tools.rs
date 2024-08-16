use crate::catalog::SkillCatalog;
use crate::router::{load_learn_weights, prompt_topk_section, rank_skills};
use async_trait::async_trait;
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register_skill_tools(registry: &ToolRegistry, catalog: Arc<SkillCatalog>) {
    registry.register(Arc::new(SkillListTool {
        catalog: catalog.clone(),
    }));
    registry.register(Arc::new(SkillSearchTool {
        catalog: catalog.clone(),
    }));
    registry.register(Arc::new(SkillLoadTool {
        catalog: catalog.clone(),
    }));
    registry.register(Arc::new(SkillRecommendTool { catalog }));
}

struct SkillListTool {
    catalog: Arc<SkillCatalog>,
}

#[async_trait]
impl ToolHandler for SkillListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "skill_list",
            "List all discovered skills (OpenAI SKILL.md + DeepSeek/dsh) with tags and examples. Prefer skill_search for task-specific routing.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let list = self.catalog.list();
        serde_json::to_string_pretty(&list).map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct SkillSearchTool {
    catalog: Arc<SkillCatalog>,
}

#[async_trait]
impl ToolHandler for SkillSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "skill_search",
            "Rank skills by relevance to a query using tags/examples/learned weights. Use before skill_load.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20 }
                },
                "required": ["query"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("query required".into()))?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let weights = load_learn_weights(&ctx.outer_home.join("meta"));
        let ranked = rank_skills(&self.catalog, query, &weights, limit);
        let payload: Vec<Value> = ranked
            .iter()
            .map(|r| {
                json!({
                    "name": r.summary.name,
                    "description": r.summary.description,
                    "score": r.score,
                    "tags": r.summary.tags,
                    "examples": r.summary.examples,
                    "reasons": r.reasons,
                    "source": r.summary.source,
                    "resources": {
                        "scripts": r.summary.resources.scripts,
                        "references": r.summary.resources.references,
                        "assets": r.summary.resources.assets,
                    }
                })
            })
            .collect();
        Ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
    }
}

struct SkillRecommendTool {
    catalog: Arc<SkillCatalog>,
}

#[async_trait]
impl ToolHandler for SkillRecommendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "skill_recommend",
            "Return a compact routing brief (top skills) for the current user intent. Faster than listing everything.",
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

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("query required".into()))?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
        let weights = load_learn_weights(&ctx.outer_home.join("meta"));
        let ranked = rank_skills(&self.catalog, query, &weights, limit);
        Ok(prompt_topk_section(&ranked))
    }
}

struct SkillLoadTool {
    catalog: Arc<SkillCatalog>,
}

#[async_trait]
impl ToolHandler for SkillLoadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "skill_load",
            "Load the full body of a skill by name (progressive disclosure). Call after skill_search/skill_recommend.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        )
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("name required".into()))?;
        let Some(record) = self.catalog.get(name) else {
            let suggestions = self.catalog.list();
            let names: Vec<_> = suggestions.into_iter().map(|s| s.name).take(8).collect();
            return Err(ToolError::Message(format!(
                "skill not found: {name}. known=[{}]",
                names.join(", ")
            )));
        };
        let resources = if record.resources.is_empty() {
            String::new()
        } else {
            format!(
                "\nResources:\n- scripts: {}\n- references: {}\n- assets: {}\n",
                record.resources.scripts.join(", "),
                record.resources.references.join(", "),
                record.resources.assets.join(", ")
            )
        };
        Ok(format!(
            "# {}\n\n{}\n\n---\nSource: {:?}\nFormat: {:?}\nTags: {}\nExamples:\n{}{resources}\nInvocation: follow these instructions; use plugin.* tools only when this skill requires them. Prefer read_file for references/ before shell.",
            record.summary.name,
            record.body,
            record.summary.source,
            record.format,
            record.summary.tags.join(", "),
            record
                .summary
                .examples
                .iter()
                .map(|e| format!("- {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}
