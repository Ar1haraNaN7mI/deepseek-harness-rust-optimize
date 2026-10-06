//! Core kernel: session log, agent loop, system prompt, CTM thought, self-learning.
//! This layer is immutable at runtime — agents may only mutate the outer layer.

mod agent_loop;
pub mod approvals;
pub mod bg;
pub mod builtin_tools;
mod config;
pub mod credentials;
pub mod ctm;
pub mod execpolicy;
pub mod features;
pub mod hooks;
pub mod learn;
pub mod learn_tools;
pub mod mcp;
pub mod model_profile;
pub mod permissions;
pub mod policy;
pub mod scheduler;
mod session;
pub mod settings;
mod startup_control;
mod startup_profile;
mod system_prompt;
pub mod task;
pub mod verification;
pub mod worker;

pub use agent_loop::{AgentEvent, AgentEventContext, AgentHandle, AgentLoop};
pub use approvals::{ApprovalQueue, ApprovalScope, DeniedAction, PendingApproval};
pub use bg::{BgJobInfo, BgTerminals};
pub use builtin_tools::register_builtin_tools;
pub use config::{AppConfig, StartupSection};
pub use credentials::{
    api_key_status, clear_api_key, load_api_key, resolve_api_key, resolve_api_key_for_backend,
    save_api_key,
};
pub use ctm::{ContinuousThought, CtmConfig, ThoughtSnapshot};
pub use dsh_protocol::{
    CheckpointRecord, EventEnvelope, EventSource, GoalSpec, RetryPolicy, RunRecord, RunState,
    TaskRecord, TaskState, PROTOCOL_VERSION, SCHEMA_VERSION,
};
pub use execpolicy::{
    check_command, load_policy_file, merge_policies, strictest, ExecCheckResult, ExecDecision,
    ExecPolicyFile, ExecRule,
};
pub use features::{load_features, save_features, FeatureFlags, DEFAULT_FLAGS};
pub use hooks::{hooks_path, load_hooks, save_hooks, HookEntry, HooksConfig};
pub use learn::LearnStore;
pub use learn_tools::register_learn_tools;
pub use mcp::{load_mcp, save_mcp, McpConfig, McpServer, McpTransport};
pub use model_profile::{
    compact_tool_description, compact_tool_result, compact_tool_schema, infer_parameter_count_b,
    select_tools_for_query, ModelOptimizationConfig, ModelOptimizationMode, ModelPolicy,
    ModelProfile, SMALL_MODEL_THRESHOLD_B,
};
pub use permissions::{PermissionMode, PERMISSION_HELP};
pub use policy::{ApprovalPolicy, SandboxMode, APPROVAL_HELP, SANDBOX_HELP};
pub use scheduler::{SchedulerSnapshot, TaskScheduler};
pub use session::{Session, SessionEvent, SessionStore, SESSION_SCHEMA_VERSION};
pub use settings::{
    load_settings, personality_prompt, save_settings, SessionSettings, PERSONALITIES, PETS,
    STATUSLINE_FIELDS, THEMES, TITLE_FIELDS,
};
pub use startup_control::{set_next_startup, take_next_startup};
pub use startup_profile::{load_startup_profile, save_startup_profile, StartupProfile};
pub use system_prompt::{SystemPromptBuilder, SECURITY_RESEARCH_PROMPT};
pub use task::{
    EventStore, JsonlEventStore, TaskStore, TaskStoreSnapshot, TASK_SNAPSHOT_SCHEMA_VERSION,
};
pub use verification::{verify_goal, VerificationCheck, VerificationReport};
pub use worker::{RunLease, WorkerSupervisor};

use anyhow::Result;
use dsh_llm::DeepSeekClient;
use dsh_plugin::PluginRegistry;
use dsh_skill::SkillCatalog;
use dsh_tools::{DefaultPipeline, ToolRegistry};
use parking_lot::{Mutex, RwLock};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Notify};

