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

    /// Resolve a caller path in the same coordinate system used by policy and
    /// filesystem operations. Relative paths are workspace-relative, which
    /// keeps direct `FsService` users consistent with the builtin tools.
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        policy_path(path, &self.config.workspace_root)
    }

    pub fn is_outer_path(&self, path: &Path) -> bool {
        let canon = policy_path(path, &self.config.workspace_root);
        let canon = canonicalize_for_policy(&canon);
        let outer_home = canonicalize_for_policy(&self.config.outer_home);
        let workspace_outer = canonicalize_for_policy(
            &self
                .config
                .workspace_root
                .join(&self.config.workspace_outer),
        );
        starts_with_platform(&canon, &outer_home) || starts_with_platform(&canon, &workspace_outer)
    }

    pub fn decide_write(&self, path: &Path) -> PathDecision {
        if !self.config.deny_core_writes {
            return PathDecision::Allow;
        }
        let resolved = self.resolve_path(path);
        let canonical = canonicalize_for_policy(&resolved);
        let rel = relative_to_workspace(&canonical, &self.config.workspace_root);
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Keep the kernel invariants independent of glob crate semantics (in
        // particular, `**` behaves differently across path separators).
        if is_builtin_protected_path(&rel_str) {
            return PathDecision::Deny(format!(
                "write denied by PathGuard (core/protected): {rel_str}"
            ));
        }

        for pat in &self.deny_globs {
            if pat.matches(&rel_str)
                || pat.matches_path(Path::new(&rel_str))
                || pat.matches(&rel_str.to_ascii_lowercase())
                || pat.matches_path(Path::new(&rel_str.to_ascii_lowercase()))
            {
                return PathDecision::Deny(format!(
                    "write denied by PathGuard (core/protected): {rel_str} matched {}",
                    pat.as_str()
                ));
            }
        }

        // Resolve existing ancestors to catch a symlink/junction that points
        // outside the workspace. Canonicalizing only the final path is not
        // enough for a new file whose leaf does not exist yet.
        if let Some(existing) = nearest_existing_ancestor(&resolved) {
            let existing = canonicalize_for_policy(&existing);
            let root = canonicalize_for_policy(&self.config.workspace_root);
            let outer_home = canonicalize_for_policy(&self.config.outer_home);
            let workspace_outer = canonicalize_for_policy(
                &self
                    .config
                    .workspace_root
                    .join(&self.config.workspace_outer),
            );
            let allowed = starts_with_platform(&existing, &root)
                || starts_with_platform(&existing, &outer_home)
                || starts_with_platform(&existing, &workspace_outer);
            if !allowed {
                return PathDecision::Deny(format!(
                    "write follows a symlink/junction outside allowed roots: {}",
                    path.display()
                ));
            }
        }

        // Absolute writes outside workspace: only allow outer_home.
        if resolved.is_absolute() {
            let root = canonicalize_for_policy(&self.config.workspace_root);
            if !starts_with_platform(&canonical, &root) && !self.is_outer_path(&canonical) {
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
        let path = self.resolve_path(path);
        if path.exists() || path.parent().map(|p| p.exists()).unwrap_or(false) {
            Ok(())
        } else if path.components().any(|c| matches!(c, Component::ParentDir))
            && !self.is_outer_path(&path)
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

fn starts_with_platform(path: &Path, prefix: &Path) -> bool {
    let p = normalize(path);
    let pre = normalize(prefix);
    #[cfg(windows)]
    {
        let p = PathBuf::from(p.to_string_lossy().to_ascii_lowercase());
        let pre = PathBuf::from(pre.to_string_lossy().to_ascii_lowercase());
        p.starts_with(pre)
    }
    #[cfg(not(windows))]
    {
        p.starts_with(&pre)
    }
}

fn relative_to_workspace(path: &Path, root: &Path) -> PathBuf {
    let path_n = normalize(path);
    let root_n = canonicalize_for_policy(root);
    if path_n.is_absolute() {
        path_n
            .strip_prefix(&root_n)
            .map(|p| p.to_path_buf())
            .unwrap_or(path_n)
    } else {
        path_n
    }
}

fn is_builtin_protected_path(path: &str) -> bool {
    let path = path.trim_start_matches('/');
    path == "Cargo.toml"
        || path == "Cargo.lock"
        || path == "target"
        || path.starts_with("target/")
        || path == "crates"
        || path.starts_with("crates/")
}

fn policy_path(path: &Path, workspace_root: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&workspace_root.join(path))
    }
}

fn canonicalize_for_policy(path: &Path) -> PathBuf {
    // `canonicalize` requires the leaf to exist. For a new file, walk up to
    // the nearest existing ancestor and append the untouched suffix.
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    while !cursor.exists() {
        if let Some(name) = cursor.file_name().map(|name| name.to_os_string()) {
            missing.push(name);
        }
        if !cursor.pop() {
            break;
        }
    }
    let mut base = std::fs::canonicalize(&cursor).unwrap_or_else(|_| normalize(&cursor));
    for component in missing.iter().rev() {
        base.push(component);
    }
    normalize(&base)
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut cursor = path.to_path_buf();
    loop {
        if cursor.exists() {
            return Some(cursor);
        }
        if !cursor.pop() {
            return None;
        }
    }
}
