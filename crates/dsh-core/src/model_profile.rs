//! Model-size aware execution policy.
//!
//! Models below roughly 70B parameters usually benefit more from a smaller,
//! explicit action space than from sending every registered tool and the full
//! session transcript on every step.  This module keeps that policy in the
//! harness instead of making every provider or tool implement its own guess.

use dsh_tools::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::str::FromStr;

/// The cutoff used by the automatic policy.  A model reported as exactly
/// 70B is intentionally treated as standard/large; the optimized branch is
/// strictly for values below this threshold.
pub const SMALL_MODEL_THRESHOLD_B: f32 = 70.0;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelOptimizationMode {
    /// Select the small-model policy only when the model can be identified as
    /// being below [`SMALL_MODEL_THRESHOLD_B`].
    #[default]
    Auto,
    /// Always use the small-model policy (useful for local quantized models
    /// whose parameter count is not encoded in the model id).
    Small,
    /// Keep the full/default request shape even for a model id containing a
    /// small-model hint.
    Standard,
    /// Alias for standard behavior that makes intent explicit in config.
    Off,
}

impl ModelOptimizationMode {
    pub fn is_small(self, detected_small: bool) -> bool {
        match self {
            Self::Auto => detected_small,
            Self::Small => true,
            Self::Standard | Self::Off => false,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Small => "small",
            Self::Standard => "standard",
            Self::Off => "off",
        }
    }
}

impl FromStr for ModelOptimizationMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "small" | "compact" => Ok(Self::Small),
            "standard" | "large" => Ok(Self::Standard),
            "off" | "disabled" => Ok(Self::Off),
            other => Err(format!(
                "unknown model optimization mode `{other}` (expected auto|small|standard|off)"
            )),
        }
    }
}

/// Tunables for the small-model path.  All fields have conservative defaults
/// and are optional in TOML, so older config files remain valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOptimizationConfig {
    #[serde(default)]
    pub mode: ModelOptimizationMode,
    /// Explicit parameter count in billions.  This takes precedence over
    /// model-id inference and is the reliable way to configure aliases.
    #[serde(default)]
    pub parameter_count_b: Option<f32>,
    #[serde(default = "default_small_max_steps")]
    pub small_max_steps: usize,
    #[serde(default = "default_small_tool_budget")]
    pub small_tool_budget: usize,
    #[serde(default = "default_small_context_messages")]
    pub small_context_messages: usize,
    #[serde(default = "default_small_context_chars")]
    pub small_context_chars: usize,
    #[serde(default = "default_small_tool_result_max_chars")]
    pub small_tool_result_max_chars: usize,
    #[serde(default = "default_small_max_tokens")]
    pub small_max_tokens: u32,
    #[serde(default = "default_small_temperature")]
    pub small_temperature: f32,
    #[serde(default = "default_small_thinking")]
    pub small_thinking: bool,
    #[serde(default = "default_small_compact_results")]
    pub compact_tool_results: bool,
    /// Maximum serialized size of one model-facing JSON schema. The full
    /// schema remains in the registry for strict preflight validation.
    #[serde(default = "default_small_tool_schema_chars")]
    pub small_tool_schema_chars: usize,
    /// Remove prose-only JSON Schema keywords before sending tools to a small
    /// model. This preserves types, required fields and validation bounds.
    #[serde(default = "default_small_compact_schemas")]
    pub compact_tool_schemas: bool,
    /// Whether the small-model request may emit multiple tool calls at once.
    /// Sequential calls are more reliable for quantized/local models.
    #[serde(default = "default_small_parallel_tool_calls")]
    pub small_parallel_tool_calls: bool,
}

fn default_small_max_steps() -> usize {
    12
}

fn default_small_tool_budget() -> usize {
    14
}

fn default_small_context_messages() -> usize {
    24
}

fn default_small_context_chars() -> usize {
    48_000
}

fn default_small_tool_result_max_chars() -> usize {
    8_000
}

fn default_small_max_tokens() -> u32 {
    4_096
}

fn default_small_temperature() -> f32 {
    0.1
}

