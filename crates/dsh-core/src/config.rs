use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ctm::CtmConfig;
use crate::model_profile::ModelOptimizationConfig;
use dsh_llm::{LlmBackend, LlmEndpoint};
use serde_json::Value;

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
    /// Transport preset. All presets speak the OpenAI-compatible
    /// `/chat/completions` protocol; the preset only supplies sensible local
    /// defaults and authentication behavior.
    #[serde(default)]
    pub backend: LlmBackend,
    /// Adaptive request budgets for models below 70B.
    #[serde(default)]
    pub optimization: ModelOptimizationConfig,
    /// Optional provider-specific OpenAI-compatible fields (for example
    /// llama.cpp `cache_prompt` or a vLLM structured-output option).
    #[serde(default)]
    pub extra_body: Option<Value>,
    /// Ordered endpoints used before a turn starts streaming. This enables a
    /// local Ollama/llama.cpp/vLLM fallback chain without changing the agent
    /// loop.
    #[serde(default)]
    pub fallbacks: Vec<LlmEndpoint>,
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
    /// Durable worker heartbeat cadence for long-running runs.
    #[serde(default = "default_worker_heartbeat_secs")]
    pub worker_heartbeat_secs: u64,
    /// Lease age after which a run is considered abandoned on startup.
    #[serde(default = "default_worker_stale_secs")]
    pub worker_stale_secs: u64,
    /// Maximum time a human approval may remain unresolved.
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
    /// Enable the local long-running task scheduler.
    #[serde(default = "default_scheduler_enabled")]
    pub scheduler_enabled: bool,
    /// Scheduler polling cadence.
    #[serde(default = "default_scheduler_poll_secs")]
    pub scheduler_poll_secs: u64,
    /// Maximum number of task runs the local scheduler starts concurrently.
    #[serde(default = "default_scheduler_max_concurrency")]
    pub scheduler_max_concurrency: usize,
    /// Default maximum attempts captured on newly-created tasks.
    #[serde(default = "default_retry_max_attempts")]
    pub retry_max_attempts: u32,
    /// Default initial retry backoff captured on newly-created tasks.
    #[serde(default = "default_retry_backoff_secs")]
    pub retry_backoff_secs: u64,
    /// Default retry backoff ceiling captured on newly-created tasks.
    #[serde(default = "default_retry_max_backoff_secs")]
    pub retry_max_backoff_secs: u64,
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

fn default_worker_heartbeat_secs() -> u64 {
    15
}

fn default_worker_stale_secs() -> u64 {
    90
}

fn default_approval_timeout_secs() -> u64 {
    300
}

fn default_scheduler_enabled() -> bool {
    true
}

fn default_scheduler_poll_secs() -> u64 {
    5
}

fn default_scheduler_max_concurrency() -> usize {
    1
}

fn default_retry_max_attempts() -> u32 {
    3
}

fn default_retry_backoff_secs() -> u64 {
    5
}

fn default_retry_max_backoff_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct TuiSection {
    pub show_thinking: bool,
    pub sidebar: bool,
    #[serde(default)]
    pub startup: StartupSection,
}

/// Presentation settings for the local terminal startup sequence.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StartupSection {
    pub enabled: bool,
    pub sound: bool,
    pub volume: f32,
    pub speed: f32,
    pub theme: String,
    pub interactive: bool,
    pub reduced_motion: bool,
}

impl Default for StartupSection {
    fn default() -> Self {
        Self {
            enabled: false,
            sound: true,
            volume: 0.35,
            speed: 1.0,
            theme: "dark".into(),
            interactive: true,
            reduced_motion: false,
        }
    }
}

