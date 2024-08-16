//! Approval queue for retrying auto-denied tool actions (Codex `/approve`).

use parking_lot::Mutex;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct DeniedAction {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub reason: String,
}

#[derive(Default)]
pub struct ApprovalQueue {
    last: Mutex<Option<DeniedAction>>,
}

impl ApprovalQueue {
    pub fn new() -> Self {
        Self::default()
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
}