fn default_small_thinking() -> bool {
    false
}

fn default_small_compact_results() -> bool {
    true
}

fn default_small_tool_schema_chars() -> usize {
    2_400
}

fn default_small_compact_schemas() -> bool {
    true
}

fn default_small_parallel_tool_calls() -> bool {
    false
}

impl Default for ModelOptimizationConfig {
    fn default() -> Self {
        Self {
            mode: ModelOptimizationMode::Auto,
            parameter_count_b: None,
            small_max_steps: default_small_max_steps(),
            small_tool_budget: default_small_tool_budget(),
            small_context_messages: default_small_context_messages(),
            small_context_chars: default_small_context_chars(),
            small_tool_result_max_chars: default_small_tool_result_max_chars(),
            small_max_tokens: default_small_max_tokens(),
            small_temperature: default_small_temperature(),
            small_thinking: default_small_thinking(),
            compact_tool_results: default_small_compact_results(),
            small_tool_schema_chars: default_small_tool_schema_chars(),
            compact_tool_schemas: default_small_compact_schemas(),
            small_parallel_tool_calls: default_small_parallel_tool_calls(),
        }
    }
}

/// A normalized view of the active model and the policy selected for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelProfile {
    pub model: String,
    pub parameter_count_b: Option<f32>,
    pub detected_small: bool,
    pub optimization_mode: ModelOptimizationMode,
    pub small_model: bool,
    pub policy: ModelPolicy,
}

/// Effective request/agent budgets.  `None` means use the regular application
/// setting.  Keeping the policy as data makes it easy for frontends and
/// diagnostics to show exactly why a request was changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPolicy {
    pub max_steps: Option<usize>,
    pub tool_budget: Option<usize>,
    pub context_messages: Option<usize>,
    pub context_chars: Option<usize>,
    pub tool_result_max_chars: Option<usize>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub thinking: Option<bool>,
    pub compact_tool_results: bool,
    pub tool_schema_chars: Option<usize>,
    pub compact_tool_schemas: bool,
    pub parallel_tool_calls: Option<bool>,
}

impl ModelPolicy {
    pub fn standard() -> Self {
        Self {
            max_steps: None,
            tool_budget: None,
            context_messages: None,
            context_chars: None,
            tool_result_max_chars: None,
            max_tokens: None,
            temperature: None,
            thinking: None,
            compact_tool_results: false,
            tool_schema_chars: None,
            compact_tool_schemas: false,
            parallel_tool_calls: None,
        }
    }

    fn small(config: &ModelOptimizationConfig) -> Self {
        let temperature = if config.small_temperature.is_finite() {
            config.small_temperature.clamp(0.0, 2.0)
        } else {
            default_small_temperature()
        };
        Self {
            max_steps: Some(config.small_max_steps.max(1)),
            tool_budget: Some(config.small_tool_budget.max(1)),
            context_messages: Some(config.small_context_messages.max(4)),
            context_chars: Some(config.small_context_chars.max(2_048)),
            tool_result_max_chars: Some(config.small_tool_result_max_chars.max(512)),
            max_tokens: Some(config.small_max_tokens.max(256)),
            temperature: Some(temperature),
            thinking: Some(config.small_thinking),
            compact_tool_results: config.compact_tool_results,
            tool_schema_chars: Some(config.small_tool_schema_chars.max(256)),
            compact_tool_schemas: config.compact_tool_schemas,
            parallel_tool_calls: Some(config.small_parallel_tool_calls),
        }
    }

    pub fn is_small(&self) -> bool {
        self.max_steps.is_some()
    }
}

impl ModelProfile {
    pub fn for_model(model: impl Into<String>, config: &ModelOptimizationConfig) -> Self {
        let model = model.into();
        let inferred = infer_parameter_count_b(&model);
        let explicit = config
            .parameter_count_b
            .filter(|count| count.is_finite() && *count > 0.0);
        let parameter_count_b = explicit.or(inferred);
        let detected_small = parameter_count_b
            .map(|count| count.is_finite() && count > 0.0 && count < SMALL_MODEL_THRESHOLD_B)
            .unwrap_or_else(|| has_small_model_hint(&model));
        let small_model = config.mode.is_small(detected_small);
        let policy = if small_model {
            ModelPolicy::small(config)
        } else {
            ModelPolicy::standard()
        };
        Self {
            model,
            parameter_count_b,
            detected_small,
            optimization_mode: config.mode,
            small_model,
            policy,
        }
    }