impl StartupSection {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            matches!(self.theme.as_str(), "dark" | "light"),
            "tui.startup.theme must be dark or light"
        );
        anyhow::ensure!(
            self.speed.is_finite() && (0.25..=3.0).contains(&self.speed),
            "tui.startup.speed must be between 0.25 and 3.0"
        );
        anyhow::ensure!(
            self.volume.is_finite() && (0.0..=1.0).contains(&self.volume),
            "tui.startup.volume must be between 0.0 and 1.0"
        );
        Ok(())
    }
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
        cfg.tui.startup.validate()?;
        Ok(cfg)
    }

    pub fn load_default(workspace_root: &Path) -> Result<Self> {
        // A dedicated workspace config is always intentional and stays strict.
        let dedicated = workspace_root.join(".dsh-rust/config.toml");
        if dedicated.exists() {
            return Self::load(dedicated);
        }
        // Keep the repository's legacy path, without mistaking another app's
        // config/default.toml for DSH or importing a different caller's config.
        let legacy = workspace_root.join("config/default.toml");
        if legacy.exists() {
            let text = fs::read_to_string(&legacy)
                .with_context(|| format!("read config {}", legacy.display()))?;
            if looks_like_dsh_config(&text) {
                return Self::load(legacy);
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
                backend: LlmBackend::DeepSeek,
                optimization: ModelOptimizationConfig::default(),
                extra_body: None,
                fallbacks: Vec::new(),
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
                worker_heartbeat_secs: 15,
                worker_stale_secs: 90,
                approval_timeout_secs: 300,
                scheduler_enabled: true,
                scheduler_poll_secs: 5,
                scheduler_max_concurrency: 1,
                retry_max_attempts: 3,
                retry_backoff_secs: 5,
                retry_max_backoff_secs: 300,
                skill_prompt_topk: 4,
                plugin_prompt_topk: 3,
            },
            tui: TuiSection {
                show_thinking: true,
                sidebar: true,
                startup: StartupSection::default(),
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
        let backend = std::env::var("DSH_LLM_BACKEND")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(self.llm.backend);
        let env_base = std::env::var("DSH_LLM_BASE_URL")
            .or_else(|_| std::env::var("DEEPSEEK_BASE_URL"))
            .ok()
            .filter(|value| !value.trim().is_empty());
        let configured_base = env_base
            .clone()
            .unwrap_or_else(|| self.llm.base_url.clone());
        let using_default_deepseek_url = configured_base.trim_end_matches('/')
            == "https://api.deepseek.com"
            && env_base.is_none();
        let base_url = if configured_base.trim().is_empty()
            || (backend != LlmBackend::DeepSeek && using_default_deepseek_url)
        {
            backend.default_base_url().to_string()
        } else {
            configured_base
        };
        let extra_body = std::env::var("DSH_LLM_EXTRA_BODY")
            .ok()
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .or_else(|| self.llm.extra_body.clone());
        let mut fallbacks = self.llm.fallbacks.clone();
        if let Ok(value) = std::env::var("DSH_LLM_FALLBACK_BASE_URLS") {
            fallbacks.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|url| !url.is_empty())
                    .map(LlmEndpoint::new),
            );
        }
        dsh_llm::LlmConfig {
            api_key,
            base_url,
            model: std::env::var("DSH_LLM_MODEL")
                .or_else(|_| std::env::var("DEEPSEEK_MODEL"))
                .unwrap_or_else(|_| self.llm.model.clone()),
            thinking: self.llm.thinking,
            max_tokens: self.llm.max_tokens,
            temperature: self.llm.temperature,
            backend,
            extra_body,
            fallbacks,
        }
    }
}

