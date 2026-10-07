//! Codex-aligned feature flags persisted under outer home.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureFlags {
    #[serde(default)]
    pub flags: BTreeMap<String, bool>,
}

impl FeatureFlags {
    pub fn defaults() -> Self {
        let mut flags = BTreeMap::new();
        for (k, v) in DEFAULT_FLAGS {
            flags.insert((*k).into(), *v);
        }
        Self { flags }
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.flags
            .get(name)
            .copied()
            .or_else(|| {
                DEFAULT_FLAGS
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| *v)
            })
            .unwrap_or(false)
    }

    pub fn set(&mut self, name: &str, enabled: bool) {
        self.flags.insert(name.to_string(), enabled);
    }

    pub fn list(&self) -> Vec<(String, bool)> {
        let mut all: BTreeMap<String, bool> = DEFAULT_FLAGS
            .iter()
            .map(|(k, v)| ((*k).into(), *v))
            .collect();
        for (k, v) in &self.flags {
            all.insert(k.clone(), *v);
        }
        all.into_iter().collect()
    }
}

pub const DEFAULT_FLAGS: &[(&str, bool)] = &[
    ("unified_exec", true),
    ("shell_tool", true),
    ("web_search", true),
    ("memory", true),
    ("experimental_network_proxy", false),
    ("prevent_sleep", false),
    ("raw_scrollback", false),
    ("vim_mode", false),
    ("pets", false),
    ("side_chat", true),
    ("background_terminals", true),
    ("approval_queue", true),
];

pub fn features_path(outer_home: &Path) -> PathBuf {
    outer_home.join("features.toml")
}

pub fn load_features(outer_home: &Path) -> FeatureFlags {
    let path = features_path(outer_home);
    fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(FeatureFlags::defaults)
}

pub fn save_features(outer_home: &Path, features: &FeatureFlags) -> Result<PathBuf> {
    fs::create_dir_all(outer_home)?;
    let path = features_path(outer_home);
    let text = toml::to_string_pretty(features).context("serialize features")?;
    fs::write(&path, text)?;
    Ok(path)
}
