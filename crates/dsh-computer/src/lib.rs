//! Standalone local computer tools. No dependency on Codex or a browser bridge.
//! The caller owns enablement/approval; construction itself performs no input.
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

#[cfg(windows)]
mod native;
#[cfg(windows)]
mod pointer;
#[cfg(windows)]
pub mod recognition;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerAction {
    ListWindows,
    Snapshot {
        window_id: String,
    },
    Screenshot {
        window_id: String,
        snapshot_id: String,
    },
    Invoke {
        window_id: String,
        snapshot_id: String,
        node_id: String,
    },
    SetValue {
        window_id: String,
        snapshot_id: String,
        node_id: String,
        text: String,
    },
    Click {
        window_id: String,
        snapshot_id: String,
        x: f64,
        y: f64,
        #[serde(default = "left_button")]
        button: String,
    },
    TypeText {
        window_id: String,
        snapshot_id: String,
        node_id: String,
        text: String,
    },
    Key {
        window_id: String,
        snapshot_id: String,
        node_id: String,
        key: String,
    },
    Scroll {
        window_id: String,
        snapshot_id: String,
        node_id: String,
        delta: i32,
    },
}
fn left_button() -> String {
    "left".into()
}

impl ComputerAction {
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::ListWindows | Self::Snapshot { .. } | Self::Screenshot { .. }
        )
    }
    pub fn window_id(&self) -> Option<&str> {
        match self {
            Self::ListWindows => None,
            Self::Snapshot { window_id }
            | Self::Screenshot { window_id, .. }
            | Self::Invoke { window_id, .. }
            | Self::SetValue { window_id, .. }
            | Self::Click { window_id, .. }
            | Self::TypeText { window_id, .. }
            | Self::Key { window_id, .. }
            | Self::Scroll { window_id, .. } => Some(window_id),
        }
    }
    pub fn snapshot_id(&self) -> Option<&str> {
        match self {
            Self::ListWindows | Self::Snapshot { .. } => None,
            Self::Screenshot { snapshot_id, .. }
            | Self::Invoke { snapshot_id, .. }
            | Self::SetValue { snapshot_id, .. }
            | Self::Click { snapshot_id, .. }
            | Self::TypeText { snapshot_id, .. }
            | Self::Key { snapshot_id, .. }
            | Self::Scroll { snapshot_id, .. } => Some(snapshot_id),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(id) = self.window_id() {
            ensure!(
                uuid::Uuid::parse_str(id).is_ok(),
                "Select a window from computer_list_windows first"
            );
        }
        if let Some(id) = self.snapshot_id() {
            ensure!(
                uuid::Uuid::parse_str(id).is_ok(),
                "A current snapshot_id is required"
            );
        }
        match self {
            Self::Invoke { node_id, .. }
            | Self::SetValue { node_id, .. }
            | Self::TypeText { node_id, .. }
            | Self::Key { node_id, .. }
            | Self::Scroll { node_id, .. } => ensure!(
                (node_id == "background"
                    && matches!(
                        self,
                        Self::TypeText { .. } | Self::Key { .. } | Self::Scroll { .. }
                    ))
                    || (node_id.starts_with('n') && node_id[1..].parse::<usize>().is_ok()),
                "Select a node from the current snapshot"
            ),
            _ => (),
        }
        match self {
            Self::SetValue { text, .. } | Self::TypeText { text, .. } => {
                ensure!(
                    text.chars().count() <= 4096 && !text.contains('\0'),
                    "Text must contain at most 4096 characters and no NUL"
                );
                if matches!(self, Self::TypeText { .. }) {
                    ensure!(!text.is_empty(), "Text cannot be empty");
                }
            }
            Self::Click { x, y, button, .. } => ensure!(
                x.is_finite()
                    && y.is_finite()
                    && *x >= 0.0
                    && *y >= 0.0
                    && matches!(button.as_str(), "left" | "right" | "double"),
                "Click needs nonnegative finite window coordinates and left/right/double button"
            ),
            Self::Scroll { delta, .. } => ensure!(
                (-10..=10).contains(delta) && *delta != 0,
                "Scroll delta must be -10..-1 or 1..10 wheel steps"
            ),
            Self::Key { key, .. } => {
                parse_key(key)?;
            }
            _ => (),
        }
        Ok(())
    }
}

