use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ctm::CtmConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub llm: LlmSection,
    pub paths: PathsSection,
    pub guard: GuardSection,
    pub agent: AgentSection,
    pub tui: TuiSection,
    #[serde(default)]
    pub ctm: CtmConfig,
    #[serde(default)]
    pub learn: LearnSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmSection {
    pub base_url: String,
    pub model: String,
    pub thinking: bool,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathsSection {
    pub outer_home: String,
    pub workspace_outer: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardSection {
    pub deny_core_writes: bool,
    pub deny_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSection {
    pub max_steps_per_turn: usize,
    pub tool_timeout_secs: u64,
    pub tool_result_max_chars: usize,
    /// Max skills injected into the turn prompt (progressive disclosure).
    #[serde(default = "default_skill_prompt_k")]
    pub skill_prompt_topk: usize,
    #[serde(default = "default_plugin_prompt_k")]
    pub plugin_prompt_topk: usize,
}

fn default_skill_prompt_k() -> usize {
    4
}
fn default_plugin_prompt_k() -> usize {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct TuiSection {
    pub show_thinking: bool,
    pub sidebar: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearnSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for LearnSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let text = fs::read_to_string(path.as_ref())
            .with_context(|| format!("read config {}", path.as_ref().display()))?;
        let cfg: Self = toml::from_str(&text)?;
        Ok(cfg)
    }

    pub fn load_default(workspace_root: &Path) -> Result<Self> {
        let candidates = [
            workspace_root.join("config/default.toml"),
            PathBuf::from("config/default.toml"),
        ];
        for c in candidates {
            if c.exists() {
                return Self::load(c);
            }
        }
        Ok(Self::builtin_default())
    }

    pub fn builtin_default() -> Self {
        Self {
            llm: LlmSection {
                base_url: "https://api.deepseek.com".into(),
                model: "deepseek-v4-pro".into(),
                thinking: true,
                max_tokens: 8192,
                temperature: 0.2,
            },
            paths: PathsSection {
                outer_home: String::new(),
                workspace_outer: ".dsh-rust".into(),
            },
            guard: GuardSection {
                deny_core_writes: true,
                deny_patterns: vec![
                    "crates/**".into(),
                    "target/**".into(),
                    "Cargo.toml".into(),
                    "Cargo.lock".into(),
                    "config/default.toml".into(),
                ],
            },
            agent: AgentSection {
                max_steps_per_turn: 24,
                tool_timeout_secs: 120,
                tool_result_max_chars: 16000,
                skill_prompt_topk: 4,
                plugin_prompt_topk: 3,
            },
            tui: TuiSection {
                show_thinking: true,
                sidebar: true,
            },
            ctm: CtmConfig::default(),
            learn: LearnSection::default(),
        }
    }

    pub fn resolve_outer_home(&self) -> Result<PathBuf> {
        if self.paths.outer_home.trim().is_empty() {
            let home = dirs::home_dir().context("cannot resolve home directory")?;
            Ok(home.join(".dsh-rust"))
        } else {
            Ok(PathBuf::from(&self.paths.outer_home))
        }
    }

    pub fn to_llm_config(&self, api_key: String) -> dsh_llm::LlmConfig {
        dsh_llm::LlmConfig {
            api_key,
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| self.llm.base_url.clone()),
            model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| self.llm.model.clone()),
            thinking: self.llm.thinking,
            max_tokens: self.llm.max_tokens,
            temperature: self.llm.temperature,
        }
    }
}
