//! Outer-layer credentials (API keys). Never stored under the core crate tree.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const CREDENTIALS_FILE: &str = "credentials.env";

pub fn credentials_path(outer_home: &Path) -> PathBuf {
    outer_home.join(CREDENTIALS_FILE)
}

/// Resolve DeepSeek API key from (in order):
/// 1. process env `DEEPSEEK_API_KEY`
/// 2. outer `credentials.env`
/// 3. empty string (boot still succeeds; configure later)
pub fn resolve_api_key(outer_home: &Path) -> String {
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return key;
        }
    }
    load_api_key(outer_home).unwrap_or_default()
}

pub fn load_api_key(outer_home: &Path) -> Option<String> {
    let path = credentials_path(outer_home);
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("DEEPSEEK_API_KEY=") {
            let v = rest.trim().trim_matches('"').trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn save_api_key(outer_home: &Path, api_key: &str) -> Result<PathBuf> {
    let key = api_key.trim();
    if key.is_empty() {
        anyhow::bail!("API key must not be empty");
    }
    fs::create_dir_all(outer_home)
        .with_context(|| format!("create {}", outer_home.display()))?;
    let path = credentials_path(outer_home);
    let body = format!(
        "# dsh-rust outer credentials (do not commit)\nDEEPSEEK_API_KEY={key}\n"
    );
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    // Keep process env in sync for this session.
    std::env::set_var("DEEPSEEK_API_KEY", key);
    Ok(path)
}

pub fn clear_api_key(outer_home: &Path) -> Result<()> {
    let path = credentials_path(outer_home);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    std::env::remove_var("DEEPSEEK_API_KEY");
    Ok(())
}

pub fn api_key_status(outer_home: &Path) -> String {
    let from_env = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let from_file = load_api_key(outer_home);
    match (from_env, from_file) {
        (Some(k), _) => format!("configured (env, …{})", mask_tail(&k)),
        (None, Some(k)) => format!(
            "configured ({} , …{})",
            credentials_path(outer_home).display(),
            mask_tail(&k)
        ),
        (None, None) => "not configured — run `dsh config set-api-key <KEY>` or TUI `/apikey <KEY>`"
            .into(),
    }
}

fn mask_tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 4 {
        "****".into()
    } else {
        chars[chars.len().saturating_sub(4)..].iter().collect()
    }
}