pub(crate) fn parse_key(input: &str) -> Result<Vec<u16>> {
    ensure!(!input.is_empty() && input.len() <= 64, "Invalid key chord");
    let parts: Vec<_> = input.split('+').collect();
    ensure!(
        parts.len() <= 4,
        "At most three modifiers and one key are supported"
    );
    let mut keys = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let name = part.trim().to_ascii_uppercase();
        let key = match name.as_str() {
            "CTRL" | "CONTROL" => 0x11, "ALT" => 0x12, "SHIFT" => 0x10,
            "ENTER" | "RETURN" => 0x0d, "TAB" => 0x09, "ESC" | "ESCAPE" => 0x1b,
            "BACKSPACE" => 0x08, "DELETE" | "DEL" => 0x2e, "SPACE" => 0x20,
            "ARROWLEFT" | "LEFT" => 0x25, "ARROWUP" | "UP" => 0x26,
            "ARROWRIGHT" | "RIGHT" => 0x27, "ARROWDOWN" | "DOWN" => 0x28,
            "HOME" => 0x24, "END" => 0x23, "PAGEUP" => 0x21, "PAGEDOWN" => 0x22,
            value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => value.as_bytes()[0] as u16,
            value if value.starts_with('F') && value[1..].parse::<u16>().is_ok_and(|n| (1..=12).contains(&n)) => 0x6f + value[1..].parse::<u16>()?,
            _ => bail!("Unsupported key; use Enter, Tab, Escape, Arrow keys, F1–F12, or Ctrl+A style chords"),
        };
        if index < parts.len() - 1 {
            ensure!(
                matches!(key, 0x10..=0x12),
                "Only Ctrl, Alt and Shift may precede the final key"
            );
        }
        ensure!(!keys.contains(&key), "Repeated modifier/key in chord");
        keys.push(key);
    }
    Ok(keys)
}

#[derive(Clone)]
pub struct ComputerService {
    #[cfg(windows)]
    native: native::Worker,
}
impl Default for ComputerService {
    fn default() -> Self {
        Self::new()
    }
}
impl ComputerService {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            native: native::Worker::new(),
        }
    }
    pub fn supported() -> bool {
        cfg!(windows)
    }
    /// Hide the independent desktop layer immediately when a controller is disabled.
    pub fn hide_pointer(&self) {
        #[cfg(windows)]
        self.native.hide_pointer();
    }
    pub async fn execute(
        &self,
        action: ComputerAction,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        action.validate()?;
        ensure!(!*cancel.borrow(), "Computer action cancelled");
        #[cfg(windows)]
        {
            self.native.execute(action, cancel).await
        }
        #[cfg(not(windows))]
        {
            let _ = (action, cancel);
            bail!("Native Computer Use currently supports Windows only")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actions_require_observed_ids_and_bounded_inputs() {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(ComputerAction::Snapshot {
            window_id: "1234".into()
        }
        .validate()
        .is_err());
        for x in [f64::NAN, -1.0, f64::INFINITY] {
            assert!(ComputerAction::Click {
                window_id: id.clone(),
                snapshot_id: id.clone(),
                x,
                y: 0.,
                button: "left".into()
            }
            .validate()
            .is_err());
        }
        assert!(parse_key("CTRL+A").is_ok());
        assert!(parse_key("CTRL+SHIFT+ARROWLEFT").is_ok());
        assert!(parse_key("CTRL+CTRL+A").is_err());
        assert!(parse_key("A+ENTER").is_err());
        assert!(parse_key("WIN+R").is_err());
        assert!(!ComputerAction::Key {
            window_id: id.clone(),
            snapshot_id: id,
            node_id: "n0".into(),
            key: "ENTER".into()
        }
        .is_read_only());
    }
}
