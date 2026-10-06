//! Approval queue for retrying auto-denied tool actions (Codex `/approve`).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct DeniedAction {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingApproval {
    pub request_id: String,
    pub call_id: String,
    pub name: String,
    pub summary: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Durable correlation scope carried by an approval request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalScope {
    pub task_id: Option<String>,
    pub run_id: Option<String>,
}

impl ApprovalScope {
    pub fn new(task_id: Option<&str>, run_id: Option<&str>) -> Self {
        Self {
            task_id: task_id.map(str::to_string),
            run_id: run_id.map(str::to_string),
        }
    }
}

impl PendingApproval {
    pub fn new(
        call_id: impl Into<String>,
        name: impl Into<String>,
        summary: impl Into<String>,
        task_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            call_id: call_id.into(),
            name: name.into(),
            summary: summary.into(),
            task_id: task_id.map(str::to_string),
            run_id: run_id.map(str::to_string),
            created_at: chrono::Utc::now(),
        }
    }
}

struct PendingEntry {
    request: PendingApproval,
    reply: oneshot::Sender<bool>,
}

#[derive(Default)]
pub struct ApprovalQueue {
    last: Mutex<Option<DeniedAction>>,
    pending: Mutex<HashMap<String, PendingEntry>>,
}

impl ApprovalQueue {
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn record_denial(&self, action: DeniedAction) {
        *self.last.lock() = Some(action);
    }

    pub fn take_last(&self) -> Option<DeniedAction> {
        self.last.lock().take()
    }

    pub fn peek(&self) -> Option<DeniedAction> {
        self.last.lock().clone()
    }

    pub fn enqueue(&self, request: PendingApproval) -> oneshot::Receiver<bool> {
        let (reply, receiver) = oneshot::channel();
        self.pending
            .lock()
            .insert(request.request_id.clone(), PendingEntry { request, reply });
        receiver
    }

    pub fn resolve(&self, request_id: &str, allow: bool) -> bool {
        self.resolve_entry(request_id, allow).is_some()
    }

    /// Resolve a request and return its metadata for durable audit events.
    pub fn resolve_entry(&self, request_id: &str, allow: bool) -> Option<PendingApproval> {
        let entry = self.pending.lock().remove(request_id)?;
        let request = entry.request;
        let _ = entry.reply.send(allow);
        Some(request)
    }

    pub fn resolve_next(&self, allow: bool) -> bool {
        self.resolve_next_entry(allow).is_some()
    }

    /// Resolve the oldest pending request and return its metadata.
    pub fn resolve_next_entry(&self, allow: bool) -> Option<PendingApproval> {
        let request_id = self
            .pending
            .lock()
            .values()
            .min_by(|left, right| {
                left.request
                    .created_at
                    .cmp(&right.request.created_at)
                    .then_with(|| left.request.request_id.cmp(&right.request.request_id))
            })
            .map(|entry| entry.request.request_id.clone());
        request_id
            .as_deref()
            .and_then(|id| self.resolve_entry(id, allow))
    }

    pub fn cancel(&self, request_id: &str) -> Option<PendingApproval> {
        self.pending
            .lock()
            .remove(request_id)
            .map(|entry| entry.request)
    }

    pub fn pending(&self) -> Vec<PendingApproval> {
        let mut pending: Vec<_> = self
            .pending
            .lock()
            .values()
            .map(|entry| entry.request.clone())
            .collect();
        pending.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broker_resolves_oldest_pending_request() {
        let queue = ApprovalQueue::new();
        let mut first = PendingApproval::new("call-1", "shell", "one", None, None);
        let mut second = PendingApproval::new("call-2", "write_file", "two", None, None);
        first.created_at = chrono::Utc::now() - chrono::Duration::seconds(2);
        second.created_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        let first_rx = queue.enqueue(first.clone());
        let second_rx = queue.enqueue(second.clone());
        assert_eq!(queue.pending().len(), 2);
        assert!(queue.resolve_next(true));
        assert!(first_rx.await.expect("first reply"));
        assert!(queue.resolve(&second.request_id, false));
        assert!(!second_rx.await.expect("second reply"));
        assert!(queue.pending().is_empty());
    }

    #[tokio::test]
    async fn cancel_returns_request_metadata_and_closes_waiter() {
        let queue = ApprovalQueue::new();
        let request = PendingApproval::new("call-1", "shell", "{}", Some("task-1"), Some("run-1"));
        let request_id = request.request_id.clone();
        let receiver = queue.enqueue(request.clone());
        assert_eq!(queue.cancel(&request_id), Some(request));
        assert!(receiver.await.is_err());
        assert!(queue.pending().is_empty());
    }

    #[test]
    fn approval_scope_keeps_task_and_run_correlation() {
        let scope = ApprovalScope::new(Some("task-1"), Some("run-1"));
        assert_eq!(scope.task_id.as_deref(), Some("task-1"));
        assert_eq!(scope.run_id.as_deref(), Some("run-1"));
    }
}
