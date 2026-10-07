//! Durable task models and append-only event storage.
//!
//! A session is a transcript.  A task is the durable unit of work and a run
//! is one execution attempt.  Keeping these concepts separate is what allows
//! a future supervisor to resume work after a process restart without
//! rewriting conversational history.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use dsh_protocol::{CheckpointRecord, EventEnvelope, RunRecord, RunState, TaskRecord, TaskState};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify;

pub const TASK_SNAPSHOT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct TaskStoreSnapshot {
    #[serde(default = "default_task_snapshot_schema_version")]
    pub schema_version: u16,
    #[serde(default)]
    pub event_sequence: u64,
    #[serde(default)]
    pub tasks: Vec<TaskRecord>,
    #[serde(default)]
    pub runs: Vec<RunRecord>,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointRecord>,
}

fn default_task_snapshot_schema_version() -> u16 {
    TASK_SNAPSHOT_SCHEMA_VERSION
}

/// Transport-independent event store contract.  The first implementation is
/// JSONL so it is inspectable and easy to migrate; a SQLite/WAL backend can be
/// added later without changing task or protocol callers.
pub trait EventStore: Send + Sync {
    fn append(&self, event: EventEnvelope) -> Result<EventEnvelope>;
    fn read_all(&self) -> Result<Vec<EventEnvelope>>;
    fn read_after(&self, sequence: u64) -> Result<Vec<EventEnvelope>>;

    /// Read at most `limit` events after a cursor. Implementations with an
    /// indexed backend can override this to avoid materialising the complete
    /// log; the JSONL fallback keeps the trait backwards compatible.
    fn read_after_limit(&self, sequence: u64, limit: usize) -> Result<Vec<EventEnvelope>> {
        Ok(self.read_after(sequence)?.into_iter().take(limit).collect())
    }

    fn latest_sequence(&self) -> u64;
}

/// Append-only JSONL event log with monotonic sequence assignment.
pub struct JsonlEventStore {
    path: PathBuf,
    writer: Mutex<File>,
    next_sequence: AtomicU64,
}

impl JsonlEventStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create event store directory {}", parent.display()))?;
        }

        let mut writer = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .with_context(|| format!("open event store {}", path.display()))?;
        let existing = with_process_lock(&path, || {
            let existing = read_events(&path)?;
            // A process can crash after the final JSON object is completely
            // written but before its line terminator reaches disk. Preserve
            // that valid event, while restoring the delimiter before the
            // next append so two objects can never be glued together.
            ensure_trailing_newline(&mut writer, &path)?;
            Ok(existing)
        })?;
        let latest = existing.last().map(|event| event.sequence).unwrap_or(0);

        Ok(Self {
            path,
            writer: Mutex::new(writer),
            next_sequence: AtomicU64::new(latest),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append_payload<T: Serialize>(
        &self,
        event_type: impl Into<String>,
        payload: &T,
    ) -> Result<EventEnvelope> {
        let payload = serde_json::to_value(payload).context("serialize event payload")?;
        self.append(EventEnvelope::new(event_type, payload))
    }

    /// Flush the underlying file after a checkpoint or other durability
    /// boundary.  Normal events only need the cheaper `append` flush.
    pub fn sync(&self) -> Result<()> {
        let writer = self.writer.lock();
        with_process_lock(&self.path, || {
            writer.sync_data().context("sync event store")
        })
    }
}

/// Ensure a JSONL file ends at a record boundary. A complete final JSON value
/// without `\n` is valid when read in isolation, but appending another value
/// directly after it would create an unparsable concatenated line.
fn ensure_trailing_newline(file: &mut File, path: &Path) -> Result<()> {
    let length = file
        .metadata()
        .with_context(|| format!("stat event store {}", path.display()))?
        .len();
    if length == 0 {
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))
        .with_context(|| format!("seek event store {}", path.display()))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)
        .with_context(|| format!("read event store terminator {}", path.display()))?;
    if last[0] == b'\n' {
        return Ok(());
    }

    file.seek(SeekFrom::End(0))
        .with_context(|| format!("seek event store append {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("repair event store terminator {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush event store terminator {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync event store terminator {}", path.display()))?;
    Ok(())
}

impl EventStore for JsonlEventStore {
    fn append(&self, mut event: EventEnvelope) -> Result<EventEnvelope> {
        event
            .validate()
            .map_err(anyhow::Error::msg)
            .context("validate event envelope")?;
        let mut writer = self.writer.lock();
        with_process_lock(&self.path, || {
            // Refresh while holding the OS-level lock so separate dsh
            // processes cannot allocate the same sequence number.
            let latest = read_events(&self.path)?
                .last()
                .map(|existing| existing.sequence)
                .unwrap_or(0);
            let sequence = latest
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("event sequence exhausted at u64::MAX"))?;
            self.next_sequence.store(sequence, Ordering::Release);
            event.sequence = sequence;
            let line = serde_json::to_string(&event).context("serialize event envelope")?;
            writeln!(writer, "{line}").context("append event")?;
            writer.flush().context("flush event")?;
            Ok(event)
        })
    }

    fn read_all(&self) -> Result<Vec<EventEnvelope>> {
        let _writer = self.writer.lock();
        let events = with_process_lock(&self.path, || read_events(&self.path))?;
        if let Some(sequence) = events.last().map(|event| event.sequence) {
            self.next_sequence.store(sequence, Ordering::Release);
        }
        Ok(events)
    }

    fn read_after(&self, sequence: u64) -> Result<Vec<EventEnvelope>> {
        self.read_after_limit(sequence, usize::MAX)
    }

    fn read_after_limit(&self, sequence: u64, limit: usize) -> Result<Vec<EventEnvelope>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _writer = self.writer.lock();
        let (events, latest) = with_process_lock(&self.path, || {
            read_events_after(&self.path, sequence, limit)
        })?;
        if latest > 0 {
            self.next_sequence.store(latest, Ordering::Release);
        }
        Ok(events)
    }

    fn latest_sequence(&self) -> u64 {
        self.next_sequence.load(Ordering::Acquire)
    }
}

fn with_process_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = ProcessFileLock::acquire(path)?;
    operation()
}

struct ProcessFileLock {
    path: PathBuf,
}

