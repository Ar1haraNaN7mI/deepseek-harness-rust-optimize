//! Local durable task scheduler for long-running goals.
//!
//! The scheduler is deliberately small: TaskStore remains the source of
//! truth, while this module only decides when a queued task is eligible to
//! start and drains the AgentLoop event stream for headless runs.

use crate::task::TaskStore;
use crate::{AgentEvent, AgentEventContext, AgentLoop, Runtime};
use chrono::{Duration as ChronoDuration, Utc};
use dsh_protocol::{EventEnvelope, EventSource, RunState, TaskState};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerSnapshot {
    pub enabled: bool,
    pub started: bool,
    pub active_runs: usize,
    pub tick_count: u64,
    pub scheduled_runs: u64,
    pub retry_promotions: u64,
    pub recovered_runs: u64,
    pub errors: u64,
}

#[derive(Default)]
struct SchedulerState {
    active: HashSet<String>,
    started: bool,
    tick_count: u64,
    scheduled_runs: u64,
    retry_promotions: u64,
    recovered_runs: u64,
    errors: u64,
}

pub struct TaskScheduler {
    tasks: Arc<TaskStore>,
    enabled: bool,
    poll_interval: Duration,
    max_concurrency: usize,
    runtime: Mutex<Option<Weak<Runtime>>>,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    stopped: Notify,
    state: Mutex<SchedulerState>,
}

impl TaskScheduler {
    pub fn new(
        tasks: Arc<TaskStore>,
        enabled: bool,
        poll_interval: Duration,
        max_concurrency: usize,
    ) -> Self {
        Self {
            tasks,
            enabled,
            poll_interval: poll_interval.max(Duration::from_secs(1)),
            max_concurrency: max_concurrency.max(1),
            runtime: Mutex::new(None),
            stop_tx: Mutex::new(None),
            stopped: Notify::new(),
            state: Mutex::new(SchedulerState::default()),
        }
    }

    pub fn attach_runtime(&self, runtime: &Arc<Runtime>) {
        *self.runtime.lock() = Some(Arc::downgrade(runtime));
    }

