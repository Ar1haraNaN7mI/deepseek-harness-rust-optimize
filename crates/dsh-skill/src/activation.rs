//! Persistent activation shared by skill and plugin registries.
use anyhow::{bail, Context, Result};
use parking_lot::RwLock;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct ActivationStore {
    path: PathBuf,
    disabled: RwLock<Result<BTreeSet<String>, String>>,
}

impl ActivationStore {
    pub fn new(path: PathBuf) -> Self {
        let disabled = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| format!("read {}: {e}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        };
        Self {
            path,
            disabled: RwLock::new(disabled),
        }
    }

    pub fn disabled(&self) -> Result<BTreeSet<String>> {
        self.disabled.read().clone().map_err(anyhow::Error::msg)
    }

    pub fn enabled(&self, name: &str) -> bool {
        self.disabled
            .read()
            .as_ref()
            .is_ok_and(|disabled| !disabled.contains(name))
    }

    /// Commit disk state before making it visible to routing and execution.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        if name.trim().is_empty() {
            bail!("extension name is empty")
        }
        let mut state = self.disabled.write();
        let mut next = state
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.clone()))?
            .clone();
        if enabled {
            next.remove(name);
        } else {
            next.insert(name.into());
        }
        let parent = self
            .path
            .parent()
            .context("activation file has no parent")?;
        std::fs::create_dir_all(parent)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temporary = parent.join(format!(
            ".activation-{}-{stamp}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&next)?)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)
                .with_context(|| format!("save {}", self.path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result?;
        *state = Ok(next);
        Ok(())
    }
}
