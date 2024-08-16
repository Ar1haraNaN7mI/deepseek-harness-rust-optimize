//! Execpolicy rule evaluation (Codex `execpolicy check`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecDecision {
    Allow,
    Prompt,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRule {
    pub pattern: String,
    pub decision: ExecDecision,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecPolicyFile {
    #[serde(default)]
    pub rules: Vec<ExecRule>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecCheckResult {
    pub command: String,
    pub decision: ExecDecision,
    pub matched_rule: Option<String>,
    pub reason: Option<String>,
}

pub fn load_policy_file(path: &Path) -> Result<ExecPolicyFile> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    if path.extension().and_then(|s| s.to_str()) == Some("json") {
        Ok(serde_json::from_str(&text)?)
    } else {
        Ok(toml::from_str(&text)?)
    }
}

pub fn check_command(policy: &ExecPolicyFile, command: &str) -> ExecCheckResult {
    let cmd = command.trim();
    // Last matching rule wins (strictest stacked evaluation like Codex preview).
    let mut best: Option<&ExecRule> = None;
    for rule in &policy.rules {
        if rule_matches(&rule.pattern, cmd) {
            best = Some(rule);
        }
    }
    match best {
        Some(r) => ExecCheckResult {
            command: cmd.into(),
            decision: r.decision,
            matched_rule: Some(r.pattern.clone()),
            reason: r.reason.clone(),
        },
        None => ExecCheckResult {
            command: cmd.into(),
            decision: ExecDecision::Allow,
            matched_rule: None,
            reason: Some("no matching rule — default allow".into()),
        },
    }
}

fn rule_matches(pattern: &str, command: &str) -> bool {
    let p = pattern.trim();
    if p == "*" || p == "**" {
        return true;
    }
    if let Some(prefix) = p.strip_suffix('*') {
        return command.starts_with(prefix);
    }
    if p.starts_with('^') {
        // simple contains regex-ish: ^foo means starts with foo after stripping
        return command.starts_with(&p[1..]);
    }
    command.contains(p) || command == p
}

pub fn merge_policies(files: &[ExecPolicyFile]) -> ExecPolicyFile {
    let mut rules = Vec::new();
    for f in files {
        rules.extend(f.rules.clone());
    }
    ExecPolicyFile { rules }
}

/// Pick the strictest decision among results (Deny > Prompt > Allow).
pub fn strictest(results: &[ExecCheckResult]) -> ExecDecision {
    let mut d = ExecDecision::Allow;
    for r in results {
        match r.decision {
            ExecDecision::Deny => return ExecDecision::Deny,
            ExecDecision::Prompt => d = ExecDecision::Prompt,
            ExecDecision::Allow => {}
        }
    }
    d
}