impl ProcessFileLock {
    fn acquire(event_path: &Path) -> Result<Self> {
        let lock_path = event_path.with_file_name(format!(
            "{}.lock",
            event_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("events")
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            // Directory creation is atomic and does not keep a sharing-locked
            // file handle on Windows, so contenders consistently observe
            // AlreadyExists instead of an opaque access-denied error.
            match fs::create_dir(&lock_path) {
                Ok(()) => {
                    let marker = format!(
                        "pid={} acquired_at={}\n",
                        std::process::id(),
                        Utc::now().to_rfc3339()
                    );
                    if let Err(error) = fs::write(lock_path.join("owner"), marker) {
                        let _ = fs::remove_dir_all(&lock_path);
                        return Err(error).with_context(|| {
                            format!("write event store lock owner {}", lock_path.display())
                        });
                    }
                    return Ok(Self { path: lock_path });
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    let lock_age = lock_path
                        .join("owner")
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .or_else(|_| {
                            lock_path
                                .metadata()
                                .and_then(|metadata| metadata.modified())
                        })
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(120));
                    if lock_age {
                        if lock_path.is_dir() {
                            let _ = fs::remove_dir_all(&lock_path);
                        } else {
                            let _ = fs::remove_file(&lock_path);
                        }
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "timed out waiting for event store lock {}",
                            lock_path.display()
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("create event store lock {}", lock_path.display())
                    });
                }
            }
        }
    }
}

impl Drop for ProcessFileLock {
    fn drop(&mut self) {
        if self.path.is_dir() {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            // Compatibility for lock files created by an older process.
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_events(path: &Path) -> Result<Vec<EventEnvelope>> {
    read_events_internal(path, None).map(|(events, _)| events)
}

/// Stream a bounded suffix of a JSONL event log while still validating every
/// sequence number. Returning the latest sequence separately lets remote
/// readers advertise a fresh cursor without loading all payloads into memory.
fn read_events_after(
    path: &Path,
    sequence: u64,
    limit: usize,
) -> Result<(Vec<EventEnvelope>, u64)> {
    read_events_internal(path, Some((sequence, limit)))
}

/// Parse the JSONL stream while retaining only an optional bounded suffix.
/// A crash can occur after bytes of the final JSON object are written but
/// before its newline reaches disk. Such a final, unterminated parse error is
/// safe to discard; a malformed *newline-terminated* record remains a hard
/// corruption error and is never silently hidden.
fn read_events_internal(
    path: &Path,
    selection: Option<(u64, usize)>,
) -> Result<(Vec<EventEnvelope>, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let file = File::open(path).with_context(|| format!("read event store {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut previous_sequence = 0;
    let mut line_no = 0usize;
    let mut byte_offset = 0u64;
    loop {
        let line_start = byte_offset;
        let mut raw_line = Vec::new();
        let bytes_read = reader
            .read_until(b'\n', &mut raw_line)
            .with_context(|| format!("read event line {}", line_no + 1))?;
        if bytes_read == 0 {
            break;
        }
        byte_offset = byte_offset.saturating_add(bytes_read as u64);
        line_no += 1;
        let terminated = raw_line.last() == Some(&b'\n');
        let line = String::from_utf8_lossy(&raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<EventEnvelope>(line.trim()) {
            Ok(event) => event,
            Err(error) if !terminated => {
                // Drop only the final unterminated record. The reader must be
                // released before opening a write handle on Windows.
                drop(reader);
                let repair = OpenOptions::new()
                    .write(true)
                    .open(path)
                    .with_context(|| format!("repair event store {}", path.display()))?;
                repair
                    .set_len(line_start)
                    .with_context(|| format!("truncate partial event line {}", line_no))?;
                repair
                    .sync_data()
                    .with_context(|| format!("sync repaired event store {}", path.display()))?;
                tracing::warn!(
                    path = %path.display(),
                    line = line_no,
                    error = %error,
                    "discarded unterminated JSONL event tail after a partial write"
                );
                break;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("parse event line {}", line_no));
            }
        };
        event
            .validate()
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("validate event line {}", line_no))?;
        if event.sequence == 0 {
            anyhow::bail!("event line {} has zero sequence", line_no);
        }
        if event.sequence <= previous_sequence {
            anyhow::bail!(
                "event line {} has non-increasing sequence {} after {}",
                line_no,
                event.sequence,
                previous_sequence
            );
        }
        previous_sequence = event.sequence;
        if let Some((sequence, limit)) = selection {
            if event.sequence > sequence && events.len() < limit {
                events.push(event);
            }
        } else {
            events.push(event);
        }
    }
    Ok((events, previous_sequence))
}

/// In-memory task projection rebuilt from the append-only event log.
pub struct TaskStore {
    events: std::sync::Arc<dyn EventStore>,
    projection_guard: Mutex<()>,
    tasks: RwLock<HashMap<String, TaskRecord>>,
    runs: RwLock<HashMap<String, RunRecord>>,
    checkpoints: RwLock<HashMap<String, CheckpointRecord>>,
    snapshot_path: Option<PathBuf>,
    event_notify: RwLock<Option<std::sync::Arc<Notify>>>,
    /// Highest event sequence observed by this in-memory projection. It is
    /// deliberately separate from the event store's global cursor because a
    /// different process may append records that have not been replayed here.
    projection_sequence: AtomicU64,
}

/// One durable projection commit for the end of a worker attempt. Keeping
/// both records in one event prevents a crash between separate run-release
/// and task-transition appends from leaving the projections permanently
/// disagreeing after replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunFinishedRecord {
    task: TaskRecord,
    run: RunRecord,
}

impl TaskStore {
    pub fn open(events: std::sync::Arc<dyn EventStore>) -> Result<Self> {
        Self::open_internal(events, None)
    }

    pub fn open_with_snapshot(
        events: std::sync::Arc<dyn EventStore>,
        snapshot_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::open_internal(events, Some(snapshot_path.into()))
    }

    fn open_internal(
        events: std::sync::Arc<dyn EventStore>,
        snapshot_path: Option<PathBuf>,
    ) -> Result<Self> {
        // Refresh a JSONL-backed sequence before trusting a snapshot. Another
        // process may have appended records after this store instance was
        // constructed, and replaying from a stale cursor would otherwise
        // silently skip them.
        let _ = events.read_all()?;
        let store = Self {
            events: events.clone(),
            projection_guard: Mutex::new(()),
            tasks: RwLock::new(HashMap::new()),
            runs: RwLock::new(HashMap::new()),
            checkpoints: RwLock::new(HashMap::new()),
            snapshot_path,
            event_notify: RwLock::new(None),
            projection_sequence: AtomicU64::new(0),
        };
        let snapshot_sequence = store.load_snapshot()?;
        store
            .projection_sequence
            .store(snapshot_sequence, Ordering::Release);
        store.replay_after(snapshot_sequence)?;
        Ok(store)
    }

    /// Attach an in-process wake-up used by long-poll event consumers. The
    /// durable JSONL log remains the source of truth, so this is only a
    /// latency optimization and is safe to omit for standalone projections.
    pub fn set_event_notifier(&self, notifier: std::sync::Arc<Notify>) {
        *self.event_notify.write() = Some(notifier);
    }

    pub fn create_task(&self, task: TaskRecord) -> Result<TaskRecord> {
        let _projection_guard = self.projection_guard.lock();
        task.validate().map_err(anyhow::Error::msg)?;
        if self.tasks.read().contains_key(&task.id) {
            anyhow::bail!("task already exists: {}", task.id);
        }
        self.append_record("task.created", Some(&task.id), None, &task)?;
        self.tasks.write().insert(task.id.clone(), task.clone());
        self.persist_snapshot_best_effort_unlocked();
        Ok(task)
    }

    pub fn task(&self, id: &str) -> Option<TaskRecord> {
        self.tasks.read().get(id).cloned()
    }

    pub fn task_for_session(&self, session_id: &str) -> Option<TaskRecord> {
        self.tasks
            .read()
            .values()
            .filter(|task| task.session_id.as_deref() == Some(session_id))
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.id.cmp(&right.id))
            })
            .cloned()
    }

