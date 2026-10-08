use crate::types::{validate_tool_arguments, ToolCall, ToolDefinition, ToolError, ToolHandler};
use indexmap::IndexMap;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct ToolRegistry {
    tools: RwLock<IndexMap<String, Arc<dyn ToolHandler>>>,
    generation: AtomicU64,
    defs_cache: RwLock<Option<(u64, Vec<ToolDefinition>)>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(IndexMap::new()),
            generation: AtomicU64::new(0),
            defs_cache: RwLock::new(None),
        }
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        *self.defs_cache.write() = None;
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn register(&self, handler: Arc<dyn ToolHandler>) {
        let name = handler.definition().name.clone();
        self.tools.write().insert(name, handler);
        self.bump();
    }

    pub fn unregister(&self, name: &str) -> bool {
        let removed = self.tools.write().shift_remove(name).is_some();
        if removed {
            self.bump();
        }
        removed
    }

    pub fn unregister_plugin(&self, plugin_id: &str) {
        let mut tools = self.tools.write();
        let before = tools.len();
        tools.retain(|_, h| h.definition().plugin_id.as_deref() != Some(plugin_id));
        let changed = tools.len() != before;
        drop(tools);
        if changed {
            self.bump();
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.tools.read().get(name).cloned()
    }

    pub fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.get(name).map(|handler| handler.definition())
    }

    /// Resolve and validate a call before any policy prompt or side effect.
    pub fn preflight(&self, call: &ToolCall) -> Result<ToolDefinition, ToolError> {
        let Some(handler) = self.get(&call.name) else {
            return Err(ToolError::Message(format!("unknown tool: {}", call.name)));
        };
        let definition = handler.definition();
        validate_tool_arguments(&definition, &call.arguments)?;
        handler.validate_arguments(&call.arguments)?;
        Ok(definition)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let gen = self.generation();
        if let Some((g, defs)) = self.defs_cache.read().as_ref() {
            if *g == gen {
                return defs.clone();
            }
        }
        let defs: Vec<ToolDefinition> =
            self.tools.read().values().map(|h| h.definition()).collect();
        *self.defs_cache.write() = Some((gen, defs.clone()));
        defs
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.read().keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
