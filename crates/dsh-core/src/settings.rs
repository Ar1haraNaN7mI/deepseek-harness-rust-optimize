//! Outer-layer session settings (Codex-aligned TUI/agent preferences).

use crate::permissions::PermissionMode;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettings {
    #[serde(default)]
    pub permissions: PermissionMode,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<bool>,
    #[serde(default)]
    pub sidebar: Option<bool>,
    #[serde(default)]
    pub show_thinking: Option<bool>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub vim_mode: bool,
    #[serde(default)]
    pub raw_mode: bool,
    #[serde(default = "default_true")]
    pub memory_inject: bool,
    #[serde(default = "default_true")]
    pub memory_generate: bool,
    #[serde(default)]
    pub statusline: Vec<String>,
    #[serde(default)]
    pub title_fields: Vec<String>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub pet: Option<String>,
    #[serde(default)]
    pub experimental: ExperimentalFlags,
    #[serde(default)]
    pub extra_read_dirs: Vec<String>,
    #[serde(default)]
    pub keymap_overrides: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub approval: crate::policy::ApprovalPolicy,
    #[serde(default)]
    pub sandbox: crate::policy::SandboxMode,
    #[serde(default)]
    pub web_search_live: bool,
    #[serde(default)]
    pub add_dirs: Vec<String>,
    #[serde(default)]
    pub bypass_hook_trust: bool,
    /// Treat scoped security research as ordinary technical work in the
    /// model-facing prompt. Runtime permissions and audit controls are
    /// independent and remain authoritative.
    #[serde(default = "default_true")]
    pub security_research_mode: bool,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            permissions: PermissionMode::default(),
            model: None,
            thinking: None,
            sidebar: None,
            show_thinking: None,
            personality: None,
            vim_mode: false,
            raw_mode: false,
            memory_inject: true,
            memory_generate: true,
            statusline: Vec::new(),
            title_fields: Vec::new(),
            theme: None,
            pet: None,
            experimental: ExperimentalFlags::default(),
            extra_read_dirs: Vec::new(),
            keymap_overrides: std::collections::BTreeMap::new(),
            approval: crate::policy::ApprovalPolicy::default(),
            sandbox: crate::policy::SandboxMode::default(),
            web_search_live: false,
            add_dirs: Vec::new(),
            bypass_hook_trust: false,
            security_research_mode: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExperimentalFlags {
    #[serde(default)]
    pub network_proxy: bool,
    #[serde(default)]
    pub prevent_sleep: bool,
}

pub const PERSONALITIES: &[&str] = &[
    "default",
    "concise",
    "explanatory",
    "collaborative",
    "friendly",
];

pub const STATUSLINE_FIELDS: &[&str] = &[
    "model",
    "context",
    "limits",
    "git",
    "tokens",
    "session",
    "permissions",
    "cwd",
];

pub const TITLE_FIELDS: &[&str] = &["project", "status", "thread", "branch", "model", "progress"];

pub const THEMES: &[&str] = &["default", "monokai", "dracula", "github", "ansi", "none"];

pub const PETS: &[&str] = &["none", "cat", "dog", "bunny", "dragon"];

pub fn settings_path(outer_home: &Path) -> PathBuf {
    outer_home.join("settings.toml")
}

pub fn load_settings(outer_home: &Path) -> SessionSettings {
    let path = settings_path(outer_home);
    let mut s: SessionSettings = fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    if s.statusline.is_empty() {
        s.statusline = vec![
            "model".into(),
            "permissions".into(),
            "session".into(),
            "cwd".into(),
        ];
    }
    if s.title_fields.is_empty() {
        s.title_fields = vec!["project".into(), "model".into(), "status".into()];
    }
    s
}

fn default_true() -> bool {
    true
}

pub fn save_settings(outer_home: &Path, settings: &SessionSettings) -> Result<PathBuf> {
    fs::create_dir_all(outer_home)?;
    let path = settings_path(outer_home);
    let text = toml::to_string_pretty(settings).context("serialize settings")?;
    fs::write(&path, text)?;
    Ok(path)
}

pub fn personality_prompt(name: &str) -> &'static str {
    match name {
        "concise" => "Respond concisely. Prefer short paragraphs and bullet points.",
        "explanatory" => "Explain reasoning clearly. Teach as you go without being verbose.",
        "collaborative" => {
            "Collaborate: ask clarifying questions when ambiguous and propose options."
        }
        "friendly" => "Be warm and encouraging while remaining technically precise.",
        _ => "Be direct, precise, and helpful.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_settings_enable_security_research_mode() {
        assert!(SessionSettings::default().security_research_mode);
    }
}
