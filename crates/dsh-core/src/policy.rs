//! Codex-aligned approval policy + sandbox mode.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::permissions::PermissionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// Never pause for human approval (automation / CI).
    Never,
    /// Pause before shell / write / install tools (Codex on-request).
    #[default]
    OnRequest,
    /// Pause for any non-read tool (Codex untrusted).
    Untrusted,
}

impl ApprovalPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "never" | "no" | "off" => Some(Self::Never),
            "on-request" | "on_request" | "request" | "auto" => Some(Self::OnRequest),
            "untrusted" | "strict" | "always" => Some(Self::Untrusted),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnRequest => "on-request",
            Self::Untrusted => "untrusted",
        }
    }

    /// Whether this tool should pause for human approval under the policy.
    pub fn requires_approval(self, tool: &str) -> bool {
        let n = tool.to_lowercase();
        let is_read = matches!(
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
        ) || n.starts_with("skill_");
        match self {
            Self::Never => false,
            Self::OnRequest => {
                matches!(
                    n.as_str(),
                    "shell"
                        | "write_file"
                        | "edit_file"
                        | "apply_patch"
                        | "plugin_install"
                        | "plugin_unload"
                ) || n.contains("install")
            }
            Self::Untrusted => !is_read,
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "read-only" | "readonly" | "ro" => Some(Self::ReadOnly),
            "workspace-write" | "workspace_write" | "workspace" | "write" => {
                Some(Self::WorkspaceWrite)
            }
            "danger-full-access" | "danger_full_access" | "full" | "danger" | "yolo" => {
                Some(Self::DangerFullAccess)
            }
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn to_permission(self) -> PermissionMode {
        match self {
            Self::ReadOnly => PermissionMode::ReadOnly,
            Self::WorkspaceWrite => PermissionMode::Auto,
            Self::DangerFullAccess => PermissionMode::FullAccess,
        }
    }
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

pub const APPROVAL_HELP: &str = "\
approval policy (Codex -a):
  never        — never pause
  on-request   — pause before shell/write (default)
  untrusted    — pause for any non-read tool
";

pub const SANDBOX_HELP: &str = "\
sandbox mode (Codex -s):
  read-only            — browse only
  workspace-write      — read/edit/run in workspace (default)
  danger-full-access   — unrestricted tools (PathGuard still protects core)
";
