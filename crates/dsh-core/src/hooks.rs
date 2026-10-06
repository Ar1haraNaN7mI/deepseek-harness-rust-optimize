//! Lifecycle hooks (Codex `/hooks`) — outer-layer TOML.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub name: String,
    /// Event: session_start | turn_start | turn_end | tool_pre | tool_post
    pub event: String,
    pub command: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub trusted: bool,
}

fn default_true() -> bool {
    true
}

pub fn hooks_path(outer_home: &Path) -> PathBuf {
    outer_home.join("hooks.toml")
}

pub fn load_hooks(outer_home: &Path) -> HooksConfig {
    fs::read_to_string(hooks_path(outer_home))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_hooks(outer_home: &Path, cfg: &HooksConfig) -> Result<PathBuf> {
    fs::create_dir_all(outer_home)?;
    let path = hooks_path(outer_home);
    let text = toml::to_string_pretty(cfg).context("serialize hooks")?;
    fs::write(&path, text)?;
    Ok(path)
}

impl HooksConfig {
    pub fn summary(&self) -> String {
        if self.hooks.is_empty() {
            return "hooks: (none)\nconfig: ~/.dsh-rust/hooks.toml\nevents: session_start, turn_start, turn_end, tool_pre, tool_post".to_string();
        }
        let mut lines = vec![format!("hooks ({}):", self.hooks.len())];
        for h in &self.hooks {
            let on = if h.enabled { "on" } else { "off" };
            let trust = if h.trusted { "trusted" } else { "untrusted" };
            lines.push(format!(
                "  [{on}/{trust}] {}  event={}  cmd={}",
                h.name, h.event, h.command
            ));
        }
        lines.join("\n")
    }

    pub fn trust_all(&mut self) {
        for h in &mut self.hooks {
            h.trusted = true;
        }
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        if let Some(h) = self.hooks.iter_mut().find(|h| h.name == name) {
            h.enabled = enabled;
            true
        } else {
            false
        }
    }

    pub fn run_event(&self, event: &str, cwd: &Path, bypass_trust: bool) -> Vec<String> {
        let mut out = Vec::new();
        for h in &self.hooks {
            if !h.enabled || h.event != event {
                continue;
            }
            if !h.trusted && !bypass_trust {
                out.push(format!(
                    "hook `{}` skipped (untrusted) — /hooks trust",
                    h.name
                ));
                continue;
            }
            match run_hook_cmd(&h.command, cwd) {
                Ok(s) => out.push(format!(
                    "hook `{}` ok: {}",
                    h.name,
                    s.chars().take(200).collect::<String>()
                )),
                Err(e) => out.push(format!("hook `{}` err: {e}", h.name)),
            }
        }
        out
    }
}

fn run_hook_cmd(command: &str, cwd: &Path) -> Result<String> {
    #[cfg(windows)]
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", command])
        .current_dir(cwd)
        .output()?;
    #[cfg(not(windows))]
    let output = Command::new("bash")
        .args(["-lc", command])
        .current_dir(cwd)
        .output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr);
    if !err.is_empty() {
        text.push_str(&err);
    }
    if !output.status.success() {
        anyhow::bail!("exit {} — {text}", output.status);
    }
    Ok(text)
}
