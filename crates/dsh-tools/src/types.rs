use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// Outer-layer plugin id when this tool comes from a plugin.
    pub plugin_id: Option<String>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: ToolMetadata,
}

impl ToolDefinition {
    pub fn builtin(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        let name = name.into();
        Self {
            metadata: ToolMetadata::for_name(&name),
            name,
            description: description.into(),
            parameters,
            plugin_id: None,
            tags: vec!["builtin".into()],
        }
    }

    pub fn with_metadata(mut self, metadata: ToolMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Validate a tool call against the model-facing JSON Schema.
///
/// Tool schemas in this workspace are intentionally small, so the P0
/// validator covers the interoperable subset needed by builtins and plugins:
/// object/array primitives, required fields, nested properties, enum, numeric
/// and length bounds, and `additionalProperties: false`.  Unknown schema
/// keywords are ignored for forward compatibility.
pub fn validate_tool_arguments(
    definition: &ToolDefinition,
    arguments: &Value,
) -> Result<(), ToolError> {
    validate_schema(&definition.parameters, arguments, "$")
}

fn validate_schema(schema: &Value, value: &Value, path: &str) -> Result<(), ToolError> {
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        if any_of
            .iter()
            .any(|candidate| validate_schema(candidate, value, path).is_ok())
        {
            return Ok(());
        }
        return Err(schema_error(path, "does not match any allowed schema"));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = one_of
            .iter()
            .filter(|candidate| validate_schema(candidate, value, path).is_ok())
            .count();
        if matches == 1 {
            return Ok(());
        }
        return Err(schema_error(path, "must match exactly one allowed schema"));
    }

    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        if !matches_type(value, expected) {
            return Err(schema_error(
                path,
                &format!("expected {expected}, got {}", value_type(value)),
            ));
        }
    }

    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if !enum_values.iter().any(|candidate| candidate == value) {
            return Err(schema_error(path, "is not an allowed value"));
        }
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(schema_error(
                    path,
                    &format!("missing required field `{field}`"),
                ));
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        if let Some(object) = value.as_object() {
            for (name, child_schema) in properties {
                if let Some(child) = object.get(name) {
                    validate_schema(child_schema, child, &format!("{path}.{name}"))?;
                }
            }
            if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                for name in object.keys() {
                    if !properties.contains_key(name) {
                        return Err(schema_error(
                            &format!("{path}.{name}"),
                            "additional property is not allowed",
                        ));
                    }
                }
            }
        }
    }

    if let Some(items) = schema.get("items") {
        if let Some(array) = value.as_array() {
            for (index, child) in array.iter().enumerate() {
                validate_schema(items, child, &format!("{path}[{index}]"))?;
            }
        }
    }

    if let Some(length) = schema.get("minLength").and_then(Value::as_u64) {
        if value.as_str().map(|text| text.chars().count()).unwrap_or(0) < length as usize {
            return Err(schema_error(
                path,
                &format!("must have at least {length} characters"),
            ));
        }
    }
    if let Some(length) = schema.get("maxLength").and_then(Value::as_u64) {
        if value
            .as_str()
            .map(|text| text.chars().count())
            .unwrap_or(usize::MAX)
            > length as usize
        {
            return Err(schema_error(
                path,
                &format!("must have at most {length} characters"),
            ));
        }
    }
    if let Some(length) = schema.get("minItems").and_then(Value::as_u64) {
        if value.as_array().map(|items| items.len()).unwrap_or(0) < length as usize {
            return Err(schema_error(
                path,
                &format!("must have at least {length} items"),
            ));
        }
    }
    if let Some(length) = schema.get("maxItems").and_then(Value::as_u64) {
        if value
            .as_array()
            .map(|items| items.len())
            .unwrap_or(usize::MAX)
            > length as usize
        {
            return Err(schema_error(
                path,
                &format!("must have at most {length} items"),
            ));
        }
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value
            .as_f64()
            .map(|number| number < minimum)
            .unwrap_or(false)
        {
            return Err(schema_error(path, &format!("must be >= {minimum}")));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if value
            .as_f64()
            .map(|number| number > maximum)
            .unwrap_or(false)
        {
            return Err(schema_error(path, &format!("must be <= {maximum}")));
        }
    }

    Ok(())
}