    pub fn start(self: &Arc<Self>) {
        if !self.enabled {
            return;
        }
        {
            let mut state = self.state.lock();
            if state.started {
                return;
            }
            state.started = true;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("cannot start task scheduler outside a Tokio runtime");
            self.state.lock().started = false;
            return;
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        *self.stop_tx.lock() = Some(stop_tx);
        let scheduler = self.clone();
        tokio::spawn(async move {
            scheduler.clone().run_loop(stop_rx).await;
            scheduler.state.lock().started = false;
            scheduler.stopped.notify_waiters();
        });
    }

    pub fn stop(&self) {
        if let Some(stop_tx) = self.stop_tx.lock().take() {
            let _ = stop_tx.send(true);
        }
        if let Some(runtime) = self.runtime.lock().as_ref().and_then(Weak::upgrade) {
            for handle in runtime.active_agents.lock().values() {
                handle.cancel();
            }
        }
        // Keep active task ids until their spawned agent closures observe the
        // cancellation and remove themselves. Clearing this set eagerly can
        // let an immediately restarted scheduler launch a duplicate run while
        // the old agent is still unwinding.
    }

    /// Request shutdown and wait until the background polling loop has
    /// actually exited. `stop` remains synchronous for Drop/CLI callers;
    /// async services should use this boundary before restarting a scheduler
    /// or tearing down its runtime.
    pub async fn stop_and_wait(&self) {
        self.stop();
        loop {
            // Create the notification future before checking state so a loop
            // exit racing this check cannot lose its wake-up.
            let notified = self.stopped.notified();
            if !self.state.lock().started {
                return;
            }
            notified.await;
        }
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        let state = self.state.lock();
        SchedulerSnapshot {
            enabled: self.enabled,
            started: state.started,
            active_runs: state.active.len(),
            tick_count: state.tick_count,
            scheduled_runs: state.scheduled_runs,
            retry_promotions: state.retry_promotions,
            recovered_runs: state.recovered_runs,
            errors: state.errors,
        }
    }

    fn update_state(&self, update: impl FnOnce(&mut SchedulerState)) {
        update(&mut self.state.lock());
    }

    async fn run_loop(self: Arc<Self>, mut stop_rx: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(self.poll_interval);
        loop {
            if *stop_rx.borrow() {
                return;
            }
            tokio::select! {
                _ = interval.tick() => self.tick().await,
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        return;
                    }
                }
            }
        }
    }

    async fn tick(self: &Arc<Self>) {
        let Some(runtime) = self.runtime.lock().as_ref().and_then(Weak::upgrade) else {
            self.stop();
            return;
        };
        self.update_state(|state| state.tick_count = state.tick_count.saturating_add(1));

        match runtime.workers.recover_stale_runs() {
            Ok(recovered) => {
                self.update_state(|state| {
                    state.recovered_runs =
                        state.recovered_runs.saturating_add(recovered.len() as u64);
                });
            }
            Err(err) => {
                tracing::warn!(error = %err, "scheduler failed to recover stale runs");
                self.update_state(|state| state.errors = state.errors.saturating_add(1));
            }
        }

        self.recover_stranded_tasks(&runtime);
        self.promote_retries(&runtime);
        self.fail_exhausted_queued(&runtime);
        if !runtime.llm.is_ready() {
            return;
        }

        let capacity = self
            .max_concurrency
            .saturating_sub(self.state.lock().active.len());
        if capacity == 0 {
            return;
        }

        let queued: Vec<_> = queued_candidates(&self.tasks.tasks(), capacity)
            .into_iter()
            .filter(|task| {
                self.tasks
                    .latest_run_for_task(&task.id)
                    .is_none_or(|run| run.attempt < task.retry_policy.max_attempts)
            })
            .collect();
        for task in queued {
            self.schedule_task(runtime.clone(), task).await;
        }
    }

    /// Execute one scheduling pass. This is useful for embedding the
    /// scheduler in an existing service loop and makes recovery testable
    /// without spawning a background task.
    pub async fn tick_once(self: &Arc<Self>) {
        self.tick().await;
    }

    fn promote_retries(&self, runtime: &Arc<Runtime>) {
        let now = Utc::now();
        for task in self.tasks.tasks() {
            if task.state != TaskState::Failed {
                continue;
            }
            let Some(run) = self.tasks.latest_run_for_task(&task.id) else {
                continue;
            };
            if !retry_ready(&task, &run, now) {
                continue;
            }
            let delay = task.retry_policy.delay_secs(run.attempt);
            if let Err(err) = self.tasks.transition_task(&task.id, TaskState::Queued) {
                tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to promote retry");
                self.update_state(|state| state.errors = state.errors.saturating_add(1));
                continue;
            }
            self.update_state(|state| {
                state.retry_promotions = state.retry_promotions.saturating_add(1)
            });
            let event = EventEnvelope::new(
                "task.retry_scheduled",
                serde_json::json!({
                    "task_id": task.id.clone(),
                    "run_id": run.id.clone(),
                    "attempt": run.attempt + 1,
                    "delay_secs": delay,
                }),
            )
            .with_source(EventSource::Scheduler)
            .with_task(task.id.clone())
            .with_run(run.id.clone());
            if let Err(err) = runtime.record_event(event.clone()) {
                tracing::warn!(error = %err, "failed to persist retry event");
            }
        }
    }

    fn recover_stranded_tasks(&self, runtime: &Arc<Runtime>) {
        let active = self.state.lock().active.clone();
        for task in self.tasks.tasks() {
            if task.state != TaskState::Running
                || active.contains(&task.id)
                || runtime.is_task_reserved(&task.id)
            {
                continue;
            }
            let Some(run) = self.tasks.latest_run_for_task(&task.id) else {
                // A running task with no run is stranded as well; queue it so
                // the next pass can create a fresh attempt.
                if let Err(err) = self.tasks.transition_task(&task.id, TaskState::Queued) {
                    tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to recover task without run");
                    self.update_state(|state| state.errors = state.errors.saturating_add(1));
                }
                continue;
            };
            let next_state = match run.state {
                RunState::Completed => TaskState::Completed,
                RunState::Cancelled => TaskState::Cancelled,
                RunState::Failed if run.attempt >= task.retry_policy.max_attempts => {
                    TaskState::Failed
                }
                RunState::Starting | RunState::Paused | RunState::Failed | RunState::Unknown => {
                    TaskState::Queued
                }
                RunState::Running | RunState::WaitingApproval | RunState::WaitingEvent => continue,
            };
            if task.state == next_state {
                continue;
            }
            if let Err(err) = self.tasks.transition_task(&task.id, next_state) {
                tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to recover stranded task");
                self.update_state(|state| state.errors = state.errors.saturating_add(1));
                continue;
            }
            let task_id = task.id.clone();
            let run_id = run.id.clone();
            let event = EventEnvelope::new(
                if next_state == TaskState::Queued {
                    "task.scheduler_requeued"
                } else {
                    "task.scheduler_recovered"
                },
                serde_json::json!({
                    "reason": "scheduler_restart",
                    "task_id": task_id,
                    "run_id": run_id,
                    "state": next_state,
                }),
            )
            .with_source(EventSource::Scheduler)
            .with_task(task.id.clone())
            .with_run(run.id.clone());
            if let Err(err) = runtime.record_event(event) {
                tracing::debug!(error = %err, task_id = %task.id, "failed to persist stranded task event");
            }
        }
    }

    fn fail_exhausted_queued(&self, runtime: &Arc<Runtime>) {
        for task in self.tasks.tasks() {
            if task.state != TaskState::Queued
                || runtime.is_task_reserved(&task.id)
                || runtime.active_agents.lock().contains_key(&task.id)
            {
                continue;
            }
            let Some(run) = self.tasks.latest_run_for_task(&task.id) else {
                continue;
            };
            if run.attempt < task.retry_policy.max_attempts {
                continue;
            }
            if let Err(err) = self.tasks.transition_task(&task.id, TaskState::Failed) {
                tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to close exhausted task");
                self.update_state(|state| state.errors = state.errors.saturating_add(1));
                continue;
            }
            let task_id = task.id.clone();
            let run_id = run.id.clone();
            let event = EventEnvelope::new(
                "task.retry_exhausted",
                serde_json::json!({
                    "task_id": task_id,
                    "run_id": run_id,
                    "attempt": run.attempt,
                    "max_attempts": task.retry_policy.max_attempts,
                }),
            )
            .with_source(EventSource::Scheduler)
            .with_task(task.id.clone())
            .with_run(run.id.clone());
            if let Err(err) = runtime.record_event(event) {
                tracing::debug!(error = %err, task_id = %task.id, "failed to persist retry exhaustion event");
            }
        }
    }

    async fn schedule_task(
        self: &Arc<Self>,
        runtime: Arc<Runtime>,
        task: dsh_protocol::TaskRecord,
    ) {
        {
            let mut state = self.state.lock();
            if !state.active.insert(task.id.clone()) {
                return;
            }
        }
        if let Err(err) = self.tasks.transition_task(&task.id, TaskState::Running) {
            tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to reserve task");
            self.update_state(|state| {
                state.active.remove(&task.id);
                state.errors = state.errors.saturating_add(1);
            });
            return;
        }

        let Some(session_id) = task.session_id.clone() else {
            let _ = self.tasks.transition_task(&task.id, TaskState::Failed);
            let _ = runtime.record_event(
                EventEnvelope::new(
                    "task.failed",
                    serde_json::json!({"reason": "task has no session"}),
                )
                .with_source(EventSource::Scheduler)
                .with_task(task.id.clone()),
            );
            self.update_state(|state| {
                state.active.remove(&task.id);
            });
            return;
        };
        if task
            .goal
            .deadline
            .is_some_and(|deadline| deadline <= Utc::now())
        {
            let _ = self.tasks.transition_task(&task.id, TaskState::Failed);
            let _ = runtime.record_event(
                EventEnvelope::new(
                    "task.failed",
                    serde_json::json!({"reason": "goal deadline expired"}),
                )
                .with_source(EventSource::Scheduler)
                .with_task(task.id.clone()),
            );
            self.update_state(|state| {
                state.active.remove(&task.id);
            });
            return;
        }
        let session = match runtime.sessions.get_or_load(&session_id) {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!(error = %err, task_id = %task.id, "scheduler failed to load task session");
                let _ = self.tasks.transition_task(&task.id, TaskState::Failed);
                self.update_state(|state| {
                    state.active.remove(&task.id);
                    state.errors = state.errors.saturating_add(1);
                });
                return;
            }
        };
        if session.read().goal_paused {
            let _ = self.tasks.transition_task(&task.id, TaskState::Paused);
            self.update_state(|state| {
                state.active.remove(&task.id);
            });
            return;
        }

        let (event_tx, mut event_rx) = mpsc::channel(256);
        let agent = AgentLoop::new(runtime.clone());
        let scheduler = self.clone();
        let task_id = task.id.clone();
        let goal = task.goal.outcome.clone();
        self.update_state(|state| state.scheduled_runs = state.scheduled_runs.saturating_add(1));
        tokio::spawn(async move {
            let start_result = agent
                .run_task(task_id.clone(), session.clone(), goal, event_tx)
                .await;
            match start_result {
                Err(err) => {
                    tracing::warn!(error = %err, task_id = %task_id, "scheduler failed to start task run");
                    let _ = scheduler.tasks.transition_task(&task_id, TaskState::Failed);
                    let _ = runtime.record_event(
                        EventEnvelope::new(
                            "task.failed",
                            serde_json::json!({"reason": err.to_string()}),
                        )
                        .with_source(EventSource::Scheduler)
                        .with_task(task_id.clone()),
                    );
                }
                Ok(handle) => {
                    runtime.register_agent_handle(task_id.clone(), handle);
                    let mut context = AgentEventContext::for_session(session.read().id.clone());
                    context.task_id = Some(task_id.clone());
                    while let Some(event) = event_rx.recv().await {
                        if let AgentEvent::TurnStarted(turn_id) = &event {
                            context.turn_id = Some(turn_id.clone());
                            context.run_id = scheduler
                                .tasks
                                .latest_run_for_task(&task_id)
                                .map(|run| run.id);
                        }
                        if let Err(err) = runtime.record_event(event.to_protocol_event(&context)) {
                            tracing::debug!(error = %err, task_id = %task_id, "failed to persist scheduler event");
                        }
                        if matches!(event, AgentEvent::Done) {
                            break;
                        }
                    }
                    runtime.clear_agent_handle(&task_id);
                }
            }
            let mut state = scheduler.state.lock();
            state.active.remove(&task_id);
        });
    }
}