    pub fn summary(&self) -> String {
        let size = self
            .parameter_count_b
            .map(|v| format!("{v:.1}B"))
            .unwrap_or_else(|| "unknown".into());
        format!(
            "model={} size={} mode={} branch={}",
            self.model,
            size,
            self.optimization_mode.label(),
            if self.small_model {
                "small"
            } else {
                "standard"
            }
        )
    }

    /// Compact, imperative guidance that is injected only for small models.
    pub fn prompt_section(&self) -> String {
        if !self.small_model {
            return String::new();
        }
        let p = &self.policy;
        format!(
            "Active model is below 70B; use the compact execution policy.\n\
             - Make a short plan (at most 3 bullets) before acting.\n\
             - Use one tool at a time and inspect returned data before the next action.\n\
             - Prefer grep/glob/read_file and the progressive skill/plugin search tools; do not list every resource.\n\
             - Never invent file contents or tool arguments; use the smallest valid JSON object.\n\
             - After edits, run a focused check and report the concrete result.\n\
             Budgets: max_steps={}, tool_schema_budget={}, schema_chars_per_tool={}, context_messages={}, context_chars={}, tool_result_chars={}, max_output_tokens={}, thinking={}, parallel_tools={}",
            p.max_steps.unwrap_or(0),
            p.tool_budget.unwrap_or(0),
            p.tool_schema_chars.unwrap_or(0),
            p.context_messages.unwrap_or(0),
            p.context_chars.unwrap_or(0),
            p.tool_result_max_chars.unwrap_or(0),
            p.max_tokens.unwrap_or(0),
            if p.thinking.unwrap_or(false) { "enabled" } else { "disabled" },
            if p.parallel_tool_calls.unwrap_or(true) { "enabled" } else { "disabled" },
        )
    }
}

/// Infer a parameter count from common model-id forms (`32b`, `8x7b`).
/// Returns `None` when the id carries no trustworthy size marker.
pub fn infer_parameter_count_b(model: &str) -> Option<f32> {
    // Published model identifiers are ASCII. Avoid slicing through a UTF-8
    // code point if a provider returns a localized display name.
    if !model.is_ascii() {
        return None;
    }
    let lower = model.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut candidates = Vec::new();
    for end in 0..bytes.len() {
        if bytes[end] != b'b' {
            continue;
        }
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        let Ok(value) = lower[start..end].parse::<f32>() else {
            continue;
        };
        if value <= 0.0 || !value.is_finite() {
            continue;
        }
        // MoE ids such as mixtral-8x7b expose active experts as 8x7b.  The
        // total parameter count is a better branch signal than 7B alone.
        if start > 1 && bytes[start - 1] == b'x' {
            let mut multiplier_start = start - 1;
            while multiplier_start > 0 && bytes[multiplier_start - 1].is_ascii_digit() {
                multiplier_start -= 1;
            }
            if multiplier_start < start - 1 {
                if let Ok(multiplier) = lower[multiplier_start..start - 1].parse::<f32>() {
                    candidates.push(value * multiplier);
                    continue;
                }
            }
        }
        candidates.push(value);
    }
    candidates
        .into_iter()
        .filter(|v| v.is_finite() && *v > 0.0)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
}

fn has_small_model_hint(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| matches!(token, "mini" | "small" | "lite" | "nano" | "flash"))
}