/// Shared runtime state owned by the core kernel.
pub struct Runtime {
    pub config: AppConfig,
    pub workspace_root: PathBuf,
    pub outer_home: PathBuf,
    pub workspace_outer: PathBuf,
    pub llm: Arc<DeepSeekClient>,
    pub tools: Arc<ToolRegistry>,
    pub pipeline: Arc<DefaultPipeline>,
    pub sessions: Arc<SessionStore>,
    pub prompt: Arc<RwLock<SystemPromptBuilder>>,
    pub learn: Arc<LearnStore>,
    pub ctm: Arc<ContinuousThought>,
    pub skills: RwLock<Option<Arc<SkillCatalog>>>,
    pub plugins: RwLock<Option<Arc<PluginRegistry>>>,
    pub permissions: RwLock<PermissionMode>,
    pub settings: RwLock<SessionSettings>,
    pub features: RwLock<FeatureFlags>,
    pub mcp: RwLock<McpConfig>,
    pub hooks: RwLock<HooksConfig>,
    pub bg: Arc<BgTerminals>,
    pub approvals: Arc<ApprovalQueue>,
    /// Versioned append-only lifecycle events shared by all frontends.
    pub events: Arc<JsonlEventStore>,
    /// In-process wake-up for long-poll event consumers. Cross-process
    /// writers still use the bounded polling fallback in the app-server.
    pub event_notify: Arc<Notify>,
    /// In-memory task projection rebuilt from the durable event stream.
    pub tasks: Arc<TaskStore>,
    /// Worker lease manager for long-running task runs.
    pub workers: Arc<WorkerSupervisor>,
    /// Background durable task scheduler.
    pub scheduler: Arc<TaskScheduler>,
    /// Live agent controls keyed by durable task id.
    pub active_agents: Mutex<HashMap<String, AgentHandle>>,
    /// Short-lived reservations covering the gap before a run record exists.
    pub task_reservations: Mutex<HashSet<String>>,
}