fn retry_ready(
    task: &dsh_protocol::TaskRecord,
    run: &dsh_protocol::RunRecord,
    now: chrono::DateTime<Utc>,
) -> bool {
    if run.state != RunState::Failed || run.attempt >= task.retry_policy.max_attempts {
        return false;
    }
    let delay = task.retry_policy.delay_secs(run.attempt);
    now.signed_duration_since(run.updated_at) >= ChronoDuration::seconds(delay as i64)
}

fn queued_candidates(
    tasks: &[dsh_protocol::TaskRecord],
    capacity: usize,
) -> Vec<dsh_protocol::TaskRecord> {
    tasks
        .iter()
        .filter(|task| task.state == TaskState::Queued && task.session_id.is_some())
        .take(capacity)
        .cloned()
        .collect()
}

impl Drop for TaskScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_protocol::{GoalSpec, RetryPolicy, TaskRecord};
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn retry_readiness_honors_backoff_and_max_attempts() {
        let mut task = TaskRecord::new_with_retry_policy(
            GoalSpec::new("retry"),
            RetryPolicy {
                max_attempts: 3,
                backoff_secs: 10,
                max_backoff_secs: 30,
            },
        )
        .expect("task");
        let now = Utc::now();
        let mut run = dsh_protocol::RunRecord::new(task.id.clone(), 1);
        run.state = RunState::Failed;
        run.updated_at = now - ChronoDuration::seconds(9);
        assert!(!retry_ready(&task, &run, now));
        run.updated_at = now - ChronoDuration::seconds(10);
        assert!(retry_ready(&task, &run, now));
        run.attempt = 3;
        assert!(!retry_ready(&task, &run, now));
        task.retry_policy.max_attempts = 1;
        run.attempt = 1;
        assert!(!retry_ready(&task, &run, now));
    }

    #[test]
    fn queued_candidates_require_sessions_and_respect_capacity() {
        let mut first = TaskRecord::new(GoalSpec::new("first")).expect("task");
        first.session_id = Some("session-1".into());
        let second = TaskRecord::new(GoalSpec::new("no session")).expect("task");
        let mut third = TaskRecord::new(GoalSpec::new("third")).expect("task");
        third.session_id = Some("session-3".into());
        let candidates = queued_candidates(&[first.clone(), second, third], 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, first.id);
    }

    #[tokio::test]
    async fn stop_and_wait_observes_background_loop_exit() {
        let dir = std::env::temp_dir().join(format!("dsh-scheduler-stop-{}", Uuid::new_v4()));
        let path: PathBuf = dir.join("events.jsonl");
        let events: Arc<dyn crate::task::EventStore> =
            Arc::new(crate::task::JsonlEventStore::open(&path).expect("events"));
        let tasks = Arc::new(TaskStore::open(events).expect("tasks"));
        let scheduler = Arc::new(TaskScheduler::new(tasks, true, Duration::from_secs(1), 1));
        scheduler.start();
        tokio::time::timeout(Duration::from_secs(2), scheduler.stop_and_wait())
            .await
            .expect("scheduler stop timeout");
        assert!(!scheduler.snapshot().started);
        let _ = std::fs::remove_dir_all(dir);
    }
}
