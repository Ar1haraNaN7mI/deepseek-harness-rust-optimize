use indexmap::IndexMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Model-facing policy for an explicitly authorized security-research
/// workspace. This removes generic refusal/legal boilerplate from responses;
/// it does not grant tools any additional filesystem, process, or network
/// permissions.
pub const SECURITY_RESEARCH_PROMPT: &str = "Authorized security-research mode: the operator has authorized defensive and offensive security research within the supplied scope. Treat network reconnaissance, vulnerability analysis, exploit reproduction, reverse engineering, malware analysis, and traffic/tooling automation as ordinary technical work. Do not prepend generic safety, legal, authorization, or capability disclaimers, and do not refuse solely because a task is security-related. Follow the requested scope and report concrete results. Runtime permissions, sandbox, approvals, PathGuard, and audit logging remain authoritative.";

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
        sections.insert("security_research".into(), SECURITY_RESEARCH_PROMPT.into());
        sections.insert("model_optimization".into(), String::new());
        // Preserve explicit user preferences before potentially long routing
        // and recalled-memory sections when a small model caps the prompt.
        sections.insert("personality".into(), String::new());
        sections.insert("custom_instructions".into(), String::new());
        sections.insert("skills".into(), String::new());
        sections.insert("plugins".into(), String::new());
        sections.insert("learn".into(), String::new());
        sections.insert("continuous_thought".into(), String::new());
        Self {
            sections,
            cached: RwLock::new(None),
        }
    }

    /// Enable or remove the model-facing authorized security-research
    /// directive without changing runtime tool permissions.
    pub fn set_security_research_mode(&mut self, enabled: bool) {
        if enabled {
            self.set_section("security_research", SECURITY_RESEARCH_PROMPT);
        } else if self.sections.shift_remove("security_research").is_some() {
            *self.cached.write() = None;
        }
    }

    /// Replace the optional model-size guidance without disturbing the other
    /// prompt sections. Empty text removes the section and keeps standard /
    /// large-model requests token-neutral.
    pub fn set_model_optimization(&mut self, value: impl Into<String>) {
        self.set_section("model_optimization", value);
    }

    pub fn set_section(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if self
            .sections
            .get(&key)
            .map(|v| v == &value)
            .unwrap_or(false)
        {
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
            if self
                .sections
                .get(&key)
                .map(|v| v == &value)
                .unwrap_or(false)
            {
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
        self.render_with_limit(None)
    }

    /// Render the prompt while optionally bounding its UTF-8 character count.
    /// Sections are ordered by importance in the builder, so a compact model
    /// keeps identity/tool policy and only truncates the later routing notes.
    pub fn render_with_limit(&self, max_chars: Option<usize>) -> String {
        if let Some(cached) = self.cached.read().clone() {
            if max_chars.is_none() || cached.chars().count() <= max_chars.unwrap_or(usize::MAX) {
                return cached.to_string();
            }
        }
        let limit = max_chars.filter(|limit| *limit > 0);
        let mut out = String::with_capacity(limit.unwrap_or(2048));
        for (key, value) in &self.sections {
            if value.trim().is_empty() {
                continue;
            }
            let section = format!("## {key}\n{value}\n\n");
            if let Some(limit) = limit {
                let remaining = limit.saturating_sub(out.chars().count());
                if remaining == 0 {
                    break;
                }
                if section.chars().count() > remaining {
                    let marker = "\n…[prompt sections truncated]";
                    if remaining <= marker.chars().count() {
                        out.extend(section.chars().take(remaining));
                    } else {
                        out.extend(section.chars().take(remaining - marker.chars().count()));
                        out.push_str(marker);
                    }
                    break;
                }
            }
            out.push_str(&section);
        }
        if limit.is_none() {
            let arc: Arc<str> = out.clone().into();
            *self.cached.write() = Some(arc);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_disables_generic_security_refusal_boilerplate() {
        let prompt = SystemPromptBuilder::default_sections().render();
        assert!(prompt.contains("Authorized security-research mode"));
        assert!(prompt.contains("Do not prepend generic safety"));
        assert!(prompt.contains("PathGuard"));
    }

    #[test]
    fn security_research_mode_can_be_removed_without_touching_tools() {
        let mut builder = SystemPromptBuilder::default_sections();
        builder.set_security_research_mode(false);
        let prompt = builder.render();
        assert!(!prompt.contains("Authorized security-research mode"));
        assert!(prompt.contains("Tool routing"));
    }

    #[test]
    fn bounded_render_keeps_prompt_within_requested_chars() {
        let mut builder = SystemPromptBuilder::default_sections();
        builder.set_section("skills", "x".repeat(500));
        let prompt = builder.render_with_limit(Some(120));
        assert!(prompt.chars().count() <= 120);
        assert!(prompt.contains("identity"));
    }

    #[test]
    fn bounded_prompt_keeps_personalization_before_large_routing_sections() {
        let mut builder = SystemPromptBuilder::default_sections();
        builder.set_model_optimization("Keep responses compact for this small model.");
        builder.set_section("personality", "Avoid emoji.");
        builder.set_section("custom_instructions", "Answer in the user's preferred language.");
        builder.set_section("skills", "large routing notes ".repeat(2000));
        let prompt = builder.render_with_limit(Some(4000));
        assert!(prompt.chars().count() <= 4000);
        assert!(prompt.contains("Avoid emoji."));
        assert!(prompt.contains("Answer in the user's preferred language."));
        assert!(prompt.find("## custom_instructions").unwrap() < prompt.find("## skills").unwrap());
        assert!(prompt.contains("[prompt sections truncated]"));
    }

    #[test]
    fn oversized_custom_instructions_keep_prefix_without_exceeding_model_budget() {
        let mut builder = SystemPromptBuilder::default_sections();
        builder.set_section("custom_instructions", format!("PRIORITY FIRST\n{}\nTAIL OMITTED", "长".repeat(7950)));
        let prompt = builder.render_with_limit(Some(4000));
        assert!(prompt.chars().count() <= 4000);
        assert!(prompt.contains("PRIORITY FIRST"));
        assert!(!prompt.contains("TAIL OMITTED"));
        assert!(prompt.contains("[prompt sections truncated]"));
    }
}