fn looks_like_dsh_config(text: &str) -> bool {
    let has_section =
        |value: &toml::Value, name: &str| value.get(name).is_some_and(toml::Value::is_table);
    if let Ok(value) = toml::from_str::<toml::Value>(text) {
        return has_section(&value, "llm") && has_section(&value, "paths");
    }

    // Recognize intact section headers even when a DSH value has invalid TOML,
    // so an edited/broken DSH config reports its error instead of using defaults.
    let mut llm = false;
    let mut paths = false;
    for line in text
        .lines()
        .filter(|line| line.trim_start().starts_with('['))
    {
        if let Ok(value) = toml::from_str::<toml::Value>(line) {
            llm |= has_section(&value, "llm");
            paths |= has_section(&value, "paths");
        }
    }
    llm && paths
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../config/default.toml");

    struct Workspace(PathBuf);

    impl Workspace {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("dsh-config-discovery-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            assert_eq!(
                self.0.parent(),
                Some(std::env::temp_dir().canonicalize().unwrap().as_path())
            );
            assert!(self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-config-discovery-"));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unrelated_legacy_configs_are_ignored_but_explicit_load_is_strict() {
        let workspace = Workspace::new();
        for source in [
            "app_name = 'another-project'\n",
            "[llm]\nmodel = 'another-project'\n",
            "app_name = [invalid TOML\n",
            "description = '''\n[llm]\n[paths]\n'''\n",
        ] {
            let path = workspace.write("config/default.toml", source);
            assert_eq!(
                AppConfig::load_default(&workspace.0).unwrap().llm.model,
                AppConfig::builtin_default().llm.model
            );
            assert!(AppConfig::load(path).is_err());
        }
    }

    #[test]
    fn recognizable_legacy_config_preserves_repo_compatibility_and_validation() {
        let workspace = Workspace::new();
        let customized = EXAMPLE.replace("model = \"deepseek-v4-pro\"", "model = \"legacy-model\"");
        workspace.write("config/default.toml", &customized);
        assert_eq!(
            AppConfig::load_default(&workspace.0).unwrap().llm.model,
            "legacy-model"
        );
        for invalid in [
            "[llm]\nmodel = 'incomplete'\n[paths]\n",
            "[llm]\nmodel = [invalid TOML\n[paths]\n",
            "[ 'llm' ] # quoted section\nmodel = [invalid TOML\n[ \"paths\" ]\n",
        ] {
            workspace.write("config/default.toml", invalid);
            assert!(AppConfig::load_default(&workspace.0).is_err(), "{invalid}");
        }
        workspace.write(
            "config/default.toml",
            &customized.replace("speed = 1.0", "speed = 0.0"),
        );
        assert!(AppConfig::load_default(&workspace.0).is_err());
    }

    #[test]
    fn dedicated_config_takes_priority_and_never_silently_falls_back() {
        let workspace = Workspace::new();
        workspace.write("config/default.toml", EXAMPLE);
        workspace.write(
            ".dsh-rust/config.toml",
            &EXAMPLE.replace("model = \"deepseek-v4-pro\"", "model = \"dedicated-model\""),
        );
        assert_eq!(
            AppConfig::load_default(&workspace.0).unwrap().llm.model,
            "dedicated-model"
        );
        for invalid in ["app_name = 'not-dsh'\n", "invalid = [\n"] {
            workspace.write(".dsh-rust/config.toml", invalid);
            assert!(AppConfig::load_default(&workspace.0).is_err());
        }
    }

    #[test]
    fn selected_workspace_does_not_import_callers_configuration() {
        const CHILD_WORKSPACE: &str = "DSH_CONFIG_DISCOVERY_TEST_WORKSPACE";
        if let Some(workspace) = std::env::var_os(CHILD_WORKSPACE) {
            assert_eq!(
                AppConfig::load_default(Path::new(&workspace))
                    .unwrap()
                    .llm
                    .model,
                AppConfig::builtin_default().llm.model
            );
            return;
        }
        let caller = Workspace::new();
        caller.write(
            "config/default.toml",
            &EXAMPLE.replace("model = \"deepseek-v4-pro\"", "model = \"caller-model\""),
        );
        let selected = Workspace::new();
        // Use a child so the test never changes the other test threads' cwd.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::discovery_tests::selected_workspace_does_not_import_callers_configuration",
            ])
            .current_dir(&caller.0)
            .env(CHILD_WORKSPACE, &selected.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn legacy_tui_config_retains_startup_defaults() {
        let legacy: TuiSection = toml::from_str("show_thinking = true\nsidebar = false\n").unwrap();
        assert!(!legacy.startup.enabled);
        assert!(legacy.startup.sound);
        assert_eq!(legacy.startup.theme, "dark");
        assert_eq!(legacy.startup.volume, 0.35);
        assert_eq!(legacy.startup.speed, 1.0);
        assert!(legacy.startup.interactive);

        let default_config = include_str!("../../../config/default.toml");
        let legacy_config = default_config.split("[tui.startup]").next().unwrap();
        let config: AppConfig = toml::from_str(legacy_config).unwrap();
        assert!(!config.tui.startup.enabled);
        assert_eq!(config.tui.startup.theme, "dark");
    }

    #[test]
    fn partial_startup_config_preserves_unset_defaults() {
        let config: TuiSection = toml::from_str(
            "show_thinking = true\nsidebar = true\n[startup]\nsound = false\ntheme = 'light'\n",
        )
        .unwrap();
        assert!(!config.startup.enabled);
        assert!(!config.startup.sound);
        assert_eq!(config.startup.theme, "light");
        assert_eq!(config.startup.speed, 1.0);
        config.startup.validate().unwrap();
    }

    #[test]
    fn invalid_startup_values_are_rejected() {
        for speed in [0.0, -1.0, f32::NAN, f32::INFINITY, 4.0] {
            let config = StartupSection {
                speed,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
        for volume in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            let config = StartupSection {
                volume,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
        let config = StartupSection {
            theme: "unknown".into(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
