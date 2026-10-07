//! Persistent DSH terminal, model and agent preferences.

use crate::permissions::PermissionMode;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
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
    pub backend: Option<dsh_llm::LlmBackend>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub send_local_api_key: Option<bool>,
    #[serde(default)]
    pub sidebar: Option<bool>,
    #[serde(default)]
    pub show_thinking: Option<bool>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub custom_instructions: String,
    #[serde(default)]
    pub characteristics: PersonalityCharacteristics,
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
            backend: None,
            base_url: None,
            temperature: None,
            max_tokens: None,
            send_local_api_key: None,
            sidebar: None,
            show_thinking: None,
            personality: None,
            custom_instructions: String::new(),
            characteristics: PersonalityCharacteristics::default(),
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

pub const CUSTOM_INSTRUCTIONS_MAX_CHARS: usize = 8000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CharacteristicLevel {
    #[default]
    Default,
    More,
    Less,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersonalityCharacteristics {
    pub warmth: CharacteristicLevel,
    pub enthusiasm: CharacteristicLevel,
    pub headers_lists: CharacteristicLevel,
    pub emoji: CharacteristicLevel,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsPatch {
    pub model: Option<String>,
    pub thinking: Option<bool>,
    pub backend: Option<dsh_llm::LlmBackend>,
    pub base_url: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub send_local_api_key: Option<bool>,
    pub personality: Option<String>,
    pub custom_instructions: Option<String>,
    pub characteristics: Option<PersonalityCharacteristics>,
    pub memory_inject: Option<bool>,
    pub memory_generate: Option<bool>,
    pub approval: Option<crate::policy::ApprovalPolicy>,
    pub sandbox: Option<crate::policy::SandboxMode>,
    pub security_research_mode: Option<bool>,
    pub sidebar: Option<bool>,
    pub show_thinking: Option<bool>,
}

impl SettingsPatch {
    pub(crate) fn apply(self, settings: &mut SessionSettings) -> Result<()> {
        if let Some(enabled) = self.send_local_api_key {
            settings.send_local_api_key = Some(enabled);
        }
        if let Some(backend) = self.backend {
            settings.backend = Some(backend);
        }
        if let Some(base_url) = self.base_url {
            let base_url = base_url.trim().trim_end_matches('/');
            validate_model_endpoint(base_url)?;
            settings.base_url = Some(base_url.to_owned());
        }
        if let Some(temperature) = self.temperature {
            settings.temperature = Some(temperature);
        }
        if let Some(max_tokens) = self.max_tokens {
            settings.max_tokens = Some(max_tokens);
        }
        if let Some(model) = self.model {
            let model = model.trim();
            anyhow::ensure!(
                !model.is_empty()
                    && model.chars().count() <= 256
                    && !model.chars().any(char::is_control),
                "model must contain 1..256 printable characters"
            );
            settings.model = Some(model.to_string());
        }
        if let Some(name) = self.personality {
            anyhow::ensure!(
                PERSONALITIES.contains(&name.as_str()),
                "Unsupported personality: {name}"
            );
            settings.personality = Some(name);
        }
        if let Some(value) = self.thinking {
            settings.thinking = Some(value);
        }
        if let Some(value) = self.sidebar {
            settings.sidebar = Some(value);
        }
        if let Some(value) = self.show_thinking {
            settings.show_thinking = Some(value);
        }
        if let Some(value) = self.custom_instructions {
            settings.custom_instructions = value;
        }
        if let Some(value) = self.characteristics {
            settings.characteristics = value;
        }
        if let Some(value) = self.memory_inject {
            settings.memory_inject = value;
        }
        if let Some(value) = self.memory_generate {
            settings.memory_generate = value;
        }
        if let Some(value) = self.approval {
            settings.approval = value;
        }
        if let Some(value) = self.security_research_mode {
            settings.security_research_mode = value;
        }
        if let Some(value) = self.sandbox {
            settings.sandbox = value;
            settings.permissions = value.to_permission();
        }
        settings.validate()
    }
}

impl SessionSettings {
    pub fn validate(&self) -> Result<()> {
        if let Some(url) = &self.base_url {
            validate_model_endpoint(url)?;
        }
        if let Some(value) = self.temperature {
            anyhow::ensure!(
                value.is_finite() && (0.0..=2.0).contains(&value),
                "temperature must be between 0 and 2"
            );
        }
        if let Some(value) = self.max_tokens {
            anyhow::ensure!(
                (1..=131072).contains(&value),
                "max_tokens must be between 1 and 131072"
            );
        }
        anyhow::ensure!(
            self.custom_instructions.chars().count() <= CUSTOM_INSTRUCTIONS_MAX_CHARS,
            "Custom instructions must be at most {CUSTOM_INSTRUCTIONS_MAX_CHARS} characters"
        );
        anyhow::ensure!(
            !self
                .custom_instructions
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')),
            "Custom instructions contain unsupported control characters"
        );
        Ok(())
    }

    pub fn personalization_prompt(&self) -> String {
        let mut lines =
            vec![personality_prompt(self.personality.as_deref().unwrap_or("default")).to_string()];
        for (level, more, less) in [
            (
                self.characteristics.warmth,
                "Use a warmer, more considerate tone.",
                "Use a matter-of-fact tone with minimal social phrasing.",
            ),
            (
                self.characteristics.enthusiasm,
                "Express more enthusiasm when appropriate.",
                "Keep enthusiasm restrained and avoid exclamations.",
            ),
            (
                self.characteristics.headers_lists,
                "Use headings and lists more often when they aid readability.",
                "Prefer connected prose with fewer headings and lists.",
            ),
            (
                self.characteristics.emoji,
                "Use occasional relevant emoji where suitable.",
                "Avoid emoji.",
            ),
        ] {
            match level {
                CharacteristicLevel::More => lines.push(more.to_string()),
                CharacteristicLevel::Less => lines.push(less.to_string()),
                CharacteristicLevel::Default => (),
            }
        }
        lines.join("\n")
    }
}

/// Keep credentials separate from the endpoint that is displayed in settings.
pub fn validate_model_endpoint(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() <= 2048 && !value.chars().any(char::is_control),
        "Invalid model service URL"
    );
    let url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid model service URL"))?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "Model service URL must use HTTP or HTTPS"
    );
    anyhow::ensure!(url.username().is_empty() && url.password().is_none() && url.query().is_none() && url.fragment().is_none(), "Put credentials in the API key field; service URL cannot contain user info, query or fragment");
    Ok(())
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
    settings.validate()?;
    fs::create_dir_all(outer_home)?;
    let path = settings_path(outer_home);
    let text = toml::to_string_pretty(settings).context("serialize settings")?;
    atomic_write(&path, text.as_bytes())?;
    Ok(path)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Persistence path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".dsh-write-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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

    #[test]
    fn older_settings_keep_defaults_and_characteristics_validate() {
        let settings: SessionSettings =
            toml::from_str("personality = 'concise'\nmemory_generate = false").unwrap();
        assert_eq!(settings.custom_instructions, "");
        assert_eq!(
            settings.characteristics,
            PersonalityCharacteristics::default()
        );
        assert!(settings.memory_inject);
        assert!(!settings.memory_generate);
        assert!(
            serde_json::from_str::<SettingsPatch>(r#"{"characteristics":{"emoji":"many"}}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"pretend_setting":true}"#).is_err());
    }

    #[test]
    fn custom_instruction_limit_counts_characters_and_rejects_controls() {
        let mut settings = SessionSettings::default();
        settings.custom_instructions = "语".repeat(CUSTOM_INSTRUCTIONS_MAX_CHARS);
        assert!(settings.validate().is_ok());
        settings.custom_instructions.push('a');
        assert!(settings.validate().is_err());
        settings.custom_instructions = "line\nnext\ttab".into();
        assert!(settings.validate().is_ok());
        settings.custom_instructions.push('\u{1b}');
        assert!(settings.validate().is_err());
    }
}
