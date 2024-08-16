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
}

impl ToolDefinition {
    pub fn builtin(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            plugin_id: None,
            tags: vec!["builtin".into()],
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
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError>;
}
