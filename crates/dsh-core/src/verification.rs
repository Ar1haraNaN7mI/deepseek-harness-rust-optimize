//! Deterministic goal verification for durable task completion.
//!
//! Verification criteria are intentionally declarative so a supervisor can
//! replay the result without invoking the model or bypassing tool policy.
//! Supported forms are:
//! `file_exists:path`, `dir_exists:path`, `contains:path::text`,
//! `not_contains:path::text`, `session_contains:text`, and
//! `assistant_contains:text`.

use crate::Session;
use dsh_protocol::GoalSpec;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct VerificationCheck {
    pub criterion: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    pub passed: bool,
    pub checks: Vec<VerificationCheck>,
}

impl VerificationReport {
    pub fn skipped() -> Self {
        Self {
            passed: true,
            checks: Vec::new(),
        }
    }
}

pub fn verify_goal(
    workspace_root: &Path,
    goal: &GoalSpec,
    session: &Session,
) -> VerificationReport {
    let criteria: Vec<String> = goal
        .verification
        .iter()
        .map(|criterion| criterion.trim())
        .filter(|criterion| !criterion.is_empty())
        .map(str::to_string)
        .collect();
    if criteria.is_empty() {
        return VerificationReport::skipped();
    }

    let transcript = session.transcript_lines().join("\n");
    let mut checks = Vec::with_capacity(criteria.len());
    for criterion in criteria {
        checks.push(evaluate_criterion(workspace_root, &transcript, &criterion));
    }
    let passed = checks.iter().all(|check| check.passed);
    VerificationReport { passed, checks }
}

fn evaluate_criterion(
    workspace_root: &Path,
    transcript: &str,
    criterion: &str,
) -> VerificationCheck {
    let (passed, detail) = if let Some(path) = criterion.strip_prefix("file_exists:") {
        match resolve_relative(workspace_root, path) {
            Ok(path) => (path.is_file(), format!("file {}", path.display())),
            Err(err) => (false, err),
        }
    } else if let Some(path) = criterion.strip_prefix("dir_exists:") {
        match resolve_relative(workspace_root, path) {
            Ok(path) => (path.is_dir(), format!("directory {}", path.display())),
            Err(err) => (false, err),
        }
    } else if let Some(spec) = criterion.strip_prefix("contains:") {
        match split_path_text(spec).and_then(|(path, text)| {
            Ok((resolve_relative(workspace_root, path)?, text.to_string()))
        }) {
            Ok((path, text)) => match std::fs::read_to_string(&path) {
                Ok(content) => (
                    content.contains(&text),
                    format!("{} contains requested text", path.display()),
                ),
                Err(err) => (false, format!("read {}: {err}", path.display())),
            },
            Err(err) => (false, err),
        }
    } else if let Some(spec) = criterion.strip_prefix("not_contains:") {
        match split_path_text(spec).and_then(|(path, text)| {
            Ok((resolve_relative(workspace_root, path)?, text.to_string()))
        }) {
            Ok((path, text)) => match std::fs::read_to_string(&path) {
                Ok(content) => (
                    !content.contains(&text),
                    format!("{} does not contain requested text", path.display()),
                ),
                Err(err) => (false, format!("read {}: {err}", path.display())),
            },
            Err(err) => (false, err),
        }
    } else if let Some(text) = criterion.strip_prefix("session_contains:") {
        (
            transcript.contains(text),
            "session transcript contains requested text".into(),
        )
    } else if let Some(text) = criterion.strip_prefix("assistant_contains:") {
        let assistant_text = transcript
            .lines()
            .filter(|line| line.starts_with("Assistant: "))
            .collect::<Vec<_>>()
            .join("\n");
        (
            assistant_text.contains(text),
            "assistant transcript contains requested text".into(),
        )
    } else {
        (
            false,
            "unknown verification syntax; use file_exists:, dir_exists:, contains:, not_contains:, session_contains:, or assistant_contains:".into(),
        )
    };

    VerificationCheck {
        criterion: criterion.to_string(),
        passed,
        detail,
    }
}

fn split_path_text(spec: &str) -> Result<(&str, &str), String> {
    let Some((path, text)) = spec.split_once("::") else {
        return Err("expected `path::text`".into());
    };
    if path.trim().is_empty() {
        return Err("verification path must not be empty".into());
    }
    Ok((path.trim(), text))
}

fn resolve_relative(workspace_root: &Path, raw: &str) -> Result<PathBuf, String> {
    let relative = Path::new(raw.trim());
    if relative.as_os_str().is_empty() {
        return Err("verification path must not be empty".into());
    }
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("verification paths must be relative to the workspace".into());
    }
    Ok(workspace_root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_protocol::GoalSpec;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn verifies_files_and_transcript_without_running_commands() {
        let root = std::env::temp_dir().join(format!("dsh-verify-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("result.txt"), "done: yes\n").expect("file");
        let mut goal = GoalSpec::new("verify");
        goal.verification = vec![
            "file_exists:result.txt".into(),
            "contains:result.txt::done: yes".into(),
            "assistant_contains:done".into(),
        ];
        let mut session = Session::new();
        session.append(crate::session::SessionEvent::AssistantMessage {
            id: Uuid::new_v4().to_string(),
            text: "done".into(),
            reasoning: None,
            at: chrono::Utc::now(),
        });
        let report = verify_goal(&root, &goal, &session);
        assert!(report.passed, "{report:?}");
        assert_eq!(report.checks.len(), 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unsafe_and_unknown_criteria() {
        let root = std::env::temp_dir();
        let mut goal = GoalSpec::new("verify");
        goal.verification = vec!["file_exists:../secret".into(), "run:echo nope".into()];
        let report = verify_goal(&root, &goal, &Session::new());
        assert!(!report.passed);
        assert_eq!(report.checks.len(), 2);
    }
}
