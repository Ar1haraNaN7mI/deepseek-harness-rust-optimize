use chrono::{DateTime, Utc};
use dsh_llm::ChatMessage;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use uuid::Uuid;

pub const SESSION_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStart {
        id: String,
        at: DateTime<Utc>,
    },
    TurnEnd {
        id: String,
        at: DateTime<Utc>,
    },
    StepStart {
        id: String,
        turn_id: String,
        at: DateTime<Utc>,
    },
    StepEnd {
        id: String,
        turn_id: String,
        at: DateTime<Utc>,
    },
    UserMessage {
        id: String,
        text: String,
        at: DateTime<Utc>,
    },
    AssistantMessage {
        id: String,
        text: String,
        reasoning: Option<String>,
        at: DateTime<Utc>,
    },
    /// Live/UI fidelity only — stripped from durable disk snapshots.
    AssistantChunk {
        id: String,
        text: String,
        at: DateTime<Utc>,
    },
    ReasoningChunk {
        id: String,
        text: String,
        at: DateTime<Utc>,
    },
    ToolCall {
        id: String,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
        at: DateTime<Utc>,
    },
    ToolResult {
        id: String,
        call_id: String,
        name: String,
        ok: bool,
        content: String,
        at: DateTime<Utc>,
    },
    SystemNote {
        id: String,
        text: String,
        at: DateTime<Utc>,
    },
}

impl SessionEvent {
    fn is_live_chunk(&self) -> bool {
        matches!(
            self,
            SessionEvent::AssistantChunk { .. } | SessionEvent::ReasoningChunk { .. }
        )
    }

    pub fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            SessionEvent::TurnStart { at, .. }
            | SessionEvent::TurnEnd { at, .. }
            | SessionEvent::StepStart { at, .. }
            | SessionEvent::StepEnd { at, .. }
            | SessionEvent::UserMessage { at, .. }
            | SessionEvent::AssistantMessage { at, .. }
            | SessionEvent::AssistantChunk { at, .. }
            | SessionEvent::ReasoningChunk { at, .. }
            | SessionEvent::ToolCall { at, .. }
            | SessionEvent::ToolResult { at, .. }
            | SessionEvent::SystemNote { at, .. } => *at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default = "default_session_schema_version")]
    pub schema_version: u16,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub goal_paused: bool,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub events: Vec<SessionEvent>,
}

