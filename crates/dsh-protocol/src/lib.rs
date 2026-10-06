//! Stable, transport-neutral protocol models.
//!
//! The protocol crate deliberately contains data contracts only.  Execution,
//! persistence and policy live in `dsh-core`; TUI, CLI, App Server and MCP
//! adapters can all depend on these versioned envelopes without duplicating
//! their own event shapes.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Increment when the wire shape of an event envelope changes incompatibly.
pub const PROTOCOL_VERSION: u16 = 1;

/// Increment when the durable task/event schema changes and needs migration.
pub const SCHEMA_VERSION: u16 = 1;

/// Versioned cloud artifact contract shared by local and remote providers.
pub const CLOUD_ARTIFACT_SCHEMA_VERSION: u16 = 1;

/// Broad source classification used for audit and routing.  It is a string in
/// the JSON representation so future clients can forward unknown values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    #[default]
    Core,
    User,
    Model,
    Tool,
    Scheduler,
    System,
    External,
    /// A source introduced by a newer producer.  Keeping the envelope
    /// readable is more useful than rejecting the whole event stream when a
    /// client is upgraded independently of the server.
    #[serde(other)]
    Unknown,
}

/// Versioned event envelope shared by all local transports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u16,
    /// Assigned by the event store.  Zero means the event has not been
    /// persisted yet.
    #[serde(default)]
    pub sequence: u64,
    #[serde(default = "new_id")]
    pub event_id: String,
    #[serde(default)]
    pub event_type: String,
    #[serde(default)]
    pub source: EventSource,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
    #[serde(default = "now_utc")]
    pub occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub payload: Value,
}

fn default_schema_version() -> u16 {
    SCHEMA_VERSION
}

fn default_protocol_version() -> u16 {
    PROTOCOL_VERSION
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

impl EventEnvelope {
    pub fn new(event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            sequence: 0,
            event_id: new_id(),
            event_type: event_type.into(),
            source: EventSource::Core,
            task_id: None,
            run_id: None,
            turn_id: None,
            step_id: None,
            causation_id: None,
            occurred_at: Utc::now(),
            payload,
        }
    }

    pub fn from_serializable<T: Serialize>(
        event_type: impl Into<String>,
        payload: &T,
    ) -> serde_json::Result<Self> {
        Ok(Self::new(event_type, serde_json::to_value(payload)?))
    }

    pub fn payload_as<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_value(self.payload.clone())
    }

    /// Validate the fields that are required before an envelope can be
    /// persisted.  Deserialization remains permissive for old logs, while
    /// writers get a precise error instead of creating an unqueryable event.
    pub fn validate(&self) -> Result<(), String> {
        if self.event_type.trim().is_empty() {
            return Err("event_type must not be empty".into());
        }
        if self.protocol_version == 0 {
            return Err("protocol_version must be greater than zero".into());
        }
        if self.schema_version == 0 {
            return Err("schema_version must be greater than zero".into());
        }
        Ok(())
    }

    pub fn with_source(mut self, source: EventSource) -> Self {
        self.source = source;
        self
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_run(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_turn(mut self, turn_id: impl Into<String>) -> Self {
        self.turn_id = Some(turn_id.into());
        self
    }

    pub fn with_step(mut self, step_id: impl Into<String>) -> Self {
        self.step_id = Some(step_id.into());
        self
    }

    pub fn caused_by(mut self, event_id: impl Into<String>) -> Self {
        self.causation_id = Some(event_id.into());
        self
    }
}

/// Auditable patch artifact exchanged by local and remote cloud providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CloudArtifact {
    #[serde(default = "default_cloud_artifact_schema_version")]
    pub schema_version: u16,
    #[serde(default)]
    pub id: String,
    #[serde(default = "new_cloud_artifact_created_at")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub workspace: String,
}

impl CloudArtifact {
    /// Check the integrity metadata without imposing provider-specific path
    /// rules.  Providers can use this before accepting or applying an
    /// artifact, while callers remain free to choose their hash algorithm
    /// policy for future schema versions.
    pub fn validate_metadata(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("artifact id must not be empty".into());
        }
        if self.patch.trim().is_empty() {
            return Err("artifact patch must not be empty".into());
        }
        if self.sha256.trim().is_empty() {
            return Err("artifact sha256 must not be empty".into());
        }
        if self.sha256.len() != 64 || !self.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err("artifact sha256 must be a 64-character hexadecimal digest".into());
        }
        Ok(())
    }
}

fn default_cloud_artifact_schema_version() -> u16 {
    CLOUD_ARTIFACT_SCHEMA_VERSION
}

fn new_cloud_artifact_created_at() -> DateTime<Utc> {
    Utc::now()
}