/// Select a bounded, relevant tool set for a small model.  Router tools are
/// retained even when the query has no lexical overlap, while the remaining
/// slots are filled by stable relevance scoring.
pub fn select_tools_for_query(
    definitions: &[ToolDefinition],
    query: &str,
    budget: Option<usize>,
) -> Vec<ToolDefinition> {
    let Some(budget) = budget else {
        return definitions.to_vec();
    };
    if budget == 0 {
        return Vec::new();
    }
    if definitions.len() <= budget {
        return definitions.to_vec();
    }
    let query_tokens = tokens(query);
    let mut scored: Vec<(usize, i32)> = definitions
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let name = definition.name.to_ascii_lowercase();
            let haystack = format!(
                "{} {} {}",
                name,
                definition.description.to_ascii_lowercase(),
                definition.tags.join(" ").to_ascii_lowercase()
            );
            let overlap = query_tokens
                .iter()
                .filter(|token| haystack.contains(token.as_str()))
                .count() as i32;
            let intent_bonus = intent_bonus(query, &name);
            let router_bonus = if matches!(
                name.as_str(),
                "skill_search"
                    | "skill_recommend"
                    | "skill_load"
                    | "plugin_search"
                    | "read_file"
                    | "grep"
                    | "glob"
            ) {
                3
            } else {
                0
            };
            let core_action_bonus = if matches!(
                name.as_str(),
                "write_file" | "edit_file" | "apply_patch" | "shell" | "list_dir"
            ) {
                2
            } else {
                0
            };
            let plugin_penalty = if definition.plugin_id.is_some() {
                -1
            } else {
                0
            };
            (
                index,
                overlap * 10 + intent_bonus + router_bonus + core_action_bonus + plugin_penalty,
            )
        })
        .collect();
    scored.sort_by(|(left_i, left_score), (right_i, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_i.cmp(right_i))
    });
    let mut selected: Vec<usize> = scored
        .into_iter()
        .take(budget)
        .map(|(index, _)| index)
        .collect();
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| definitions[index].clone())
        .collect()
}

/// Trim a tool description for the model-facing request while retaining the
/// full description in the registry and audit log.
pub fn compact_tool_description(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    if max_chars <= 1 {
        return "…".chars().take(max_chars).collect();
    }
    let mut output: String = input.chars().take(max_chars - 1).collect();
    output.push('…');
    output
}

/// Produce a compact JSON Schema for a small model.
///
/// The registry keeps the original schema for strict validation. This
/// projection removes prose and rarely useful annotation keywords, recursively
/// preserving the interoperable subset (`type`, `properties`, `required`,
/// `items`, enums and basic bounds). If a provider still imposes a very small
/// request budget, a minimal object schema is returned rather than emitting an
/// oversized tool contract.
pub fn compact_tool_schema(schema: &Value, max_chars: usize) -> Value {
    if max_chars == 0 {
        return Value::Object(Map::new());
    }
    let compact = compact_schema_value(schema, false);
    if serialized_chars(&compact) <= max_chars {
        return compact;
    }
    let minimal = compact_schema_value(&compact, true);
    if serialized_chars(&minimal) <= max_chars {
        return minimal;
    }
    if let Some(object) = minimal.as_object() {
        let mut fallback = Map::new();
        if let Some(kind) = object.get("type") {
            fallback.insert("type".into(), kind.clone());
        }
        if let Some(required) = object.get("required") {
            if object.get("properties").is_some() {
                fallback.insert("required".into(), required.clone());
            }
        }
        let candidate = Value::Object(fallback);
        if serialized_chars(&candidate) <= max_chars {
            return candidate;
        }
    }
    // The caller enforces the minimum budget at 256 characters, so this is a
    // valid, tiny schema for pathological plugin definitions.
    json_object_schema()
}

