//! MCP server registry (Codex `mcp` surface) stored in outer home.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServer>,
}

pub fn mcp_path(outer_home: &Path) -> PathBuf {
    outer_home.join("mcp.toml")
}

pub fn load_mcp(outer_home: &Path) -> McpConfig {
    let path = mcp_path(outer_home);
    fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_mcp(outer_home: &Path, cfg: &McpConfig) -> Result<PathBuf> {
    fs::create_dir_all(outer_home)?;
    let path = mcp_path(outer_home);
    let text = toml::to_string_pretty(cfg).context("serialize mcp")?;
    fs::write(&path, text)?;
    Ok(path)
}

impl McpConfig {
    pub fn add_stdio(
        &mut self,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) {
        let name = name.into();
        self.servers.retain(|s| s.name != name);
        self.servers.push(McpServer {
            name,
            transport: McpTransport::Stdio {
                command: command.into(),
                args,
                env: BTreeMap::new(),
            },
            enabled: true,
        });
    }

    pub fn add_http(&mut self, name: impl Into<String>, url: impl Into<String>) {
        let name = name.into();
        self.servers.retain(|s| s.name != name);
        self.servers.push(McpServer {
            name,
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
            enabled: true,
        });
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.servers.len();
        self.servers.retain(|s| s.name != name);
        self.servers.len() != before
    }

    pub fn list_summary(&self, verbose: bool) -> String {
        if self.servers.is_empty() {
            return "MCP servers: (none configured)\nusage: dsh mcp add <name> -- <cmd> [args...]\n       dsh mcp add-http <name> <url>".into();
        }
        let mut lines = vec![format!("MCP servers ({}):", self.servers.len())];
        for s in &self.servers {
            let state = if s.enabled { "on" } else { "off" };
            match &s.transport {
                McpTransport::Stdio { command, args, .. } => {
                    if verbose {
                        lines.push(format!(
                            "  [{}] {}  stdio: {} {}",
                            state,
                            s.name,
                            command,
                            args.join(" ")
                        ));
                    } else {
                        lines.push(format!("  [{}] {}  stdio:{command}", state, s.name));
                    }
                }
                McpTransport::Http { url, .. } => {
                    lines.push(format!("  [{state}] {}  http:{url}", s.name));
                }
            }
        }
        lines.join("\n")
    }
}