    pub fn tasks(&self) -> Vec<TaskRecord> {
        let mut tasks: Vec<_> = self.tasks.read().values().cloned().collect();
        tasks.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        tasks
    }

    pub fn transition_task(&self, id: &str, next: TaskState) -> Result<TaskRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut tasks = self.tasks.write();
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {id}"))?;
        if !task_transition_allowed(task.state, next) {
            anyhow::bail!("invalid task transition {:?} -> {:?}", task.state, next);
        }
        let previous = task.clone();
        task.state = next;
        task.updated_at = chrono::Utc::now();
        let updated = task.clone();
        if let Err(err) = self.append_record("task.state_changed", Some(id), None, &updated) {
            tasks.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(tasks);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    /// Update the declarative goal contract while preserving the task id and
    /// lifecycle state. The complete record is logged for replay compatibility.
    pub fn update_task(
        &self,
        id: &str,
        update: impl FnOnce(&mut TaskRecord),
    ) -> Result<TaskRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut tasks = self.tasks.write();
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {id}"))?;
        let previous = task.clone();
        update(task);
        if task.id != previous.id {
            tasks.insert(id.to_string(), previous);
            anyhow::bail!("task id is immutable");
        }
        if task.state != previous.state {
            tasks.insert(id.to_string(), previous);
            anyhow::bail!("use transition_task to change task state");
        }
        if let Err(error) = task.validate() {
            tasks.insert(id.to_string(), previous);
            anyhow::bail!("invalid task update: {error}");
        }
        task.updated_at = Utc::now();
        let updated = task.clone();
        if let Err(err) = self.append_record("task.updated", Some(id), None, &updated) {
            tasks.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(tasks);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    pub fn create_run(&self, task_id: &str, attempt: u32) -> Result<RunRecord> {
        let _projection_guard = self.projection_guard.lock();
        let Some(task) = self.task(task_id) else {
            anyhow::bail!("task not found: {task_id}");
        };
        if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
            anyhow::bail!("task {task_id} is terminal ({:?})", task.state);
        }
        let attempt = attempt.max(1);
        if self
            .runs
            .read()
            .values()
            .any(|run| run.task_id == task_id && run.attempt == attempt)
        {
            anyhow::bail!("run attempt {attempt} already exists for task {task_id}");
        }
        let run = RunRecord::new(task_id, attempt);
        self.append_record("run.started", Some(task_id), Some(&run.id), &run)?;
        self.runs.write().insert(run.id.clone(), run.clone());
        self.persist_snapshot_best_effort_unlocked();
        Ok(run)
    }

    pub fn run(&self, id: &str) -> Option<RunRecord> {
        self.runs.read().get(id).cloned()
    }

    pub fn runs(&self) -> Vec<RunRecord> {
        let mut runs: Vec<_> = self.runs.read().values().cloned().collect();
        runs.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        runs
    }

    pub fn latest_run_for_task(&self, task_id: &str) -> Option<RunRecord> {
        self.runs
            .read()
            .values()
            .filter(|run| run.task_id == task_id)
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.attempt.cmp(&right.attempt))
            })
            .cloned()
    }

    pub fn transition_run(&self, id: &str, next: RunState) -> Result<RunRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if !run_transition_allowed(run.state, next) {
            anyhow::bail!("invalid run transition {:?} -> {:?}", run.state, next);
        }
        let previous = run.clone();
        run.state = next;
        run.updated_at = chrono::Utc::now();
        let updated = run.clone();
        if let Err(err) = self.append_record(
            "run.state_changed",
            Some(&updated.task_id),
            Some(id),
            &updated,
        ) {
            runs.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(runs);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    /// Acquire a starting run for one worker and establish its lease.
    pub fn claim_run(
        &self,
        id: &str,
        worker_id: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<RunRecord> {
        let worker_id = worker_id.into();
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if run.state != RunState::Starting {
            anyhow::bail!("run {id} is not claimable from {:?}", run.state);
        }
        if run.worker_id.is_some() {
            anyhow::bail!("run {id} is already claimed");
        }
        let previous = run.clone();
        run.state = RunState::Running;
        run.worker_id = Some(worker_id.clone());
        run.heartbeat_at = Some(now);
        run.updated_at = now;
        let updated = run.clone();
        if let Err(err) =
            self.append_record("run.claimed", Some(&updated.task_id), Some(id), &updated)
        {
            runs.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(runs);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    /// Refresh a worker lease without changing lifecycle state.
    pub fn heartbeat_run(
        &self,
        id: &str,
        worker_id: &str,
        now: DateTime<Utc>,
    ) -> Result<RunRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if run.worker_id.as_deref() != Some(worker_id) {
            anyhow::bail!("worker `{worker_id}` does not own run {id}");
        }
        if !matches!(
            run.state,
            RunState::Running
                | RunState::WaitingApproval
                | RunState::WaitingEvent
                | RunState::Paused
        ) {
            anyhow::bail!("run {id} is not heartbeatable from {:?}", run.state);
        }
        run.heartbeat_at = Some(now);
        run.updated_at = now;
        let updated = run.clone();
        self.append_record("run.heartbeat", Some(&updated.task_id), Some(id), &updated)?;
        Ok(updated)
    }

    /// Release a lease and commit the terminal/paused state for the run.
    pub fn release_run(
        &self,
        id: &str,
        worker_id: &str,
        next: RunState,
        now: DateTime<Utc>,
    ) -> Result<RunRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if run.worker_id.as_deref() != Some(worker_id) {
            anyhow::bail!("worker `{worker_id}` does not own run {id}");
        }
        if !run_transition_allowed(run.state, next) {
            anyhow::bail!("invalid run transition {:?} -> {:?}", run.state, next);
        }
        let previous = run.clone();
        run.state = next;
        run.worker_id = None;
        run.heartbeat_at = None;
        run.updated_at = now;
        let updated = run.clone();
        if let Err(err) =
            self.append_record("run.released", Some(&updated.task_id), Some(id), &updated)
        {
            runs.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(runs);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    /// Atomically release a worker lease and commit the corresponding task
    /// lifecycle state in one append-only event.
    pub fn finish_run(
        &self,
        id: &str,
        worker_id: &str,
        task_state: TaskState,
        run_state: RunState,
        now: DateTime<Utc>,
    ) -> Result<(TaskRecord, RunRecord)> {
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if run.worker_id.as_deref() != Some(worker_id) {
            anyhow::bail!("worker `{worker_id}` does not own run {id}");
        }

        let mut tasks = self.tasks.write();
        let task = tasks
            .get_mut(&run.task_id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {}", run.task_id))?;
        // A user cancellation can race the final agent callback. Preserve an
        // already-terminal task projection and reconcile the run to the same
        // terminal state instead of leaving a worker lease stranded.
        let effective_task_state =
            if matches!(task.state, TaskState::Completed | TaskState::Cancelled)
                && task.state != task_state
            {
                task.state
            } else {
                task_state
            };
        let effective_run_state = match effective_task_state {
            TaskState::Completed => RunState::Completed,
            TaskState::Cancelled => RunState::Cancelled,
            _ => run_state,
        };
        if !run_transition_allowed(run.state, effective_run_state) {
            anyhow::bail!(
                "invalid run transition {:?} -> {:?}",
                run.state,
                effective_run_state
            );
        }
        if !task_transition_allowed(task.state, effective_task_state) {
            anyhow::bail!(
                "invalid task transition {:?} -> {:?}",
                task.state,
                effective_task_state
            );
        }

        let previous_run = run.clone();
        let previous_task = task.clone();
        run.state = effective_run_state;
        run.worker_id = None;
        run.heartbeat_at = None;
        run.updated_at = now;
        task.state = effective_task_state;
        task.updated_at = now;
        let updated_run = run.clone();
        let updated_task = task.clone();
        let record = RunFinishedRecord {
            task: updated_task.clone(),
            run: updated_run.clone(),
        };
        if let Err(error) = self.append_record(
            "run.finished",
            Some(&updated_task.id),
            Some(&updated_run.id),
            &record,
        ) {
            runs.insert(id.to_string(), previous_run);
            tasks.insert(updated_task.id.clone(), previous_task);
            return Err(error);
        }
        drop(tasks);
        drop(runs);
        self.persist_snapshot_best_effort_unlocked();
        Ok((updated_task, updated_run))
    }

    /// Requeue runs whose lease stopped moving. The failed run remains in the
    /// log for audit, while its task becomes eligible for a later retry.
    pub fn recover_stale_runs(
        &self,
        now: DateTime<Utc>,
        stale_after: Duration,
    ) -> Result<Vec<RunRecord>> {
        let stale_after = stale_after.max(Duration::zero());
        let candidates: Vec<String> = self
            .runs
            .read()
            .values()
            .filter(|run| {
                matches!(
                    run.state,
                    RunState::Running
                        | RunState::WaitingApproval
                        | RunState::WaitingEvent
                        | RunState::Paused
                ) && now.signed_duration_since(run.heartbeat_at.unwrap_or(run.updated_at))
                    >= stale_after
            })
            .map(|run| run.id.clone())
            .collect();
        let mut recovered = Vec::new();
        for run_id in candidates {
            recovered.push(self.recover_stale_run(&run_id, now)?);
        }
        Ok(recovered)
    }

    /// Recover active runs owned by a previous local worker immediately after
    /// restart. The current worker id is excluded so a live supervisor is not
    /// mistaken for an orphan while its heartbeat loop is running.
    pub fn recover_orphaned_runs(
        &self,
        now: DateTime<Utc>,
        current_worker_id: &str,
    ) -> Result<Vec<RunRecord>> {
        let candidates: Vec<String> = self
            .runs
            .read()
            .values()
            .filter(|run| {
                matches!(
                    run.state,
                    RunState::Running
                        | RunState::WaitingApproval
                        | RunState::WaitingEvent
                        | RunState::Paused
                ) && run
                    .worker_id
                    .as_deref()
                    .is_some_and(|worker_id| worker_id != current_worker_id)
            })
            .map(|run| run.id.clone())
            .collect();
        let mut recovered = Vec::new();
        for run_id in candidates {
            recovered.push(self.recover_stale_run(&run_id, now)?);
        }
        Ok(recovered)
    }

    fn recover_stale_run(&self, id: &str, now: DateTime<Utc>) -> Result<RunRecord> {
        let _projection_guard = self.projection_guard.lock();
        let mut runs = self.runs.write();
        let run = runs
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("run not found: {id}"))?;
        if !matches!(
            run.state,
            RunState::Running
                | RunState::WaitingApproval
                | RunState::WaitingEvent
                | RunState::Paused
        ) {
            return Ok(run.clone());
        }
        let previous = run.clone();
        run.state = RunState::Failed;
        run.worker_id = None;
        run.heartbeat_at = None;
        run.updated_at = now;
        let updated = run.clone();
        if let Err(err) =
            self.append_record("run.recovered", Some(&updated.task_id), Some(id), &updated)
        {
            runs.insert(id.to_string(), previous);
            return Err(err);
        }
        drop(runs);

        let mut tasks = self.tasks.write();
        if let Some(task) = tasks.get_mut(&updated.task_id) {
            if matches!(
                task.state,
                TaskState::Running
                    | TaskState::WaitingApproval
                    | TaskState::WaitingEvent
                    | TaskState::Paused
            ) {
                let previous_task = task.clone();
                let retryable = updated.attempt < task.retry_policy.max_attempts;
                let next_state = if retryable {
                    TaskState::Queued
                } else {
                    TaskState::Failed
                };
                task.state = next_state;
                task.updated_at = now;
                let updated_task = task.clone();
                let task_id = updated_task.id.clone();
                let event = EventEnvelope::new(
                    "task.recovered",
                    serde_json::json!({
                        "task": updated_task,
                        "retryable": retryable,
                        "reason": if retryable {
                            "worker lease recovered; retry is available"
                        } else {
                            "worker lease recovered after retry limit"
                        }
                    }),
                )
                .with_source(dsh_protocol::EventSource::Scheduler)
                .with_task(task_id);
                match self.events.append(event) {
                    Ok(persisted) => {
                        self.projection_sequence
                            .store(persisted.sequence, Ordering::Release);
                    }
                    Err(err) => {
                        tasks.insert(updated_task.id.clone(), previous_task);
                        return Err(err);
                    }
                }
                if let Some(notifier) = self.event_notify.read().as_ref() {
                    notifier.notify_waiters();
                }
            }
        }
        drop(tasks);
        self.persist_snapshot_best_effort_unlocked();
        Ok(updated)
    }

    pub fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<CheckpointRecord> {
        let _projection_guard = self.projection_guard.lock();
        checkpoint
            .validate()
            .map_err(anyhow::Error::msg)
            .context("validate checkpoint")?;
        if self.task(&checkpoint.task_id).is_none() {
            anyhow::bail!("task not found: {}", checkpoint.task_id);
        }
        let Some(run) = self.run(&checkpoint.run_id) else {
            anyhow::bail!("run not found: {}", checkpoint.run_id);
        };
        if run.task_id != checkpoint.task_id {
            anyhow::bail!(
                "checkpoint task {} does not match run {} task {}",
                checkpoint.task_id,
                checkpoint.run_id,
                run.task_id
            );
        }
        if checkpoint.event_sequence > self.events.latest_sequence() {
            anyhow::bail!(
                "checkpoint event sequence {} is ahead of event log {}",
                checkpoint.event_sequence,
                self.events.latest_sequence()
            );
        }
        self.append_record(
            "checkpoint.created",
            Some(&checkpoint.task_id),
            Some(&checkpoint.run_id),
            &checkpoint,
        )?;
        self.checkpoints
            .write()
            .insert(checkpoint.id.clone(), checkpoint.clone());
        let mut runs = self.runs.write();
        if let Some(run) = runs.get_mut(&checkpoint.run_id) {
            let previous = run.clone();
            run.checkpoint_id = Some(checkpoint.id.clone());
            run.updated_at = checkpoint.created_at;
            let updated = run.clone();
            if let Err(err) = self.append_record(
                "run.checkpoint_bound",
                Some(&updated.task_id),
                Some(&updated.id),
                &updated,
            ) {
                runs.insert(updated.id.clone(), previous);
                tracing::warn!(
                    error = %err,
                    run_id = %updated.id,
                    checkpoint_id = %checkpoint.id,
                    "checkpoint binding event failed; checkpoint remains durable"
                );
            }
        }
        drop(runs);
        self.persist_snapshot_best_effort_unlocked();
        Ok(checkpoint)
    }

    pub fn latest_checkpoint(&self, task_id: &str) -> Option<CheckpointRecord> {
        self.checkpoints
            .read()
            .values()
            .filter(|checkpoint| checkpoint.task_id == task_id)
            .max_by_key(|checkpoint| checkpoint.event_sequence)
            .cloned()
    }

    pub fn event_store(&self) -> &std::sync::Arc<dyn EventStore> {
        &self.events
    }

    fn append_record<T: Serialize>(
        &self,
        event_type: &str,
        task_id: Option<&str>,
        run_id: Option<&str>,
        value: &T,
    ) -> Result<EventEnvelope> {
        let mut event = EventEnvelope::from_serializable(event_type, value)
            .map_err(|e| anyhow::anyhow!("serialize {event_type}: {e}"))?;
        if let Some(task_id) = task_id {
            event = event.with_task(task_id.to_string());
        }
        if let Some(run_id) = run_id {
            event = event.with_run(run_id.to_string());
        }
        let persisted = self.events.append(event)?;
        self.projection_sequence
            .store(persisted.sequence, Ordering::Release);
        if let Some(notifier) = self.event_notify.read().as_ref() {
            notifier.notify_waiters();
        }
        Ok(persisted)
    }

    pub fn snapshot(&self) -> TaskStoreSnapshot {
        let _projection_guard = self.projection_guard.lock();
        self.snapshot_unlocked()
    }

    fn snapshot_unlocked(&self) -> TaskStoreSnapshot {
        let mut tasks: Vec<_> = self.tasks.read().values().cloned().collect();
        let mut runs: Vec<_> = self.runs.read().values().cloned().collect();
        let mut checkpoints: Vec<_> = self.checkpoints.read().values().cloned().collect();
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        runs.sort_by(|left, right| left.id.cmp(&right.id));
        checkpoints.sort_by(|left, right| left.id.cmp(&right.id));
        TaskStoreSnapshot {
            schema_version: TASK_SNAPSHOT_SCHEMA_VERSION,
            event_sequence: self.projection_sequence.load(Ordering::Acquire),
            tasks,
            runs,
            checkpoints,
        }
    }

    pub fn save_snapshot(&self, path: impl AsRef<Path>) -> Result<()> {
        let _projection_guard = self.projection_guard.lock();
        self.save_snapshot_unlocked(path.as_ref())
    }

    fn save_snapshot_unlocked(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create task snapshot directory {}", parent.display()))?;
        }
        let snapshot = self.snapshot_unlocked();
        let encoded = serde_json::to_vec_pretty(&snapshot).context("serialize task snapshot")?;
        // Never truncate the last known-good snapshot in place. A process
        // crash between truncate and rename should fall back to replaying the
        // event log, not leave a half-written projection behind.
        let temp = path.with_extension(format!(
            "{}.tmp-{}",
            path.extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("json"),
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)
            .with_context(|| format!("open task snapshot temp {}", temp.display()))?;
        file.write_all(&encoded).context("write task snapshot")?;
        file.flush().context("flush task snapshot")?;
        file.sync_data().context("sync task snapshot")?;
        drop(file);
        if let Err(first_error) = fs::rename(&temp, path) {
            // Windows does not replace an existing destination with rename.
            // Remove only the validated snapshot target and retry; a corrupt
            // or missing snapshot is always recoverable from the event log.
            if cfg!(windows) && path.is_file() {
                fs::remove_file(path)
                    .with_context(|| format!("remove old task snapshot {}", path.display()))?;
                fs::rename(&temp, path).with_context(|| {
                    format!("replace task snapshot {} after remove", path.display())
                })?;
            } else {
                let _ = fs::remove_file(&temp);
                return Err(first_error)
                    .with_context(|| format!("replace task snapshot {}", path.display()));
            }
        }
        Ok(())
    }

    fn load_snapshot(&self) -> Result<u64> {
        let Some(path) = &self.snapshot_path else {
            return Ok(0);
        };
        if !path.exists() {
            return Ok(0);
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "ignoring unreadable task snapshot");
                return Ok(0);
            }
        };
        let mut snapshot: TaskStoreSnapshot = match serde_json::from_reader(BufReader::new(file)) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "ignoring corrupt task snapshot");
                return Ok(0);
            }
        };
        if snapshot.schema_version > TASK_SNAPSHOT_SCHEMA_VERSION {
            tracing::warn!(
                schema = snapshot.schema_version,
                supported = TASK_SNAPSHOT_SCHEMA_VERSION,
                path = %path.display(),
                "ignoring newer task snapshot"
            );
            return Ok(0);
        }
        // Version 0 snapshots predate the explicit field. Their payload shape
        // is compatible, so migration only needs to stamp the current version.
        snapshot.schema_version = TASK_SNAPSHOT_SCHEMA_VERSION;
        if snapshot.event_sequence > self.events.latest_sequence() {
            tracing::warn!(
                snapshot_sequence = snapshot.event_sequence,
                event_sequence = self.events.latest_sequence(),
                path = %path.display(),
                "ignoring task snapshot ahead of event log"
            );
            return Ok(0);
        }
        if let Err(error) = validate_snapshot_records(&snapshot) {
            tracing::warn!(
                error = %error,
                path = %path.display(),
                "ignoring invalid task snapshot"
            );
            return Ok(0);
        }
        let snapshot_sequence = snapshot.event_sequence;
        self.tasks.write().extend(
            snapshot
                .tasks
                .into_iter()
                .map(|task| (task.id.clone(), task)),
        );
        self.runs
            .write()
            .extend(snapshot.runs.into_iter().map(|run| (run.id.clone(), run)));
        self.checkpoints.write().extend(
            snapshot
                .checkpoints
                .into_iter()
                .map(|checkpoint| (checkpoint.id.clone(), checkpoint)),
        );
        Ok(snapshot_sequence)
    }

    fn persist_snapshot_best_effort_unlocked(&self) {
        if let Some(path) = &self.snapshot_path {
            if let Err(err) = self.save_snapshot_unlocked(path) {
                tracing::warn!(error = %err, path = %path.display(), "failed to persist task snapshot");
            }
        }
    }

    fn replay_after(&self, sequence: u64) -> Result<()> {
        for event in self.events.read_after(sequence)? {
            match event.event_type.as_str() {
                "task.created" | "task.updated" | "task.state_changed" | "task.requeued" => {
                    let task: TaskRecord = event.payload_as().with_context(|| {
                        format!("decode task event sequence {}", event.sequence)
                    })?;
                    self.tasks.write().insert(task.id.clone(), task);
                }
                "task.recovered" => {
                    let task: TaskRecord = event
                        .payload
                        .get("task")
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "recovered task event sequence {} has no task payload",
                                event.sequence
                            )
                        })
                        .and_then(|payload| {
                            serde_json::from_value(payload).with_context(|| {
                                format!("decode recovered task event {}", event.sequence)
                            })
                        })?;
                    self.tasks.write().insert(task.id.clone(), task);
                }
                "run.started"
                | "run.state_changed"
                | "run.claimed"
                | "run.heartbeat"
                | "run.released"
                | "run.recovered"
                | "run.checkpoint_bound" => {
                    let run: RunRecord = event
                        .payload_as()
                        .with_context(|| format!("decode run event sequence {}", event.sequence))?;
                    self.runs.write().insert(run.id.clone(), run);
                }
                "run.finished" => {
                    let finished: RunFinishedRecord = event.payload_as().with_context(|| {
                        format!("decode finished run event sequence {}", event.sequence)
                    })?;
                    self.tasks
                        .write()
                        .insert(finished.task.id.clone(), finished.task);
                    self.runs
                        .write()
                        .insert(finished.run.id.clone(), finished.run);
                }
                "checkpoint.created" => {
                    let checkpoint: CheckpointRecord = event.payload_as().with_context(|| {
                        format!("decode checkpoint event sequence {}", event.sequence)
                    })?;
                    self.checkpoints
                        .write()
                        .insert(checkpoint.id.clone(), checkpoint);
                }
                _ => {}
            }
            self.projection_sequence
                .store(event.sequence, Ordering::Release);
        }
        Ok(())
    }
}