fn compact_schema_value(value: &Value, minimal: bool) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| compact_schema_value(item, minimal))
                .collect(),
        ),
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, child) in object {
                let keep = match key.as_str() {
                    "type"
                    | "properties"
                    | "required"
                    | "items"
                    | "enum"
                    | "anyOf"
                    | "oneOf"
                    | "allOf"
                    | "additionalProperties"
                    | "$ref"
                    | "$defs"
                    | "definitions"
                    | "patternProperties"
                    | "dependentSchemas"
                    | "const"
                    | "nullable" => true,
                    "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum"
                    | "minLength" | "maxLength" | "minItems" | "maxItems" | "pattern"
                    | "format" => !minimal,
                    _ => false,
                };
                if !keep {
                    continue;
                }
                output.insert(key.clone(), compact_schema_value(child, minimal));
            }
            // A schema often uses a nested property map. Recurse explicitly so
            // property names remain stable while annotation keys disappear.
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                let mut compact_properties = Map::new();
                for (name, child) in properties {
                    compact_properties.insert(name.clone(), compact_schema_value(child, minimal));
                }
                output.insert("properties".into(), Value::Object(compact_properties));
            }
            for keyword in [
                "$defs",
                "definitions",
                "patternProperties",
                "dependentSchemas",
            ] {
                if let Some(named) = object.get(keyword).and_then(Value::as_object) {
                    let mut compact_named = Map::new();
                    for (name, child) in named {
                        compact_named.insert(name.clone(), compact_schema_value(child, minimal));
                    }
                    output.insert(keyword.into(), Value::Object(compact_named));
                }
            }
            Value::Object(output)
        }
        other => other.clone(),
    }
}

fn serialized_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.chars().count())
        .unwrap_or(usize::MAX)
}

fn json_object_schema() -> Value {
    serde_json::json!({"type": "object"})
}

fn tokens(input: &str) -> Vec<String> {
    input
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|token| token.len() >= 2)
        .map(str::to_string)
        .collect()
}

fn intent_bonus(query: &str, tool_name: &str) -> i32 {
    let query = query.to_ascii_lowercase();
    let matches = |terms: &[&str]| terms.iter().any(|term| query.contains(term));
    let tool_matches = |names: &[&str]| names.contains(&tool_name);
    if matches(&["文件", "代码", "读取", "查看", "file", "code"])
        && tool_matches(&["read_file", "list_dir", "grep", "glob"])
    {
        return 8;
    }
    if matches(&["搜索", "查找", "检索", "grep", "search", "find"])
        && tool_matches(&["grep", "glob", "skill_search", "plugin_search"])
    {
        return 8;
    }
    if matches(&["修改", "编辑", "写入", "修复", "edit", "write", "fix"])
        && tool_matches(&["edit_file", "apply_patch", "write_file", "read_file"])
    {
        return 8;
    }
    if matches(&[
        "命令", "运行", "测试", "编译", "shell", "command", "test", "build",
    ]) && tool_matches(&["shell", "todo_write", "read_file"])
    {
        return 7;
    }
    if matches(&["网址", "网页", "网络", "url", "http", "web"]) && tool_matches(&["web_fetch"])
    {
        return 8;
    }
    if matches(&["技能", "skill"])
        && tool_matches(&["skill_search", "skill_load", "skill_recommend"])
    {
        return 8;
    }
    if matches(&["插件", "plugin"]) && tool_matches(&["plugin_search"]) {
        return 8;
    }
    0
}