/// Lifecycle state for a durable task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Queued,
    Running,
    WaitingApproval,
    WaitingEvent,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// Lifecycle state for one execution attempt of a task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Starting,
    Running,
    WaitingApproval,
    WaitingEvent,
    Paused,
    Completed,
    Failed,
    Cancelled,
    /// State value unknown to this client version.
    #[serde(other)]
    Unknown,
}

/// Resource and verification contract for a long-running goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalSpec {
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub max_steps: Option<u64>,
    #[serde(default)]
    pub deadline: Option<DateTime<Utc>>,
}

/// Retry contract captured on a task when it is created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryPolicy {
    #[serde(default = "default_retry_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_retry_backoff_secs")]
    pub backoff_secs: u64,
    #[serde(default = "default_retry_max_backoff_secs")]
    pub max_backoff_secs: u64,
}

fn default_retry_max_attempts() -> u32 {
    3
}

fn default_retry_backoff_secs() -> u64 {
    5
}

fn default_retry_max_backoff_secs() -> u64 {
    300
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_retry_max_attempts(),
            backoff_secs: default_retry_backoff_secs(),
            max_backoff_secs: default_retry_max_backoff_secs(),
        }
    }
}

impl RetryPolicy {
    pub fn normalized(mut self) -> Self {
        self.max_attempts = self.max_attempts.max(1);
        self.max_backoff_secs = self.max_backoff_secs.max(self.backoff_secs);
        self
    }

    pub fn delay_secs(&self, attempt: u32) -> u64 {
        let exponent = attempt.saturating_sub(1).min(20);
        self.backoff_secs
            .saturating_mul(1_u64 << exponent)
            .min(self.max_backoff_secs)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_attempts == 0 {
            return Err("retry max_attempts must be greater than zero".into());
        }
        if self.max_backoff_secs < self.backoff_secs {
            return Err("retry max_backoff_secs must be >= backoff_secs".into());
        }
        Ok(())
    }
}

impl GoalSpec {
    pub fn new(outcome: impl Into<String>) -> Self {
        Self {
            outcome: outcome.into(),
            constraints: Vec::new(),
            verification: Vec::new(),
            max_steps: None,
            deadline: None,
        }
    }

    /// Validate the minimum contract without making legacy `/goal` strings
    /// unusable.  A stricter verifier can require `verification` separately.
    pub fn validate(&self) -> Result<(), String> {
        if self.outcome.trim().is_empty() {
            return Err("goal outcome must not be empty".into());
        }
        if self.max_steps == Some(0) {
            return Err("goal max_steps must be greater than zero".into());
        }
        Ok(())
    }

    pub fn has_verification(&self) -> bool {
        self.verification.iter().any(|v| !v.trim().is_empty())
    }
}

/// Durable task identity and current state.  Mutable execution details are
/// recorded as events/checkpoints instead of being silently overwritten.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    #[serde(default = "new_id")]
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub goal: GoalSpec,
    #[serde(default)]
    pub state: TaskState,
    #[serde(default = "now_utc")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "now_utc")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub policy_snapshot: Value,
    #[serde(default)]
    pub retry_policy: RetryPolicy,
}

impl TaskRecord {
    pub fn new(goal: GoalSpec) -> Result<Self, String> {
        goal.validate()?;
        let now = Utc::now();
        Ok(Self {
            id: new_id(),
            session_id: None,
            goal,
            state: TaskState::Queued,
            created_at: now,
            updated_at: now,
            parent_task_id: None,
            policy_snapshot: Value::Object(serde_json::Map::new()),
            retry_policy: RetryPolicy::default(),
        })
    }

    pub fn new_with_retry_policy(
        goal: GoalSpec,
        retry_policy: RetryPolicy,
    ) -> Result<Self, String> {
        let mut task = Self::new(goal)?;
        task.retry_policy = retry_policy.normalized();
        Ok(task)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("task id must not be empty".into());
        }
        self.goal.validate()?;
        self.retry_policy.validate()?;
        if self
            .session_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("task session_id must not be empty".into());
        }
        Ok(())
    }
}

/// One worker attempt.  A task may have multiple runs after retry/recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    #[serde(default = "new_id")]
    pub id: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    #[serde(default)]
    pub state: RunState,
    #[serde(default = "now_utc")]
    pub started_at: DateTime<Utc>,
    #[serde(default = "now_utc")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub worker_id: Option<String>,
    #[serde(default)]
    pub heartbeat_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub checkpoint_id: Option<String>,
}

impl RunRecord {
    pub fn new(task_id: impl Into<String>, attempt: u32) -> Self {
        let now = Utc::now();
        Self {
            id: new_id(),
            task_id: task_id.into(),
            attempt: attempt.max(1),
            state: RunState::Starting,
            started_at: now,
            updated_at: now,
            worker_id: None,
            heartbeat_at: None,
            checkpoint_id: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("run id must not be empty".into());
        }
        if self.task_id.trim().is_empty() {
            return Err("run task_id must not be empty".into());
        }
        if self.attempt == 0 {
            return Err("run attempt must be greater than zero".into());
        }
        if self
            .worker_id
            .as_deref()
            .is_some_and(|worker| worker.trim().is_empty())
        {
            return Err("run worker_id must not be empty".into());
        }
        Ok(())
    }
}

