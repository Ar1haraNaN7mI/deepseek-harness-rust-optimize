use indexmap::IndexMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Sectioned system prompt with cache invalidation for outer-layer changes.
#[derive(Debug, Default)]
pub struct SystemPromptBuilder {
    sections: IndexMap<String, String>,
    cached: RwLock<Option<Arc<str>>>,
}

impl SystemPromptBuilder {
    pub fn default_sections() -> Self {
        let mut sections = IndexMap::new();
        sections.insert(
            "identity".into(),
            "You are dsh-rust, a DeepSeek Harness-aligned coding agent.\n\
             Architecture: two nested layers (core immutable / outer writable).\n\
             Session log is the source of model-visible context (derive_messages).\n\
             Never modify crates/, Cargo.toml, target/, or the binary. Extend via outer plugins/skills."
                .into(),
        );
        sections.insert(
            "tools".into(),
            "Tool routing (efficient):\n\
             1) skill_search/skill_recommend → skill_load → follow body\n\
             2) plugin_search → plugin.<id>.<tool>\n\
             3) Code navigation: grep, glob, read_file(offset/limit), list_dir\n\
             4) Edits: edit_file or apply_patch (SEARCH/REPLACE hunks)\n\
             5) shell (cancellable), web_fetch, todo_write/todo_read\n\
             6) Never rewrite core crates; only mutate outer plugins/skills\n\
             7) Outcomes auto-update learn weights"
                .into(),
        );
        sections.insert("skills".into(), String::new());
        sections.insert("plugins".into(), String::new());
        sections.insert("learn".into(), String::new());
        sections.insert("continuous_thought".into(), String::new());
        Self {
            sections,
            cached: RwLock::new(None),
        }
    }

    pub fn set_section(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if self.sections.get(&key).map(|v| v == &value).unwrap_or(false) {
            return;
        }
        self.sections.insert(key, value);
        *self.cached.write() = None;
    }

    /// Batch update sections and invalidate the cache once.
    pub fn set_sections(&mut self, updates: HashMap<&str, String>) {
        let mut changed = false;
        for (key, value) in updates {
            let key = key.to_string();
            if self.sections.get(&key).map(|v| v == &value).unwrap_or(false) {
                continue;
            }
            self.sections.insert(key, value);
            changed = true;
        }
        if changed {
            *self.cached.write() = None;
        }
    }

    pub fn render(&self) -> String {
        if let Some(cached) = self.cached.read().clone() {
            return cached.to_string();
        }
        let mut out = String::with_capacity(2048);
        for (key, value) in &self.sections {
            if value.trim().is_empty() {
                continue;
            }
            out.push_str("## ");
            out.push_str(key);
            out.push('\n');
            out.push_str(value);
            out.push_str("\n\n");
        }
        let arc: Arc<str> = out.clone().into();
        *self.cached.write() = Some(arc);
        out
    }
}
