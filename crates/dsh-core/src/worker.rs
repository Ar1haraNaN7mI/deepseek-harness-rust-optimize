//! Durable worker leases for long-running task runs.

use crate::task::TaskStore;
use chrono::Duration as ChronoDuration;
use dsh_protocol::{RunRecord, RunState, TaskState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

pub struct WorkerSupervisor {
    tasks: Arc<TaskStore>,
    worker_id: String,
    heartbeat_interval: Duration,
    stale_after: Duration,
}

pub struct RunLease {
    supervisor: Arc<WorkerSupervisor>,
    task_id: String,
    run_id: String,
    stop_tx: watch::Sender<bool>,
    finished: AtomicBool,
}

impl WorkerSupervisor {
    pub fn new(
        tasks: Arc<TaskStore>,
        worker_id: impl Into<String>,
        heartbeat_interval: Duration,
        stale_after: Duration,
    ) -> Self {
        let worker_id = worker_id.into();
        Self {
            tasks,
            worker_id: if worker_id.trim().is_empty() {
                format!("dsh-worker-{}", std::process::id())
            } else {
                worker_id
            },
            heartbeat_interval: heartbeat_interval.max(Duration::from_millis(100)),
            stale_after: stale_after.max(Duration::from_secs(1)),
        }
    }

    pub fn local_default(
        tasks: Arc<TaskStore>,
        heartbeat_interval: Duration,
        stale_after: Duration,
    ) -> Self {
        Self::new(
            tasks,
            format!("dsh-worker-{}-{}", std::process::id(), Uuid::new_v4()),
            heartbeat_interval,
            stale_after,
        )
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub fn stale_after(&self) -> Duration {
        self.stale_after
    }

    pub fn recover_stale_runs(&self) -> anyhow::Result<Vec<RunRecord>> {
        let now = chrono::Utc::now();
        let mut recovered = self.tasks.recover_orphaned_runs(now, &self.worker_id)?;
        let stale_after = ChronoDuration::from_std(self.stale_after)
            .unwrap_or_else(|_| ChronoDuration::seconds(1));
        let stale = self.tasks.recover_stale_runs(now, stale_after)?;
        recovered.extend(stale);
        Ok(recovered)
    }

    pub fn acquire(self: &Arc<Self>, run_id: &str) -> anyhow::Result<RunLease> {
        if tokio::runtime::Handle::try_current().is_err() {
            anyhow::bail!("worker leases require a Tokio runtime for heartbeats");
        }
        let run = self
            .tasks
            .claim_run(run_id, self.worker_id.clone(), chrono::Utc::now())?;
        let (stop_tx, stop_rx) = watch::channel(false);
        let tasks = self.tasks.clone();
        let worker_id = self.worker_id.clone();
        let heartbeat_interval = self.heartbeat_interval;
        let heartbeat_run_id = run.id.clone();
        tokio::spawn(async move {
            heartbeat_loop(
                tasks,
                heartbeat_run_id,
                worker_id,
                heartbeat_interval,
                stop_rx,
            )
            .await;
        });
        Ok(RunLease {
            supervisor: self.clone(),
            task_id: run.task_id,
            run_id: run.id,
            stop_tx,
            finished: AtomicBool::new(false),
        })
    }
}

impl RunLease {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn finish(&self, task_state: TaskState, run_state: RunState) -> anyhow::Result<()> {
        if self.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let _ = self.stop_tx.send(true);
        if let Err(error) = self.supervisor.tasks.finish_run(
            &self.run_id,
            &self.supervisor.worker_id,
            task_state,
            run_state,
            chrono::Utc::now(),
        ) {
            // No projection is changed when the combined append fails.
            // Resetting the guard allows a caller to retry safely.
            self.finished.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(true);
    }
}

async fn heartbeat_loop(
    tasks: Arc<TaskStore>,
    run_id: String,
    worker_id: String,
    heartbeat_interval: Duration,
    mut stop_rx: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(heartbeat_interval);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if *stop_rx.borrow() {
                    return;
                }
                if let Err(err) = tasks.heartbeat_run(&run_id, &worker_id, chrono::Utc::now()) {
                    tracing::debug!(error = %err, run_id, "worker heartbeat stopped");
                    return;
                }
            }
            changed = stop_rx.changed() => {
                if changed.is_err() || *stop_rx.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{EventStore, JsonlEventStore};
    use dsh_protocol::{GoalSpec, TaskRecord};
    use std::path::PathBuf;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-rust-p1-{label}-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn lease_claims_heartbeats_and_releases() {
        let dir = temp_path("lease");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let tasks = Arc::new(TaskStore::open(events).expect("tasks"));
        let task = tasks
            .create_task(TaskRecord::new(GoalSpec::new("lease")).expect("task"))
            .expect("create");
        tasks
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        let run = tasks.create_run(&task.id, 1).expect("run");
        let supervisor = Arc::new(WorkerSupervisor::new(
            tasks.clone(),
            "worker-test",
            Duration::from_millis(10),
            Duration::from_secs(1),
        ));
        let lease = supervisor.acquire(&run.id).expect("claim");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            tasks.run(&run.id).unwrap().worker_id.as_deref(),
            Some("worker-test")
        );
        lease
            .finish(TaskState::Completed, RunState::Completed)
            .expect("finish");
        assert_eq!(tasks.run(&run.id).unwrap().state, RunState::Completed);
        assert!(tasks.run(&run.id).unwrap().worker_id.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