fn validate_snapshot_records(snapshot: &TaskStoreSnapshot) -> Result<()> {
    let mut task_ids = HashSet::with_capacity(snapshot.tasks.len());
    for task in &snapshot.tasks {
        task.validate().map_err(anyhow::Error::msg)?;
        if !task_ids.insert(task.id.clone()) {
            anyhow::bail!("duplicate task id in snapshot: {}", task.id);
        }
    }
    let mut run_ids = HashSet::with_capacity(snapshot.runs.len());
    for run in &snapshot.runs {
        run.validate().map_err(anyhow::Error::msg)?;
        if !task_ids.contains(&run.task_id) {
            anyhow::bail!(
                "run {} references missing task {} in snapshot",
                run.id,
                run.task_id
            );
        }
        if !run_ids.insert(run.id.clone()) {
            anyhow::bail!("duplicate run id in snapshot: {}", run.id);
        }
    }
    let mut checkpoint_ids = HashSet::with_capacity(snapshot.checkpoints.len());
    for checkpoint in &snapshot.checkpoints {
        checkpoint.validate().map_err(anyhow::Error::msg)?;
        if !task_ids.contains(&checkpoint.task_id) || !run_ids.contains(&checkpoint.run_id) {
            anyhow::bail!("checkpoint {} references missing task/run", checkpoint.id);
        }
        if !checkpoint_ids.insert(checkpoint.id.clone()) {
            anyhow::bail!("duplicate checkpoint id in snapshot: {}", checkpoint.id);
        }
    }
    Ok(())
}

