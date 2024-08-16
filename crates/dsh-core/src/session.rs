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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TurnStart { id: String, at: DateTime<Utc> },
    TurnEnd { id: String, at: DateTime<Utc> },
    StepStart { id: String, turn_id: String, at: DateTime<Utc> },
    StepEnd { id: String, turn_id: String, at: DateTime<Utc> },
    UserMessage { id: String, text: String, at: DateTime<Utc> },
    AssistantMessage {
        id: String,
        text: String,
        reasoning: Option<String>,
        at: DateTime<Utc>,
    },
    /// Live/UI fidelity only — stripped from durable disk snapshots.
    AssistantChunk { id: String, text: String, at: DateTime<Utc> },
    ReasoningChunk { id: String, text: String, at: DateTime<Utc> },
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
    SystemNote { id: String, text: String, at: DateTime<Utc> },
}

impl SessionEvent {
    fn is_live_chunk(&self) -> bool {
        matches!(
            self,
            SessionEvent::AssistantChunk { .. } | SessionEvent::ReasoningChunk { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
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

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
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
        let idx = self.events.iter().rposition(|e| {
            matches!(e, SessionEvent::UserMessage { .. })
        })?;
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
                    text,
                    reasoning,
                    ..
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

    pub fn transcript_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for event in &self.events {
            match event {
                SessionEvent::UserMessage { text, .. } => lines.push(format!("You: {text}")),
                SessionEvent::AssistantMessage { text, .. } => {
                    if !text.is_empty() {
                        lines.push(format!("Assistant: {text}"));
                    }
                }
                SessionEvent::ToolCall { name, arguments, .. } => {
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
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_json(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

pub struct SessionStore {
    sessions: RwLock<HashMap<String, Arc<RwLock<Session>>>>,
    dir: Option<PathBuf>,
    dirty: RwLock<HashMap<String, Arc<RwLock<Session>>>>,
    flush_scheduled: AtomicBool,
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
        }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            sessions: RwLock::new(HashMap::new()),
            dir: Some(dir),
            dirty: RwLock::new(HashMap::new()),
            flush_scheduled: AtomicBool::new(false),
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
        let mut ids = self.list_ids();
        ids.pop()
    }

    pub fn get_or_load(&self, id: &str) -> anyhow::Result<Arc<RwLock<Session>>> {
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
        let Some(dir) = &self.dir else {
            return;
        };
        let snap = session.read().clone();
        let path = dir.join(format!("{}.json", snap.id));
        let _ = snap.save_json(&path);
        self.dirty.write().remove(&snap.id);
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
        let mut ids: Vec<_> = self.sessions.read().keys().cloned().collect();
        if let Some(dir) = &self.dir {
            if let Ok(entries) = fs::read_dir(dir) {
                for e in entries.flatten() {
                    if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                            if !ids.iter().any(|i| i == stem) {
                                ids.push(stem.to_string());
                            }
                        }
                    }
                }
            }
        }
        ids.sort();
        ids
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
        let mut out = Vec::new();
        for id in self.list_ids() {
            if let Ok(s) = self.get_or_load(&id) {
                let snap = s.read();
                if snap.archived && !include_archived {
                    continue;
                }
                out.push((snap.id.clone(), snap.display_name(), snap.archived));
            }
        }
        out
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        session.write().archived = archived;
        self.persist_now(&session);
        Ok(())
    }

    pub fn rename(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        session.write().rename(name);
        self.persist_now(&session);
        Ok(())
    }

    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        let session = self.resolve(id)?;
        let real_id = session.read().id.clone();
        self.sessions.write().remove(&real_id);
        self.dirty.write().remove(&real_id);
        if let Some(dir) = &self.dir {
            let path = dir.join(format!("{real_id}.json"));
            if path.exists() {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    pub fn latest_active_id(&self) -> Option<String> {
        self.list_summaries(false).into_iter().map(|(id, _, _)| id).last()
    }
}