impl Runtime {
    pub fn bootstrap(
        config: AppConfig,
        workspace_root: PathBuf,
        llm: DeepSeekClient,
        tools: Arc<ToolRegistry>,
    ) -> Result<Arc<Self>> {
        let outer_home = config.resolve_outer_home()?;
        let workspace_outer = workspace_root.join(&config.paths.workspace_outer);
        std::fs::create_dir_all(&outer_home)?;
        std::fs::create_dir_all(outer_home.join("skills"))?;
        std::fs::create_dir_all(outer_home.join("plugins"))?;
        std::fs::create_dir_all(outer_home.join("meta"))?;
        std::fs::create_dir_all(outer_home.join("learn"))?;
        std::fs::create_dir_all(outer_home.join("sessions"))?;
        std::fs::create_dir_all(outer_home.join("rules"))?;
        std::fs::create_dir_all(&workspace_outer)?;
        std::fs::create_dir_all(workspace_outer.join("skills"))?;
        std::fs::create_dir_all(workspace_outer.join("plugins"))?;

        let pipeline = Arc::new(DefaultPipeline::new(tools.clone()));
        let sessions = Arc::new(SessionStore::with_dir(outer_home.join("sessions")));
        let events = Arc::new(JsonlEventStore::open(
            outer_home.join("events/events.jsonl"),
        )?);
        if let Err(err) = recover_unresolved_approvals(&events) {
            tracing::warn!(error = %err, "failed to recover approval events");
        }
        let tasks = Arc::new(TaskStore::open_with_snapshot(
            events.clone(),
            outer_home.join("events/tasks.snapshot.json"),
        )?);
        let event_notify = Arc::new(Notify::new());
        tasks.set_event_notifier(event_notify.clone());
        let workers = Arc::new(WorkerSupervisor::local_default(
            tasks.clone(),
            std::time::Duration::from_secs(config.agent.worker_heartbeat_secs.max(1)),
            std::time::Duration::from_secs(config.agent.worker_stale_secs.max(2)),
        ));
        if let Err(err) = workers.recover_stale_runs() {
            tracing::warn!(error = %err, "failed to recover stale worker runs");
        }
        let scheduler = Arc::new(TaskScheduler::new(
            tasks.clone(),
            config.agent.scheduler_enabled,
            std::time::Duration::from_secs(config.agent.scheduler_poll_secs.max(1)),
            config.agent.scheduler_max_concurrency.max(1),
        ));
        let prompt = Arc::new(RwLock::new(SystemPromptBuilder::default_sections()));
        let learn = Arc::new(LearnStore::open(&outer_home));
        let ctm = Arc::new(ContinuousThought::new(config.ctm.clone(), learn.clone()));
        let settings = load_settings(&outer_home);
        let permissions = settings.permissions;
        let features = load_features(&outer_home);
        let mcp = load_mcp(&outer_home);
        let hooks = load_hooks(&outer_home);

        let runtime = Arc::new(Self {
            config,
            workspace_root,
            outer_home,
            workspace_outer,
            llm: Arc::new(llm),
            tools,
            pipeline,
            sessions,
            prompt,
            learn,
            ctm,
            skills: RwLock::new(None),
            plugins: RwLock::new(None),
            permissions: RwLock::new(permissions),
            settings: RwLock::new(settings.clone()),
            features: RwLock::new(features),
            mcp: RwLock::new(mcp),
            hooks: RwLock::new(hooks),
            bg: Arc::new(BgTerminals::new()),
            approvals: Arc::new(ApprovalQueue::new()),
            events,
            event_notify,
            tasks,
            workers,
            scheduler,
            active_agents: Mutex::new(HashMap::new()),
            task_reservations: Mutex::new(HashSet::new()),
        });
        runtime.scheduler.attach_runtime(&runtime);

        runtime
            .prompt
            .write()
            .set_security_research_mode(settings.security_research_mode);

        if let Some(model) = settings.model {
            runtime.llm.set_model(model);
        }
        if let Some(thinking) = settings.thinking {
            runtime.llm.set_thinking(thinking);
        }
        if let Some(p) = &settings.personality {
            runtime
                .prompt
                .write()
                .set_section("personality", personality_prompt(p).to_string());
        }

        runtime.sync_model_optimization();

        // Align permission mode with sandbox setting when sandbox was persisted.
        let _ = settings.sandbox;

        Ok(runtime)
    }

    /// Start the local scheduler after tools/plugins have been attached.
    pub fn start_scheduler(self: &Arc<Self>) {
        self.scheduler.start();
    }

    pub fn stop_scheduler(&self) {
        self.scheduler.stop();
    }

    /// Asynchronously stop the scheduler and wait until its polling task has
    /// exited. This is useful for service shutdown/restart boundaries where a
    /// synchronous signal alone could overlap a newly started loop.
    pub async fn stop_scheduler_and_wait(&self) {
        self.scheduler.stop_and_wait().await;
    }

    pub fn register_agent_handle(&self, task_id: impl Into<String>, handle: AgentHandle) {
        self.active_agents.lock().insert(task_id.into(), handle);
    }

    pub fn reserve_task(&self, task_id: impl Into<String>) {
        self.task_reservations.lock().insert(task_id.into());
    }

    pub fn release_task_reservation(&self, task_id: &str) {
        self.task_reservations.lock().remove(task_id);
    }

    pub fn is_task_reserved(&self, task_id: &str) -> bool {
        self.task_reservations.lock().contains(task_id)
    }

    pub fn clear_agent_handle(&self, task_id: &str) {
        self.active_agents.lock().remove(task_id);
    }

    pub fn cancel_active_task(&self, task_id: &str) -> bool {
        let Some(handle) = self.active_agents.lock().get(task_id).cloned() else {
            return false;
        };
        handle.cancel();
        true
    }