fn matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn schema_error(path: &str, message: &str) -> ToolError {
    ToolError::Message(format!("invalid tool arguments at {path}: {message}"))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCapability {
    Read,
    Write,
    Process,
    Network,
    Secret,
    Memory,
    Plugin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolConcurrency {
    #[default]
    Parallel,
    Serial,
    Exclusive,
}

/// Execution metadata used by the P0 policy layer.  It is intentionally
/// separate from the model-facing JSON Schema so policy can evolve without
/// changing the provider request shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolMetadata {
    #[serde(default)]
    pub capabilities: Vec<ToolCapability>,
    #[serde(default)]
    pub risk: ToolRisk,
    #[serde(default = "default_true")]
    pub idempotent: bool,
    #[serde(default)]
    pub concurrency: ToolConcurrency,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

impl Default for ToolMetadata {
    fn default() -> Self {
        Self {
            capabilities: Vec::new(),
            risk: ToolRisk::Low,
            idempotent: true,
            concurrency: ToolConcurrency::Parallel,
            requires_approval: false,
            timeout_secs: None,
        }
    }
}

impl ToolMetadata {
    pub fn read_only() -> Self {
        Self {
            capabilities: vec![ToolCapability::Read],
            ..Self::default()
        }
    }

    pub fn write() -> Self {
        Self {
            capabilities: vec![ToolCapability::Write],
            risk: ToolRisk::Medium,
            concurrency: ToolConcurrency::Serial,
            requires_approval: true,
            ..Self::default()
        }
    }

    pub fn process() -> Self {
        Self {
            capabilities: vec![ToolCapability::Process],
            risk: ToolRisk::High,
            idempotent: false,
            concurrency: ToolConcurrency::Exclusive,
            requires_approval: true,
            ..Self::default()
        }
    }

    pub fn network() -> Self {
        Self {
            capabilities: vec![ToolCapability::Network],
            risk: ToolRisk::Medium,
            ..Self::default()
        }
    }

    pub fn plugin_default() -> Self {
        Self {
            capabilities: vec![ToolCapability::Plugin],
            risk: ToolRisk::High,
            idempotent: false,
            concurrency: ToolConcurrency::Serial,
            requires_approval: true,
            ..Self::default()
        }
    }

    pub fn for_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "read_file" | "list_dir" | "glob" | "grep" | "todo_read" | "skill_list"
            | "skill_search" | "skill_recommend" | "skill_load" | "plugin_list"
            | "plugin_search" | "learn_recall" | "learn_weights" => Self::read_only(),
            "write_file" | "edit_file" | "apply_patch" | "todo_write" => Self::write(),
            "shell" => Self::process(),
            "web_fetch" => Self::network(),
            "plugin_install" | "plugin_reload" | "plugin_unload" => Self::plugin_default(),
            _ => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub outer_home: PathBuf,
    pub workspace_outer: PathBuf,
    pub cancel: tokio::sync::watch::Receiver<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub content: String,
    pub data: Option<Value>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            data: None,
        }
    }

    pub fn ok_json(content: impl Into<String>, data: Value) -> Self {
        Self {
            ok: true,
            content: content.into(),
            data: Some(data),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
            data: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    /// Optional semantic checks performed after JSON Schema validation and
    /// before a caller requests approval or executes side effects.
    fn validate_arguments(&self, _arguments: &Value) -> Result<(), ToolError> {
        Ok(())
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_metadata_exposes_policy_signals() {
        let read = ToolDefinition::builtin("read_file", "read", serde_json::json!({}));
        assert_eq!(read.metadata.capabilities, vec![ToolCapability::Read]);
        assert!(read.metadata.idempotent);
        assert!(!read.metadata.requires_approval);

        let shell = ToolDefinition::builtin("shell", "run", serde_json::json!({}));
        assert_eq!(shell.metadata.capabilities, vec![ToolCapability::Process]);
        assert_eq!(shell.metadata.risk, ToolRisk::High);
        assert!(!shell.metadata.idempotent);
        assert!(shell.metadata.requires_approval);
    }

    #[test]
    fn metadata_defaults_when_loading_legacy_definition() {
        let legacy = serde_json::json!({
            "name": "legacy",
            "description": "legacy tool",
            "parameters": {"type": "object"},
            "plugin_id": null,
            "tags": []
        });
        let definition: ToolDefinition = serde_json::from_value(legacy).expect("legacy definition");
        assert!(definition.metadata.idempotent);
        assert_eq!(definition.metadata.risk, ToolRisk::Low);
    }

    #[test]
    fn validates_required_nested_and_unknown_fields() {
        let definition = ToolDefinition::builtin(
            "example",
            "example",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "items": {
                        "type": "array",
                        "items": {"type": "integer"}
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        );
        assert!(validate_tool_arguments(
            &definition,
            &serde_json::json!({
                "name": "ok",
                "items": [1, 2]
            })
        )
        .is_ok());
        assert!(validate_tool_arguments(&definition, &serde_json::json!({})).is_err());
        assert!(validate_tool_arguments(
            &definition,
            &serde_json::json!({
                "name": "ok",
                "extra": true
            })
        )
        .is_err());
    }
}