fn task_transition_allowed(current: TaskState, next: TaskState) -> bool {
    if current == next {
        return true;
    }
    match current {
        TaskState::Queued => matches!(
            next,
            TaskState::Running | TaskState::Paused | TaskState::Failed | TaskState::Cancelled
        ),
        TaskState::Running => matches!(
            next,
            TaskState::Queued
                | TaskState::WaitingApproval
                | TaskState::WaitingEvent
                | TaskState::Paused
                | TaskState::Completed
                | TaskState::Failed
                | TaskState::Cancelled
        ),
        TaskState::WaitingApproval | TaskState::WaitingEvent => {
            matches!(
                next,
                TaskState::Queued
                    | TaskState::Running
                    | TaskState::Paused
                    | TaskState::Failed
                    | TaskState::Cancelled
            )
        }
        TaskState::Paused => matches!(
            next,
            TaskState::Queued | TaskState::Running | TaskState::Failed | TaskState::Cancelled
        ),
        TaskState::Failed => matches!(next, TaskState::Queued | TaskState::Cancelled),
        TaskState::Completed | TaskState::Cancelled => false,
    }
}

fn run_transition_allowed(current: RunState, next: RunState) -> bool {
    if current == next {
        return true;
    }
    match current {
        RunState::Starting => matches!(
            next,
            RunState::Running | RunState::Completed | RunState::Failed | RunState::Cancelled
        ),
        RunState::Running => matches!(
            next,
            RunState::WaitingApproval
                | RunState::WaitingEvent
                | RunState::Paused
                | RunState::Completed
                | RunState::Failed
                | RunState::Cancelled
        ),
        RunState::WaitingApproval | RunState::WaitingEvent => {
            matches!(
                next,
                RunState::Running | RunState::Paused | RunState::Cancelled
            )
        }
        RunState::Paused => matches!(next, RunState::Running | RunState::Cancelled),
        RunState::Failed => matches!(next, RunState::Starting | RunState::Cancelled),
        RunState::Completed | RunState::Cancelled | RunState::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_protocol::{EventSource, GoalSpec};
    use std::sync::Arc;
    use uuid::Uuid;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-rust-p0-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn jsonl_store_assigns_sequences_and_reopens() {
        let dir = temp_path("events");
        let path = dir.join("events.jsonl");
        let store = JsonlEventStore::open(&path).expect("open");
        let first = store
            .append(EventEnvelope::new("test.one", serde_json::json!({"n": 1})))
            .expect("append first");
        let second = store
            .append(
                EventEnvelope::new("test.two", serde_json::json!({"n": 2}))
                    .with_source(EventSource::System),
            )
            .expect("append second");
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(store.read_after(1).expect("read after").len(), 1);
        drop(store);

        let reopened = JsonlEventStore::open(&path).expect("reopen");
        assert_eq!(reopened.latest_sequence(), 2);
        let third = reopened
            .append(EventEnvelope::new("test.three", serde_json::json!({})))
            .expect("append third");
        assert_eq!(third.sequence, 3);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reopening_repairs_a_complete_unterminated_jsonl_record() {
        let dir = temp_path("unterminated-complete");
        let path = dir.join("events.jsonl");
        let store = JsonlEventStore::open(&path).expect("open");
        let first = store
            .append(EventEnvelope::new(
                "test.complete",
                serde_json::json!({"ok": true}),
            ))
            .expect("append first");
        drop(store);

        let mut bytes = fs::read(&path).expect("read event log");
        assert_eq!(bytes.pop(), Some(b'\n'));
        fs::write(&path, bytes).expect("remove final newline");

        let reopened = JsonlEventStore::open(&path).expect("reopen");
        assert_eq!(reopened.read_all().expect("read repaired").len(), 1);
        let second = reopened
            .append(EventEnvelope::new("test.after", serde_json::json!({})))
            .expect("append after repair");
        assert_eq!(first.sequence + 1, second.sequence);
        let events = reopened.read_all().expect("read final log");
        assert_eq!(events.len(), 2);
        let raw = fs::read(&path).expect("read final bytes");
        assert!(raw.windows(2).any(|pair| pair == b"}\n"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn task_projection_replays_state_and_checkpoint() {
        let dir = temp_path("tasks");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events.clone()).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("run checks")).expect("task"))
            .expect("create task");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("start task");
        let run = store.create_run(&task.id, 1).expect("run");
        store
            .transition_run(&run.id, RunState::Running)
            .expect("run running");
        let checkpoint = store
            .save_checkpoint(CheckpointRecord::new(
                task.id.clone(),
                run.id.clone(),
                events.latest_sequence(),
            ))
            .expect("checkpoint");
        assert_eq!(store.latest_checkpoint(&task.id).unwrap().id, checkpoint.id);
        drop(store);

        let reopened_events: Arc<dyn EventStore> =
            Arc::new(JsonlEventStore::open(&path).expect("reopen events"));
        let reopened = TaskStore::open(reopened_events).expect("replay");
        assert_eq!(reopened.task(&task.id).unwrap().state, TaskState::Running);
        assert_eq!(reopened.run(&run.id).unwrap().state, RunState::Running);
        assert_eq!(
            reopened.latest_checkpoint(&task.id).unwrap().id,
            checkpoint.id
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn terminal_task_cannot_be_reopened() {
        let dir = temp_path("transitions");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("finish")).expect("task"))
            .expect("create task");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        store
            .transition_task(&task.id, TaskState::Completed)
            .expect("completed");
        assert!(store.transition_task(&task.id, TaskState::Running).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn orphaned_worker_run_is_requeued_immediately() {
        let dir = temp_path("orphan-recovery");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("recover me")).expect("task"))
            .expect("create task");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        let run = store.create_run(&task.id, 1).expect("run");
        store
            .claim_run(&run.id, "worker-old", chrono::Utc::now())
            .expect("claim");

        let recovered = store
            .recover_orphaned_runs(chrono::Utc::now(), "worker-new")
            .expect("recover");
        assert_eq!(recovered.len(), 1);
        assert_eq!(store.run(&run.id).unwrap().state, RunState::Failed);
        assert_eq!(store.task(&task.id).unwrap().state, TaskState::Queued);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exhausted_recovered_run_marks_task_failed_instead_of_requeueing() {
        let dir = temp_path("exhausted-recovery");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events.clone()).expect("task store");
        let task = store
            .create_task(
                TaskRecord::new_with_retry_policy(
                    GoalSpec::new("do not retry"),
                    dsh_protocol::RetryPolicy {
                        max_attempts: 1,
                        backoff_secs: 0,
                        max_backoff_secs: 0,
                    },
                )
                .expect("task"),
            )
            .expect("create");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        let run = store.create_run(&task.id, 1).expect("run");
        store
            .claim_run(&run.id, "worker-old", Utc::now())
            .expect("claim");
        store
            .recover_orphaned_runs(Utc::now(), "worker-new")
            .expect("recover");
        assert_eq!(store.task(&task.id).expect("task").state, TaskState::Failed);
        assert!(events
            .read_all()
            .expect("events")
            .iter()
            .any(|event| event.event_type == "task.recovered"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finishing_a_run_commits_task_and_run_in_one_event() {
        let dir = temp_path("atomic-finish");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events.clone()).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("finish atomically")).expect("task"))
            .expect("create");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        let run = store.create_run(&task.id, 1).expect("run");
        store
            .claim_run(&run.id, "worker-atomic", Utc::now())
            .expect("claim");
        let (_, finished) = store
            .finish_run(
                &run.id,
                "worker-atomic",
                TaskState::Completed,
                RunState::Completed,
                Utc::now(),
            )
            .expect("finish");
        assert_eq!(finished.state, RunState::Completed);
        assert_eq!(
            store.task(&task.id).expect("task").state,
            TaskState::Completed
        );
        let events = events.read_all().expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "run.finished")
                .count(),
            1
        );

        let reopened_events: Arc<dyn EventStore> =
            Arc::new(JsonlEventStore::open(&path).expect("reopen events"));
        let reopened = TaskStore::open(reopened_events).expect("replay");
        assert_eq!(
            reopened.task(&task.id).expect("replayed task").state,
            TaskState::Completed
        );
        assert_eq!(
            reopened.run(&run.id).expect("replayed run").state,
            RunState::Completed
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finishing_after_user_cancellation_reconciles_the_run_state() {
        let dir = temp_path("finish-cancel-race");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("cancel race")).expect("task"))
            .expect("create");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        let run = store.create_run(&task.id, 1).expect("run");
        store
            .claim_run(&run.id, "worker-race", Utc::now())
            .expect("claim");
        store
            .transition_task(&task.id, TaskState::Cancelled)
            .expect("cancel");
        let (_, reconciled) = store
            .finish_run(
                &run.id,
                "worker-race",
                TaskState::Completed,
                RunState::Completed,
                Utc::now(),
            )
            .expect("reconcile");
        assert_eq!(reconciled.state, RunState::Cancelled);
        assert_eq!(
            store.task(&task.id).expect("task").state,
            TaskState::Cancelled
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_appends_keep_file_order_and_sequences() {
        let dir = temp_path("concurrent-events");
        let path = dir.join("events.jsonl");
        let store = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let mut workers = Vec::new();
        for worker in 0..8 {
            let store = store.clone();
            workers.push(std::thread::spawn(move || {
                for item in 0..25 {
                    store
                        .append(EventEnvelope::new(
                            "test.concurrent",
                            serde_json::json!({"worker": worker, "item": item}),
                        ))
                        .expect("append");
                }
            }));
        }
        for worker in workers {
            worker.join().expect("worker");
        }
        let events = store.read_all().expect("read");
        assert_eq!(events.len(), 200);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn separate_store_instances_keep_sequences_unique() {
        let dir = temp_path("cross-process-events");
        let path = dir.join("events.jsonl");
        let first = Arc::new(JsonlEventStore::open(&path).expect("open first"));
        let second = Arc::new(JsonlEventStore::open(&path).expect("open second"));
        let mut workers = Vec::new();
        for (store, label) in [(first.clone(), "first"), (second.clone(), "second")] {
            workers.push(std::thread::spawn(move || {
                for item in 0..40 {
                    store
                        .append(EventEnvelope::new(
                            "test.cross_instance",
                            serde_json::json!({"store": label, "item": item}),
                        ))
                        .expect("append");
                }
            }));
        }
        for worker in workers {
            worker.join().expect("worker");
        }
        let events = first.read_all().expect("read");
        assert_eq!(events.len(), 80);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reopening_rejects_duplicate_sequences() {
        let dir = temp_path("duplicate-sequences");
        let path = dir.join("events.jsonl");
        fs::create_dir_all(&dir).expect("directory");
        let mut first = EventEnvelope::new("test.one", serde_json::json!({}));
        first.sequence = 1;
        let mut duplicate = EventEnvelope::new("test.two", serde_json::json!({}));
        duplicate.sequence = 1;
        let content = format!(
            "{}\n{}\n",
            serde_json::to_string(&first).expect("first"),
            serde_json::to_string(&duplicate).expect("duplicate")
        );
        fs::write(&path, content).expect("write");
        assert!(JsonlEventStore::open(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn append_rejects_sequence_overflow_instead_of_reusing_maximum() {
        let dir = temp_path("sequence-overflow");
        let path = dir.join("events.jsonl");
        fs::create_dir_all(&dir).expect("directory");
        let mut event = EventEnvelope::new("test.max", serde_json::json!({}));
        event.sequence = u64::MAX;
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&event).expect("event")),
        )
        .expect("write");
        let store = JsonlEventStore::open(&path).expect("open");
        assert!(store
            .append(EventEnvelope::new("test.after_max", serde_json::json!({})))
            .is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reopening_repairs_an_unterminated_jsonl_tail() {
        let dir = temp_path("partial-tail");
        let path = dir.join("events.jsonl");
        let store = JsonlEventStore::open(&path).expect("open");
        let event = store
            .append(EventEnvelope::new(
                "test.complete",
                serde_json::json!({"ok": true}),
            ))
            .expect("append");
        drop(store);
        let mut bytes = fs::read(&path).expect("read log");
        let partial = serde_json::to_vec(&EventEnvelope::new(
            "test.partial",
            serde_json::json!({"truncated": true}),
        ))
        .expect("serialize partial");
        bytes.extend_from_slice(&partial[..partial.len().saturating_sub(5)]);
        fs::write(&path, bytes).expect("write torn tail");

        let reopened = JsonlEventStore::open(&path).expect("repair and reopen");
        assert_eq!(reopened.latest_sequence(), event.sequence);
        assert_eq!(reopened.read_all().expect("read repaired").len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn task_snapshot_rehydrates_projection() {
        let dir = temp_path("task-snapshot");
        let path = dir.join("events.jsonl");
        let snapshot_path = dir.join("tasks.snapshot.json");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store =
            TaskStore::open_with_snapshot(events.clone(), &snapshot_path).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("snapshot me")).expect("task"))
            .expect("create");
        store
            .update_task(&task.id, |task| {
                task.goal.verification.push("file_exists:result.txt".into());
            })
            .expect("update verification");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        assert!(snapshot_path.exists());
        drop(store);

        let reopened_events: Arc<dyn EventStore> =
            Arc::new(JsonlEventStore::open(&path).expect("reopen events"));
        let reopened = TaskStore::open_with_snapshot(reopened_events, &snapshot_path)
            .expect("reopen task store");
        assert_eq!(reopened.task(&task.id).unwrap().state, TaskState::Running);
        assert_eq!(
            reopened.task(&task.id).unwrap().goal.verification,
            vec!["file_exists:result.txt"]
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn snapshot_cursor_does_not_skip_unreplayed_external_events() {
        let dir = temp_path("projection-cursor");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events.clone()).expect("store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("cursor")).expect("task"))
            .expect("create");
        let mut external = task.clone();
        external.state = TaskState::Running;
        external.updated_at = Utc::now();
        events
            .append(
                EventEnvelope::from_serializable("task.state_changed", &external)
                    .expect("event")
                    .with_task(task.id.clone()),
            )
            .expect("external append");
        // Refresh the concrete event-store cursor without replaying the
        // external task event into this projection.
        let _ = events.read_all().expect("refresh");
        let snapshot = store.snapshot();
        assert_eq!(snapshot.event_sequence, 1);
        assert_eq!(store.task(&task.id).expect("task").state, TaskState::Queued);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn running_task_can_be_requeued_and_replayed_after_scheduler_recovery() {
        let dir = temp_path("scheduler-stranded");
        let path = dir.join("events.jsonl");
        let events: Arc<dyn EventStore> = Arc::new(JsonlEventStore::open(&path).expect("open"));
        let store = TaskStore::open(events.clone()).expect("task store");
        let task = store
            .create_task(TaskRecord::new(GoalSpec::new("stranded")).expect("task"))
            .expect("create");
        store
            .transition_task(&task.id, TaskState::Running)
            .expect("running");
        store
            .transition_task(&task.id, TaskState::Queued)
            .expect("requeue");
        let scheduler_event = EventEnvelope::new(
            "task.scheduler_requeued",
            serde_json::json!({"reason": "scheduler_restart", "task_id": task.id}),
        )
        .with_source(EventSource::Scheduler)
        .with_task(task.id.clone());
        events.append(scheduler_event).expect("scheduler event");
        drop(store);

        let reopened_events: Arc<dyn EventStore> =
            Arc::new(JsonlEventStore::open(&path).expect("reopen events"));
        let reopened = TaskStore::open(reopened_events).expect("replay");
        assert_eq!(
            reopened.task(&task.id).expect("task").state,
            TaskState::Queued
        );
        let _ = fs::remove_dir_all(dir);
    }
}