    pub fn pause_active_task(&self, task_id: &str) -> bool {
        let Some(handle) = self.active_agents.lock().get(task_id).cloned() else {
            return false;
        };
        handle.pause();
        true
    }

    pub fn attach_skills(&self, skills: Arc<SkillCatalog>) {
        *self.skills.write() = Some(skills);
    }

    pub fn attach_plugins(&self, plugins: Arc<PluginRegistry>) {
        *self.plugins.write() = Some(plugins);
    }

    pub fn set_permissions(&self, mode: PermissionMode) -> anyhow::Result<()> {
        *self.permissions.write() = mode;
        let mut s = self.settings.write();
        s.permissions = mode;
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn set_approval(&self, policy: ApprovalPolicy) -> anyhow::Result<()> {
        let mut s = self.settings.write();
        s.approval = policy;
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn set_sandbox(&self, mode: SandboxMode) -> anyhow::Result<()> {
        *self.permissions.write() = mode.to_permission();
        let mut s = self.settings.write();
        s.sandbox = mode;
        s.permissions = mode.to_permission();
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn persist_settings(&self) -> anyhow::Result<()> {
        let s = self.settings.read().clone();
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn persist_features(&self) -> anyhow::Result<()> {
        let f = self.features.read().clone();
        save_features(&self.outer_home, &f)?;
        Ok(())
    }

    pub fn persist_mcp(&self) -> anyhow::Result<()> {
        let m = self.mcp.read().clone();
        save_mcp(&self.outer_home, &m)?;
        Ok(())
    }

    pub fn persist_hooks(&self) -> anyhow::Result<()> {
        let h = self.hooks.read().clone();
        save_hooks(&self.outer_home, &h)?;
        Ok(())
    }

    /// Append a protocol event without making callers depend on the concrete
    /// JSONL backend.  Lifecycle paths may intentionally ignore the returned
    /// error after emitting a user-facing error; callers that need a hard
    /// durability boundary should propagate it.
    pub fn record_event(&self, event: EventEnvelope) -> anyhow::Result<EventEnvelope> {
        let persisted = self.events.append(event)?;
        self.event_notify.notify_waiters();
        Ok(persisted)
    }

    pub fn record_system_event<T: serde::Serialize>(
        &self,
        event_type: impl Into<String>,
        payload: &T,
    ) -> anyhow::Result<EventEnvelope> {
        let event =
            EventEnvelope::from_serializable(event_type, payload)?.with_source(EventSource::Core);
        self.record_event(event)
    }

    pub fn set_personality(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut s = self.settings.write();
            s.personality = Some(name.to_string());
        }
        self.prompt
            .write()
            .set_section("personality", personality_prompt(name).to_string());
        self.persist_settings()?;
        Ok(())
    }

    /// Switch the active model and immediately refresh the small-model prompt
    /// section. Persisting the user preference remains the caller's choice,
    /// matching the existing CLI/TUI settings flow.
    pub fn set_model(&self, model: impl Into<String>) {
        self.llm.set_model(model);
        self.sync_model_optimization();
    }

    /// Switch an OpenAI-compatible backend. If the current URL is still the
    /// built-in DeepSeek default, move to the backend's local preset; an
    /// explicitly configured URL is preserved.
    pub fn set_backend(&self, backend: dsh_llm::LlmBackend) {
        let current = self.llm.config();
        let current_url = current.base_url.trim_end_matches('/');
        let current_is_preset = current_url
            == current.backend.default_base_url().trim_end_matches('/')
            || current_url == "https://api.deepseek.com";
        if current_is_preset {
            self.llm.set_base_url(backend.default_base_url());
        }
        self.llm.set_backend(backend);
    }

    /// Compute the active model-size policy from the current model id and
    /// refresh the model-facing prompt section. The request budgets themselves
    /// are read by the agent loop for each turn, so changing `/model` takes
    /// effect without restarting the process.
    pub fn model_profile(&self) -> ModelProfile {
        let mut optimization = self.config.llm.optimization.clone();
        if let Ok(value) = std::env::var("DSH_MODEL_OPTIMIZATION") {
            if let Ok(mode) = value.parse() {
                optimization.mode = mode;
            }
        }
        if let Ok(value) = std::env::var("DSH_MODEL_SIZE_B") {
            if let Ok(size) = value.trim().parse::<f32>() {
                if size.is_finite() && size > 0.0 {
                    optimization.parameter_count_b = Some(size);
                }
            }
        }
        ModelProfile::for_model(self.llm.config().model, &optimization)
    }

    pub fn sync_model_optimization(&self) -> ModelProfile {
        let profile = self.model_profile();
        self.prompt
            .write()
            .set_model_optimization(profile.prompt_section());
        profile
    }

    /// Toggle the model-facing authorized security-research directive. This
    /// never bypasses tool permissions, sandbox checks, approvals, or audit
    /// events.
    pub fn set_security_research_mode(&self, enabled: bool) -> anyhow::Result<()> {
        self.prompt.write().set_security_research_mode(enabled);
        self.settings.write().security_research_mode = enabled;
        self.persist_settings()
    }

    /// Resolve a pending approval from the TUI (y/n).
    pub fn resolve_approval(&self, allow: bool) {
        if let Some(request) = self.approvals.resolve_next_entry(allow) {
            self.record_approval_resolved(
                &request,
                allow,
                if allow { "approved" } else { "denied" },
            );
        }
    }

    /// Resolve a specific pending approval. This is the preferred API for
    /// frontends that can preserve request ids while multiple runs wait.
    pub fn resolve_approval_request(&self, request_id: &str, allow: bool) -> bool {
        let Some(request) = self.approvals.resolve_entry(request_id, allow) else {
            return false;
        };
        self.record_approval_resolved(&request, allow, if allow { "approved" } else { "denied" });
        true
    }

    /// Ask the UI to approve a tool; returns false if denied / cancelled / timed out.
    pub async fn request_tool_approval(
        &self,
        event_tx: &mpsc::Sender<AgentEvent>,
        call_id: &str,
        name: &str,
        summary: &str,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> bool {
        self.request_tool_approval_scoped(
            event_tx,
            call_id,
            name,
            summary,
            ApprovalScope::default(),
            cancel_rx,
        )
        .await
    }

    /// Ask the UI to approve a tool while preserving durable task/run
    /// correlation. Every request is independently resolvable, cancellable,
    /// and auditable in the protocol event stream.
    pub async fn request_tool_approval_scoped(
        &self,
        event_tx: &mpsc::Sender<AgentEvent>,
        call_id: &str,
        name: &str,
        summary: &str,
        scope: ApprovalScope,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> bool {
        let request = PendingApproval::new(
            call_id,
            name,
            summary,
            scope.task_id.as_deref(),
            scope.run_id.as_deref(),
        );
        let request_id = request.request_id.clone();
        let reply = self.approvals.enqueue(request.clone());
        self.record_approval_requested(&request);

        if event_tx
            .send(AgentEvent::ApprovalNeeded {
                request_id: request_id.clone(),
                call_id: call_id.to_string(),
                name: name.to_string(),
                summary: summary.to_string(),
            })
            .await
            .is_err()
        {
            if self.approvals.cancel(&request_id).is_some() {
                self.record_approval_resolved(&request, false, "event_channel_closed");
            }
            return false;
        }

        let outcome: Result<bool, &'static str> = tokio::select! {
            result = reply => Ok(result.unwrap_or(false)),
            _ = wait_for_cancel(cancel_rx) => Err("cancelled"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(
                self.config.agent.approval_timeout_secs.max(1),
            )) => Err("timed_out"),
        };

        match outcome {
            Ok(allow) => {
                self.record_approval_resolved(
                    &request,
                    allow,
                    if allow { "approved" } else { "denied" },
                );
                allow
            }
            Err(reason) => {
                if self.approvals.cancel(&request_id).is_some() {
                    self.record_approval_resolved(&request, false, reason);
                }
                false
            }
        }
    }

    fn record_approval_requested(&self, request: &PendingApproval) {
        let mut event =
            EventEnvelope::new("approval.requested", json!(request)).with_source(EventSource::Core);
        if let Some(task_id) = &request.task_id {
            event = event.with_task(task_id.clone());
        }
        if let Some(run_id) = &request.run_id {
            event = event.with_run(run_id.clone());
        }
        if let Err(err) = self.record_event(event) {
            tracing::warn!(error = %err, request_id = %request.request_id, "failed to persist approval request");
        }
    }

    fn record_approval_resolved(&self, request: &PendingApproval, allow: bool, reason: &str) {
        let source = if matches!(reason, "approved" | "denied") {
            EventSource::User
        } else {
            EventSource::System
        };
        let mut event = EventEnvelope::new(
            "approval.resolved",
            json!({
                "request": request,
                "allow": allow,
                "reason": reason,
            }),
        )
        .with_source(source);
        if let Some(task_id) = &request.task_id {
            event = event.with_task(task_id.clone());
        }
        if let Some(run_id) = &request.run_id {
            event = event.with_run(run_id.clone());
        }
        if let Err(err) = self.record_event(event) {
            tracing::warn!(error = %err, request_id = %request.request_id, "failed to persist approval resolution");
        }
    }
}

async fn wait_for_cancel(cancel_rx: &mut watch::Receiver<bool>) {
    if *cancel_rx.borrow() {
        return;
    }
    loop {
        if cancel_rx.changed().await.is_err() {
            return;
        }
        if *cancel_rx.borrow() {
            return;
        }
    }
}

fn recover_unresolved_approvals(events: &JsonlEventStore) -> anyhow::Result<usize> {
    let mut pending = HashMap::<String, PendingApproval>::new();
    for event in events.read_all()? {
        match event.event_type.as_str() {
            "approval.requested" => {
                if let Ok(request) = event.payload_as::<PendingApproval>() {
                    pending.insert(request.request_id.clone(), request);
                }
            }
            "approval.resolved" | "approval.recovered" => {
                let request = event
                    .payload
                    .get("request")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<PendingApproval>(value).ok());
                if let Some(request) = request {
                    pending.remove(&request.request_id);
                }
            }
            _ => {}
        }
    }

    let mut requests: Vec<_> = pending.into_values().collect();
    requests.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    let mut recovered = 0;
    for request in requests {
        let mut event = EventEnvelope::new(
            "approval.recovered",
            json!({
                "request": &request,
                "allow": false,
                "reason": "runtime_restarted",
            }),
        )
        .with_source(EventSource::Scheduler);
        if let Some(task_id) = &request.task_id {
            event = event.with_task(task_id.clone());
        }
        if let Some(run_id) = &request.run_id {
            event = event.with_run(run_id.clone());
        }
        events.append(event)?;
        recovered += 1;
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_approval_is_recovered_once() {
        let dir =
            std::env::temp_dir().join(format!("dsh-rust-p1-approval-{}", uuid::Uuid::new_v4()));
        let path = dir.join("events.jsonl");
        let events = JsonlEventStore::open(&path).expect("open events");
        let request = PendingApproval::new("call-1", "shell", "{}", Some("task-1"), Some("run-1"));
        events
            .append(EventEnvelope::new("approval.requested", json!(request)))
            .expect("append request");
        assert_eq!(recover_unresolved_approvals(&events).expect("recover"), 1);
        assert_eq!(
            recover_unresolved_approvals(&events).expect("recover again"),
            0
        );
        let recovered = events
            .read_all()
            .expect("read events")
            .into_iter()
            .filter(|event| event.event_type == "approval.recovered")
            .count();
        assert_eq!(recovered, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
