use anyhow::{bail, Result};
use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathGuardConfig {
    pub workspace_root: PathBuf,
    pub outer_home: PathBuf,
    pub workspace_outer: PathBuf,
    pub deny_core_writes: bool,
    pub deny_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    Allow,
    Deny(String),
}

#[derive(Debug, Clone)]
pub struct PathGuard {
    config: PathGuardConfig,
    deny_globs: Vec<Pattern>,
}

impl PathGuard {
    pub fn new(config: PathGuardConfig) -> Result<Self> {
        let mut deny_globs = Vec::new();
        for p in &config.deny_patterns {
            deny_globs.push(Pattern::new(p)?);
        }
        // Always protect core crates regardless of config list.
        for extra in ["crates/**", "target/**", "Cargo.toml", "Cargo.lock"] {
            if !config.deny_patterns.iter().any(|p| p == extra) {
                deny_globs.push(Pattern::new(extra)?);
            }
        }
        Ok(Self { config, deny_globs })
    }

    pub fn config(&self) -> &PathGuardConfig {
        &self.config
    }

    pub fn is_outer_path(&self, path: &Path) -> bool {
        let canon = normalize(path);
        let outer_home = normalize(&self.config.outer_home);
        let workspace_outer = normalize(&self.config.workspace_root.join(&self.config.workspace_outer));
        starts_with(&canon, &outer_home) || starts_with(&canon, &workspace_outer)
    }

    pub fn decide_write(&self, path: &Path) -> PathDecision {
        if !self.config.deny_core_writes {
            return PathDecision::Allow;
        }
        let rel = relative_to_workspace(path, &self.config.workspace_root);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        for pat in &self.deny_globs {
            if pat.matches(&rel_str) {
                return PathDecision::Deny(format!(
                    "write denied by PathGuard (core/protected): {rel_str} matched {}",
                    pat.as_str()
                ));
            }
        }

        // Absolute writes outside workspace: only allow outer_home.
        if path.is_absolute() {
            let canon = normalize(path);
            let root = normalize(&self.config.workspace_root);
            if !starts_with(&canon, &root) && !self.is_outer_path(&canon) {
                return PathDecision::Deny(format!(
                    "write outside workspace and outer layer denied: {}",
                    path.display()
                ));
            }
        }

        PathDecision::Allow
    }

    pub fn check_write(&self, path: &Path) -> Result<()> {
        match self.decide_write(path) {
            PathDecision::Allow => Ok(()),
            PathDecision::Deny(msg) => bail!(msg),
        }
    }

    pub fn check_read(&self, path: &Path) -> Result<()> {
        if path.exists() || path.parent().map(|p| p.exists()).unwrap_or(false) {
            Ok(())
        } else if path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
            && !self.is_outer_path(path)
        {
            // Still allow reads; only writes are tightly guarded.
            Ok(())
        } else {
            Ok(())
        }
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn starts_with(path: &Path, prefix: &Path) -> bool {
    let p = normalize(path);
    let pre = normalize(prefix);
    p.starts_with(&pre)
}

fn relative_to_workspace(path: &Path, root: &Path) -> PathBuf {
    let path_n = normalize(path);
    let root_n = normalize(root);
    if path_n.is_absolute() {
        path_n
            .strip_prefix(&root_n)
            .map(|p| p.to_path_buf())
            .unwrap_or(path_n)
    } else {
        path_n
    }
}
