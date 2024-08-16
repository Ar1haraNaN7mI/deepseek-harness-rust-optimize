//! Plugin relevance routing and invocation helpers.

use crate::registry::{PluginRegistry, RoutingSummary};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RankedPlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
    pub score: f32,
    pub reasons: Vec<String>,
}

pub fn rank_plugins(
    registry: &PluginRegistry,
    query: &str,
    learn_weights: &HashMap<String, f32>,
    limit: usize,
) -> Vec<RankedPlugin> {
    let q = normalize(query);
    let tokens = tokenize(&q);
    let mut ranked = Vec::new();

    for item in registry.routing_summaries() {
        score_one(item, &q, &tokens, learn_weights, &mut ranked);
    }

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit.max(1));
    ranked
}

fn score_one(
    item: RoutingSummary,
    q: &str,
    tokens: &[String],
    learn_weights: &HashMap<String, f32>,
    ranked: &mut Vec<RankedPlugin>,
) {
    let id = item.id;
    if id.is_empty() {
        return;
    }
    let name = item.name;
    let description = item.description;
    let tags = item.tags;
    let tools = item.tools;

    let mut score = 0.0_f32;
    let mut reasons = Vec::new();
    let blob = normalize(&format!("{id} {name} {description} {}", tags.join(" ")));

    if blob.contains(&normalize(&id)) && q.contains(&normalize(&id)) {
        score += 6.0;
        reasons.push("id-match".into());
    }
    for t in tokens {
        if blob.contains(t) {
            score += 1.8;
        }
        for tag in &tags {
            if normalize(tag).contains(t) {
                score += 2.5;
                reasons.push(format!("tag:{tag}"));
            }
        }
        for tool in &tools {
            if normalize(tool).contains(t) {
                score += 2.0;
                reasons.push(format!("tool:{tool}"));
            }
        }
    }

    let key = format!("plugin:{id}");
    if let Some(w) = learn_weights.get(&key) {
        score += w * 2.0;
        if *w > 0.1 {
            reasons.push(format!("learned:+{w:.2}"));
        }
    }

    if score > 0.0 {
        ranked.push(RankedPlugin {
            id,
            name,
            description,
            tools,
            tags,
            score,
            reasons,
        });
    }
}

pub fn prompt_topk_section(ranked: &[RankedPlugin]) -> String {
    if ranked.is_empty() {
        return "No strongly relevant plugins. Use plugin_search / plugin_list.".into();
    }
    let mut out = String::from("Relevant outer plugins (call as plugin.<id>.<tool>):\n");
    for r in ranked {
        out.push_str(&format!(
            "- {} ({}) score={:.1} — {} tools=[{}] tags=[{}]\n",
            r.id,
            r.name,
            r.score,
            r.description,
            r.tools.join(", "),
            r.tags.join(", ")
        ));
    }
    out.push_str(
        "Prefer skills for procedures; call plugin tools for concrete side-effects in the outer layer.",
    );
    out
}

pub fn ranked_to_json(ranked: &[RankedPlugin]) -> Value {
    Value::Array(
        ranked
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "name": r.name,
                    "description": r.description,
                    "tools": r.tools,
                    "tags": r.tags,
                    "score": r.score,
                    "reasons": r.reasons,
                })
            })
            .collect(),
    )
}

pub fn load_learn_weights(meta_dir: &std::path::Path) -> HashMap<String, f32> {
    let path = meta_dir.join("learn-weights.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokenize(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect()
}