fn default_session_schema_version() -> u16 {
    SESSION_SCHEMA_VERSION
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            name: None,
            archived: false,
            goal: None,
            goal_paused: false,
            personality: None,
            cwd: None,
            events: Vec::new(),
        }
    }

    pub fn with_id(id: String) -> Self {
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id,
            name: None,
            archived: false,
            goal: None,
            goal_paused: false,
            personality: None,
            cwd: None,
            events: Vec::new(),
        }
    }

    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.id[..8.min(self.id.len())].to_string())
    }

    pub fn rename(&mut self, name: impl Into<String>) {
        let n = name.into().trim().to_string();
        self.name = if n.is_empty() { None } else { Some(n) };
    }

    pub fn append(&mut self, event: SessionEvent) {
        self.events.push(event);
    }

    pub fn last_activity_at(&self) -> Option<DateTime<Utc>> {
        self.events.iter().map(SessionEvent::occurred_at).max()
    }

    /// Compact transcript: keep recent durable events + a summary note (Codex /compact).
    pub fn compact(&mut self, keep_last: usize) -> String {
        let lines = self.transcript_lines();
        let summary: String = if lines.is_empty() {
            "(empty session)".into()
        } else {
            lines
                .iter()
                .rev()
                .take(24)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        let keep = keep_last.max(8);
        if self.events.len() > keep {
            let drain = self.events.len() - keep;
            self.events.drain(0..drain);
        }
        let note = format!(
            "[compact] retained last {keep} events. Prior context digest:\n{}",
            summary.chars().take(2500).collect::<String>()
        );
        self.events.insert(
            0,
            SessionEvent::SystemNote {
                id: Uuid::new_v4().to_string(),
                text: note.clone(),
                at: Utc::now(),
            },
        );
        note
    }

    /// Fork this session into a new id with copied events.
    pub fn fork_clone(&self) -> Session {
        Session {
            schema_version: SESSION_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            name: self.name.as_ref().map(|n| format!("{n} (fork)")),
            archived: false,
            goal: self.goal.clone(),
            goal_paused: self.goal_paused,
            personality: self.personality.clone(),
            cwd: self.cwd.clone(),
            events: self.events.clone(),
        }
    }

    /// Truncate events after the last matching user message (EscEsc edit/fork).
    pub fn fork_from_last_user(&self) -> Option<(Session, String)> {
        let idx = self
            .events
            .iter()
            .rposition(|e| matches!(e, SessionEvent::UserMessage { .. }))?;
        let text = match &self.events[idx] {
            SessionEvent::UserMessage { text, .. } => text.clone(),
            _ => return None,
        };
        let mut forked = self.fork_clone();
        forked.events = self.events[..idx].to_vec();
        Some((forked, text))
    }

    /// Project model-visible history from the append-only log.
    /// Chunks are ignored (UI-only); durable assistant/message + tools form the model view.
    pub fn derive_messages(&self, system: &str) -> Vec<ChatMessage> {
        let mut messages = Vec::with_capacity(self.events.len().min(64) + 1);
        messages.push(ChatMessage::system(system));
        for event in &self.events {
            match event {
                SessionEvent::UserMessage { text, .. } => {
                    messages.push(ChatMessage::user(text.clone()));
                }
                SessionEvent::AssistantMessage {
                    text, reasoning, ..
                } => {
                    let mut msg = ChatMessage::assistant(text.clone());
                    msg.reasoning_content = reasoning.clone();
                    messages.push(msg);
                }
                SessionEvent::ToolCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    if let Some(last) = messages.last_mut() {
                        if last.role == dsh_llm::Role::Assistant {
                            let mut calls = last.tool_calls.clone().unwrap_or_default();
                            calls.push(dsh_llm::ToolCallDelta {
                                index: Some(calls.len()),
                                id: Some(call_id.clone()),
                                r#type: Some("function".into()),
                                function: Some(dsh_llm::FunctionCallDelta {
                                    name: Some(name.clone()),
                                    arguments: Some(arguments.to_string()),
                                }),
                            });
                            last.tool_calls = Some(calls);
                            continue;
                        }
                    }
                    let mut msg = ChatMessage::assistant("");
                    msg.tool_calls = Some(vec![dsh_llm::ToolCallDelta {
                        index: Some(0),
                        id: Some(call_id.clone()),
                        r#type: Some("function".into()),
                        function: Some(dsh_llm::FunctionCallDelta {
                            name: Some(name.clone()),
                            arguments: Some(arguments.to_string()),
                        }),
                    }]);
                    messages.push(msg);
                }
                SessionEvent::ToolResult {
                    call_id, content, ..
                } => {
                    messages.push(ChatMessage::tool_result(call_id.clone(), content.clone()));
                }
                _ => {}
            }
        }
        messages
    }

    /// Project a bounded model context for compact/local models.
    ///
    /// The durable session remains complete; this only limits the request
    /// projection. Newest messages are preferred, reasoning text can be
    /// omitted, and leading orphaned tool results are removed so an API does
    /// not receive a tool response without its assistant tool-call message.
    pub fn derive_messages_with_budget(
        &self,
        system: &str,
        max_messages: Option<usize>,
        max_chars: Option<usize>,
        include_reasoning: bool,
    ) -> Vec<ChatMessage> {
        let mut all = self.derive_messages(system);
        if !include_reasoning {
            for message in all.iter_mut() {
                message.reasoning_content = None;
            }
        }
        if max_messages == Some(0) || max_chars == Some(0) {
            return all.into_iter().take(1).collect();
        }
        let Some(max_messages) = max_messages else {
            if max_chars.is_none() {
                return all;
            }
            return trim_messages_by_chars(all, max_chars.unwrap_or(usize::MAX));
        };
        let max_messages = max_messages.max(2);
        let mut selected = Vec::with_capacity(max_messages.min(all.len()));
        if let Some(system_message) = all.first().cloned() {
            selected.push(system_message);
        }
        let mut used_chars = selected.first().map(message_chars).unwrap_or(0);
        let char_budget = max_chars.unwrap_or(usize::MAX).max(1_024);
        let mut tail = Vec::new();
        for message in all.drain(1..).rev() {
            if selected.len() + tail.len() >= max_messages {
                break;
            }
            let chars = message_chars(&message);
            if !tail.is_empty() && used_chars.saturating_add(chars) > char_budget {
                break;
            }
            // Keep at least the newest message even if one tool result is
            // larger than the entire budget; the caller can compact it later.
            used_chars = used_chars.saturating_add(chars);
            tail.push(message);
        }
        tail.reverse();
        selected.extend(tail);
        while selected.len() > 1 && matches!(selected[1].role, dsh_llm::Role::Tool) {
            selected.remove(1);
        }
        if let Some(max_chars) = max_chars {
            trim_messages_by_chars(selected, max_chars)
        } else {
            selected
        }
    }

    pub fn transcript_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for event in &self.events {
            match event {
                SessionEvent::UserMessage { text, .. } => lines.push(format!("You: {text}")),
                SessionEvent::AssistantMessage { text, .. } if !text.is_empty() => {
                    lines.push(format!("Assistant: {text}"));
                }
                SessionEvent::ToolCall {
                    name, arguments, ..
                } => {
                    lines.push(format!("→ tool {name}({arguments})"));
                }
                SessionEvent::ToolResult {
                    name, ok, content, ..
                } => {
                    let status = if *ok { "ok" } else { "err" };
                    let preview: String = content.chars().take(240).collect();
                    lines.push(format!("← {name} [{status}] {preview}"));
                }
                SessionEvent::SystemNote { text, .. } => lines.push(format!("! {text}")),
                _ => {}
            }
        }
        lines
    }

    /// Durable snapshot: omit live chunk events; compact JSON (no pretty).
    pub fn save_json(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let durable = Session {
            schema_version: SESSION_SCHEMA_VERSION,
            id: self.id.clone(),
            name: self.name.clone(),
            archived: self.archived,
            goal: self.goal.clone(),
            goal_paused: self.goal_paused,
            personality: self.personality.clone(),
            cwd: self.cwd.clone(),
            events: self
                .events
                .iter()
                .filter(|e| !e.is_live_chunk())
                .cloned()
                .collect(),
        };
        let text = serde_json::to_string(&durable)?;
        // Atomic-ish write via temp file.
        let tmp = path.with_extension(format!("json.tmp.{}", Uuid::new_v4()));
        fs::write(&tmp, text)?;
        if let Err(err) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(err.into());
        }
        Ok(())
    }

    pub fn load_json(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

fn message_chars(message: &ChatMessage) -> usize {
    let mut count = message.text().chars().count();
    if let Some(reasoning) = &message.reasoning_content {
        count = count.saturating_add(reasoning.chars().count());
    }
    if let Some(calls) = &message.tool_calls {
        count = count.saturating_add(
            serde_json::to_string(calls)
                .map(|value| value.chars().count())
                .unwrap_or(0),
        );
    }
    count
}

fn trim_messages_by_chars(mut messages: Vec<ChatMessage>, max_chars: usize) -> Vec<ChatMessage> {
    if messages.len() <= 1 || max_chars == 0 {
        return messages;
    }
    let max_chars = max_chars.max(1_024);
    let system = messages.remove(0);
    let mut selected = vec![system];
    let mut used = message_chars(&selected[0]);
    let mut tail = Vec::new();
    for message in messages.into_iter().rev() {
        let chars = message_chars(&message);
        if !tail.is_empty() && used.saturating_add(chars) > max_chars {
            break;
        }
        used = used.saturating_add(chars);
        tail.push(message);
    }
    tail.reverse();
    selected.extend(tail);
    while selected.len() > 1 && matches!(selected[1].role, dsh_llm::Role::Tool) {
        selected.remove(1);
    }
    selected
}

pub struct SessionStore {
    sessions: RwLock<HashMap<String, Arc<RwLock<Session>>>>,
    dir: Option<PathBuf>,
    dirty: RwLock<HashMap<String, Arc<RwLock<Session>>>>,
    flush_scheduled: AtomicBool,
    // Serialize durable writes with management operations so a delayed flush
    // cannot recreate a successfully deleted conversation.
    io_lock: parking_lot::Mutex<()>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            dir: None,
            dirty: RwLock::new(HashMap::new()),
            flush_scheduled: AtomicBool::new(false),
            io_lock: parking_lot::Mutex::new(()),
        }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            sessions: RwLock::new(HashMap::new()),
            dir: Some(dir),
            dirty: RwLock::new(HashMap::new()),
            flush_scheduled: AtomicBool::new(false),
            io_lock: parking_lot::Mutex::new(()),
        }
    }

    pub fn create(&self) -> Arc<RwLock<Session>> {
        let session = Arc::new(RwLock::new(Session::new()));
        let id = session.read().id.clone();
        self.sessions.write().insert(id, session.clone());
        self.persist_now(&session);
        session
    }

    /// Insert an already-built session (e.g. fork) into the store.
    pub fn insert(&self, session: Session) -> Arc<RwLock<Session>> {
        let id = session.id.clone();
        let session = Arc::new(RwLock::new(session));
        self.sessions.write().insert(id, session.clone());
        self.persist_now(&session);
        session
    }

    pub fn latest_id(&self) -> Option<String> {
        self.latest_by_activity(|_| true)
    }

    pub fn get_or_load(&self, id: &str) -> anyhow::Result<Arc<RwLock<Session>>> {
        if let Some(s) = self.get(id) {
            return Ok(s);
        }
        let _io = self.io_lock.lock();
        if let Some(s) = self.get(id) {
            return Ok(s);
        }
        if let Some(dir) = &self.dir {
            let path = dir.join(format!("{id}.json"));
            if path.exists() {
                let session = Session::load_json(&path)?;
                let session = Arc::new(RwLock::new(session));
                self.sessions
                    .write()
                    .insert(id.to_string(), session.clone());
                return Ok(session);
            }
        }
        anyhow::bail!("session not found: {id}")
    }

    pub fn get(&self, id: &str) -> Option<Arc<RwLock<Session>>> {
        self.sessions.read().get(id).cloned()
    }

    pub fn mark_dirty(&self, session: &Arc<RwLock<Session>>) {
        let id = session.read().id.clone();
        self.dirty.write().insert(id, session.clone());
    }

    /// Immediate durable write (turn end / cancel).
    pub fn persist_now(&self, session: &Arc<RwLock<Session>>) {
        if let Err(err) = self.persist_now_result(session) {
            tracing::error!(error = %err, "failed to persist session");
        }
    }

    pub fn persist_now_result(&self, session: &Arc<RwLock<Session>>) -> anyhow::Result<()> {
        let _io = self.io_lock.lock();
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let snap = session.read().clone();
        if !self
            .sessions
            .read()
            .get(&snap.id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            anyhow::bail!("session is no longer in this store: {}", snap.id);
        }
        let path = dir.join(format!("{}.json", snap.id));
        snap.save_json(&path)?;
        self.dirty.write().remove(&snap.id);
        Ok(())
    }

    /// Flush all dirty sessions (called between steps / turn end).
    pub fn flush_dirty(&self) {
        let pending: Vec<_> = self.dirty.write().drain().map(|(_, s)| s).collect();
        for session in pending {
            self.persist_now(&session);
        }
        self.flush_scheduled.store(false, Ordering::Relaxed);
    }

    /// Backward-compatible alias — prefers deferred mark when possible.
    pub fn persist(&self, session: &Arc<RwLock<Session>>) {
        self.mark_dirty(session);
        self.flush_dirty();
    }

    pub fn list_ids(&self) -> Vec<String> {
        self.try_list_ids().unwrap_or_else(|_| {
            let mut ids: Vec<_> = self.sessions.read().keys().cloned().collect();
            ids.sort();
            ids
        })
    }

    /// Management callers must not mistake an unreadable store for an empty one.
    pub fn try_list_ids(&self) -> anyhow::Result<Vec<String>> {
        let mut ids: Vec<_> = self.sessions.read().keys().cloned().collect();
        if let Some(dir) = &self.dir {
            match fs::read_dir(dir) {
                Ok(entries) => {
                    for entry in entries {
                        let e = entry?;
                        if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
                            if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                                if !ids.iter().any(|i| i == stem) {
                                    ids.push(stem.to_string());
                                }
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn resolve(&self, query: &str) -> anyhow::Result<Arc<RwLock<Session>>> {
        if let Ok(s) = self.get_or_load(query) {
            return Ok(s);
        }
        // Match by name or id prefix.
        for id in self.list_ids() {
            if let Ok(s) = self.get_or_load(&id) {
                let snap = s.read();
                if snap.id.starts_with(query)
                    || snap
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(query))
                {
                    return Ok(s.clone());
                }
            }
        }
        anyhow::bail!("session not found: {query}")
    }

    pub fn list_summaries(&self, include_archived: bool) -> Vec<(String, String, bool)> {
        let mut out: Vec<(DateTime<Utc>, String, String, bool)> = Vec::new();
        for id in self.list_ids() {
            if let Ok(s) = self.get_or_load(&id) {
                let snap = s.read();
                if snap.archived && !include_archived {
                    continue;
                }
                out.push((
                    snap.last_activity_at().unwrap_or_else(Utc::now),
                    snap.id.clone(),
                    snap.display_name(),
                    snap.archived,
                ));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        out.into_iter()
            .map(|(_, id, name, archived)| (id, name, archived))
            .collect()
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        self.commit_change(&session, |snapshot| snapshot.archived = archived)
    }

    pub fn rename(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        self.commit_change(&session, |snapshot| snapshot.rename(name))
    }

    fn commit_change(
        &self,
        session: &Arc<RwLock<Session>>,
        change: impl FnOnce(&mut Session),
    ) -> anyhow::Result<()> {
        let _io = self.io_lock.lock();
        let mut current = session.write();
        if !self
            .sessions
            .read()
            .get(&current.id)
            .is_some_and(|entry| Arc::ptr_eq(entry, session))
        {
            anyhow::bail!("session is no longer in this store: {}", current.id);
        }
        let mut next = current.clone();
        change(&mut next);
        if let Some(dir) = &self.dir {
            next.save_json(&dir.join(format!("{}.json", next.id)))?;
        }
        self.dirty.write().remove(&next.id);
        *current = next;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        let _io = self.io_lock.lock();
        let real_id = session.read().id.clone();
        if let Some(dir) = &self.dir {
            let path = dir.join(format!("{real_id}.json"));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.sessions.write().remove(&real_id);
        self.dirty.write().remove(&real_id);
        Ok(())
    }

    pub fn latest_active_id(&self) -> Option<String> {
        self.latest_by_activity(|session| !session.archived)
    }

    fn latest_by_activity<F>(&self, include: F) -> Option<String>
    where
        F: Fn(&Session) -> bool,
    {
        let mut best: Option<(DateTime<Utc>, String)> = None;
        for id in self.list_ids() {
            let Ok(session) = self.get_or_load(&id) else {
                continue;
            };
            let snapshot = session.read();
            if !include(&snapshot) {
                continue;
            }
            let at = snapshot.last_activity_at().unwrap_or_else(Utc::now);
            let replace = match &best {
                None => true,
                Some((best_at, best_id)) => {
                    at > *best_at || (at == *best_at && snapshot.id > *best_id)
                }
            };
            if replace {
                best = Some((at, snapshot.id.clone()));
            }
        }
        best.map(|(_, id)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ManagementFixture(PathBuf);

    impl ManagementFixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("dsh-session-management-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for ManagementFixture {
        fn drop(&mut self) {
            let temp = std::env::temp_dir().canonicalize().unwrap();
            let path = self.0.canonicalize().unwrap();
            assert_eq!(path.parent(), Some(temp.as_path()));
            assert!(path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-session-management-"));
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn management_disk_errors_do_not_change_in_memory_session() {
        let fixture = ManagementFixture::new();
        let store = SessionStore::with_dir(fixture.0.clone());
        let session = store.create();
        let id = session.read().id.clone();
        let path = fixture.0.join(format!("{id}.json"));
        let original = fixture.0.join("original.backup");
        fs::rename(&path, &original).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.set_archived(&id, true).is_err());
        assert!(!session.read().archived);
        assert!(store.rename(&id, "new name").is_err());
        assert!(session.read().name.is_none());
        assert!(store.delete(&id).is_err());
        assert!(store.get(&id).is_some());
        fs::remove_dir(&path).unwrap();
        fs::rename(&original, &path).unwrap();
        store.set_archived(&id, true).unwrap();
        assert!(Session::load_json(&path).unwrap().archived);
    }

    #[test]
    fn deleted_session_cannot_be_recreated_by_a_delayed_flush() {
        let fixture = ManagementFixture::new();
        let store = SessionStore::with_dir(fixture.0.clone());
        let session = store.create();
        let id = session.read().id.clone();
        store.mark_dirty(&session);
        store.delete(&id).unwrap();
        store.mark_dirty(&session);
        store.flush_dirty();
        assert!(store.persist_now_result(&session).is_err());
        assert!(!fixture.0.join(format!("{id}.json")).exists());
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn old_session_json_gets_schema_default() {
        let value = serde_json::json!({
            "id": "legacy",
            "events": []
        });
        let session: Session = serde_json::from_value(value).expect("legacy session");
        assert_eq!(session.schema_version, SESSION_SCHEMA_VERSION);
    }

    #[test]
    fn latest_session_uses_activity_time_not_id_order() {
        let dir = std::env::temp_dir().join(format!("dsh-rust-session-{}", Uuid::new_v4()));
        let store = SessionStore::with_dir(dir.clone());
        let first = store.create();
        let second = store.create();
        let older = Utc::now() - chrono::Duration::minutes(2);
        let newer = Utc::now();
        first.write().append(SessionEvent::UserMessage {
            id: Uuid::new_v4().to_string(),
            text: "older".into(),
            at: older,
        });
        second.write().append(SessionEvent::UserMessage {
            id: Uuid::new_v4().to_string(),
            text: "newer".into(),
            at: newer,
        });
        store.persist_now_result(&first).expect("persist first");
        store.persist_now_result(&second).expect("persist second");
        assert_eq!(store.latest_id(), Some(second.read().id.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compact_projection_keeps_system_and_newest_context() {
        let mut session = Session::new();
        for text in ["first", "second", "third", "fourth"] {
            session.append(SessionEvent::UserMessage {
                id: Uuid::new_v4().to_string(),
                text: text.into(),
                at: Utc::now(),
            });
        }
        let messages = session.derive_messages_with_budget("system", Some(3), Some(10_000), false);
        assert_eq!(
            messages.first().map(|message| message.text()),
            Some("system".into())
        );
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages.last().map(|message| message.text()),
            Some("fourth".into())
        );
        assert!(messages
            .iter()
            .all(|message| message.reasoning_content.is_none()));
    }
}