fn default_attempt() -> u32 {
    1
}

/// Replay point used before/after side-effecting tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckpointRecord {
    #[serde(default = "new_id")]
    pub id: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub event_sequence: u64,
    #[serde(default)]
    pub step_index: u32,
    #[serde(default)]
    pub session_event_count: u64,
    #[serde(default)]
    pub workspace_digest: Option<String>,
    #[serde(default)]
    pub pending_approval_ids: Vec<String>,
    #[serde(default = "now_utc")]
    pub created_at: DateTime<Utc>,
}

impl CheckpointRecord {
    pub fn new(task_id: impl Into<String>, run_id: impl Into<String>, event_sequence: u64) -> Self {
        Self {
            id: new_id(),
            task_id: task_id.into(),
            run_id: run_id.into(),
            event_sequence,
            step_index: 0,
            session_event_count: 0,
            workspace_digest: None,
            pending_approval_ids: Vec::new(),
            created_at: Utc::now(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("checkpoint id must not be empty".into());
        }
        if self.task_id.trim().is_empty() {
            return Err("checkpoint task_id must not be empty".into());
        }
        if self.run_id.trim().is_empty() {
            return Err("checkpoint run_id must not be empty".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_and_keeps_correlations() {
        let event = EventEnvelope::new("task.created", serde_json::json!({"ok": true}))
            .with_task("task-1")
            .with_run("run-1")
            .with_source(EventSource::Scheduler);
        let encoded = serde_json::to_string(&event).expect("serialize");
        let decoded: EventEnvelope = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.event_type, "task.created");
        assert_eq!(decoded.task_id.as_deref(), Some("task-1"));
        assert_eq!(decoded.run_id.as_deref(), Some("run-1"));
        assert_eq!(decoded.source, EventSource::Scheduler);
        assert_eq!(decoded.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn goal_validation_rejects_empty_outcome_but_allows_legacy_constraints() {
        assert!(GoalSpec::new(" ").validate().is_err());
        assert!(GoalSpec::new("build it").validate().is_ok());
        assert!(!GoalSpec::new("build it").has_verification());
    }

    #[test]
    fn retry_policy_normalizes_and_caps_exponential_backoff() {
        let policy = RetryPolicy {
            max_attempts: 0,
            backoff_secs: 3,
            max_backoff_secs: 5,
        }
        .normalized();
        assert_eq!(policy.max_attempts, 1);
        assert_eq!(policy.max_backoff_secs, 5);
        assert_eq!(policy.delay_secs(1), 3);
        assert_eq!(policy.delay_secs(2), 5);
        assert_eq!(policy.delay_secs(8), 5);
    }

    #[test]
    fn retry_policy_legacy_json_uses_defaults() {
        let task: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": "task-1",
            "goal": {"outcome": "legacy"},
            "state": "queued",
            "created_at": Utc::now(),
            "updated_at": Utc::now()
        }))
        .expect("legacy task");
        assert_eq!(task.retry_policy, RetryPolicy::default());
    }

    #[test]
    fn legacy_event_defaults_timestamp_and_unknown_source() {
        let event: EventEnvelope = serde_json::from_value(serde_json::json!({
            "event_type": "legacy",
            "source": "future_source",
            "payload": {}
        }))
        .expect("legacy event");
        assert_eq!(event.event_type, "legacy");
        assert_eq!(event.source, EventSource::Unknown);
        assert!(event.occurred_at <= Utc::now());
        assert!(event.validate().is_ok());
    }

    #[test]
    fn artifact_metadata_validation_rejects_malformed_digest() {
        let mut artifact = CloudArtifact {
            schema_version: CLOUD_ARTIFACT_SCHEMA_VERSION,
            id: "art-1".into(),
            created_at: Utc::now(),
            source: "test".into(),
            patch: "patch".into(),
            sha256: "bad".into(),
            workspace: String::new(),
        };
        assert!(artifact.validate_metadata().is_err());
        artifact.sha256 = "0".repeat(64);
        assert!(artifact.validate_metadata().is_ok());
    }

    #[test]
    fn run_and_checkpoint_validation_rejects_missing_correlations() {
        let mut run = RunRecord::new("task-1", 1);
        assert!(run.validate().is_ok());
        run.task_id.clear();
        assert!(run.validate().is_err());
        let checkpoint = CheckpointRecord::new("task-1", "run-1", 1);
        assert!(checkpoint.validate().is_ok());
    }
}
