//! Codex-like permission presets for what the agent may do without asking.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Browse/read only — writes, shell, and installs require switching mode.
    ReadOnly,
    /// Default: read/edit/run in the workspace + outer layer (Codex "Auto").
    #[default]
    Auto,
    /// Full access across machine/network tooling (use sparingly).
    FullAccess,
}

impl PermissionMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "read-only" | "readonly" | "ro" | "read" => Some(Self::ReadOnly),
            "auto" | "workspace" | "default" => Some(Self::Auto),
            "full" | "full-access" | "fullaccess" | "danger" | "yolo" => Some(Self::FullAccess),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Auto => "auto",
            Self::FullAccess => "full-access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Browse files and search only; edits/shell blocked",
            Self::Auto => "Read/edit/run tools in workspace + outer layer",
            Self::FullAccess => "Unrestricted tool use (still PathGuard-denies core crates)",
        }
    }

    /// Whether a tool name is allowed under this preset.
    pub fn allows_tool(self, name: &str) -> bool {
        let n = name.to_lowercase();
        match self {
            Self::FullAccess => true,
            Self::Auto => true,
            Self::ReadOnly => {
                matches!(
                    n.as_str(),
                    "read_file"
                        | "list_dir"
                        | "glob"
                        | "grep"
                        | "web_fetch"
                        | "skill_list"
                        | "skill_search"
                        | "skill_recommend"
                        | "skill_load"
                        | "plugin_list"
                        | "plugin_search"
                        | "todo_read"
                        | "learn_recall"
                        | "learn_weights"
                ) || n.starts_with("skill_")
                    || (n.starts_with("plugin.") && !n.contains("install"))
            }
        }
    }

    pub fn deny_reason(self, name: &str) -> String {
        format!(
            "permission mode `{}` blocks tool `{name}`. Switch with /permissions auto|full-access",
            self.label()
        )
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

pub const PERMISSION_HELP: &str = "\
permissions presets (Codex-aligned):
  read-only     — browse only
  auto          — workspace read/edit/run (default)
  full-access   — unrestricted tools (PathGuard still protects core)
Usage: /permissions [read-only|auto|full-access]
Aliases: /approvals
";