/// Preserve both the beginning and the end of a tool result.  The tail often
/// contains an exit status, diagnostics, or a final matching line that a
/// head-only truncation would lose.
pub fn compact_tool_result(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let marker = "\n…[compact result truncated]…\n";
    if max_chars <= marker.chars().count() + 2 {
        return input.chars().take(max_chars).collect();
    }
    let available = max_chars.saturating_sub(marker.chars().count());
    let head_len = available / 2;
    let tail_len = available.saturating_sub(head_len);
    let head: String = input.chars().take(head_len).collect();
    let tail: String = input
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}{marker}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_common_parameter_markers_and_moe_ids() {
        assert_eq!(infer_parameter_count_b("qwen2.5-32b-instruct"), Some(32.0));
        assert_eq!(infer_parameter_count_b("mixtral-8x7b"), Some(56.0));
        assert_eq!(infer_parameter_count_b("deepseek-v4-pro"), None);
    }

    #[test]
    fn strict_cutoff_keeps_70b_on_standard_branch() {
        let cfg = ModelOptimizationConfig::default();
        assert!(!ModelProfile::for_model("llama-70b", &cfg).small_model);
        assert!(ModelProfile::for_model("llama-32b", &cfg).small_model);
    }

    #[test]
    fn explicit_small_mode_handles_aliases_without_size_in_name() {
        let cfg = ModelOptimizationConfig {
            mode: ModelOptimizationMode::Small,
            ..ModelOptimizationConfig::default()
        };
        let profile = ModelProfile::for_model("my-local-quant", &cfg);
        assert!(profile.small_model);
        assert!(profile.prompt_section().contains("below 70B"));
    }

    #[test]
    fn optimization_mode_parser_accepts_compact_alias() {
        assert_eq!(
            "compact".parse::<ModelOptimizationMode>().unwrap(),
            ModelOptimizationMode::Small
        );
        assert_eq!(
            "off".parse::<ModelOptimizationMode>().unwrap(),
            ModelOptimizationMode::Off
        );
    }

    #[test]
    fn bounded_tool_selection_keeps_router_tools() {
        let defs = vec![
            ToolDefinition::builtin("shell", "run commands", json!({})),
            ToolDefinition::builtin("skill_search", "search skills", json!({})),
            ToolDefinition::builtin("read_file", "read a file", json!({})),
            ToolDefinition::builtin("web_fetch", "fetch URL", json!({})),
        ];
        let selected = select_tools_for_query(&defs, "inspect a source file", Some(2));
        let names: Vec<_> = selected.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"skill_search"));
    }

    #[test]
    fn chinese_intent_keeps_matching_tools_in_small_budget() {
        let defs = vec![
            ToolDefinition::builtin("shell", "run commands", json!({})),
            ToolDefinition::builtin("web_fetch", "fetch URL", json!({})),
            ToolDefinition::builtin("read_file", "read a file", json!({})),
        ];
        let selected = select_tools_for_query(&defs, "读取代码文件", Some(1));
        assert_eq!(selected[0].name, "read_file");
    }

    #[test]
    fn zero_tool_budget_is_explicitly_empty() {
        let defs = vec![ToolDefinition::builtin("read_file", "read", json!({}))];
        assert!(select_tools_for_query(&defs, "read", Some(0)).is_empty());
    }

    #[test]
    fn compaction_preserves_head_and_tail() {
        let output = compact_tool_result(
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            40,
        );
        assert!(output.contains("abc"));
        assert!(output.contains("xyz"));
        assert!(output.contains("compact result"));
        assert!(output.chars().count() <= 40);
    }

    #[test]
    fn compact_schema_drops_annotations_but_keeps_contract() {
        let schema = json!({
            "type": "object",
            "description": "A very long description that should not be sent to a small model",
            "additionalProperties": false,
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "another long explanation",
                    "minLength": 1
                }
            }
        });
        let compact = compact_tool_schema(&schema, 240);
        let text = serde_json::to_string(&compact).unwrap();
        assert!(text.chars().count() <= 240);
        assert_eq!(compact["type"], "object");
        assert_eq!(compact["required"][0], "path");
        assert!(compact["properties"]["path"].get("description").is_none());
    }

    #[test]
    fn compact_schema_preserves_named_definitions_for_refs() {
        let schema = json!({
            "type": "object",
            "$defs": {
                "entry": {"type": "string", "description": "long prose"}
            },
            "properties": {"value": {"$ref": "#/$defs/entry"}}
        });
        let compact = compact_tool_schema(&schema, 500);
        assert_eq!(compact["$defs"]["entry"]["type"], "string");
        assert_eq!(compact["properties"]["value"]["$ref"], "#/$defs/entry");
    }

    #[test]
    fn small_policy_exposes_schema_budget() {
        let profile = ModelProfile::for_model("qwen2.5-14b", &ModelOptimizationConfig::default());
        assert!(profile.small_model);
        assert_eq!(profile.policy.tool_schema_chars, Some(2_400));
        assert!(profile.policy.compact_tool_schemas);
        assert_eq!(profile.policy.parallel_tool_calls, Some(false));
    }
}
