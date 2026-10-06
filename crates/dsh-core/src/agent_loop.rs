use crate::session::{Session, SessionEvent};
use crate::task::EventStore;
use crate::verification::verify_goal;
use crate::worker::RunLease;
use crate::{
    compact_tool_description, compact_tool_result, compact_tool_schema, select_tools_for_query,
};
use crate::{ApprovalScope, Runtime};
use chrono::Utc;
use dsh_llm::{FinishReason, LlmEvent, LlmRequestOptions, ToolFunctionSpec, ToolSpec};
use dsh_protocol::{
    CheckpointRecord, EventEnvelope, EventSource, GoalSpec, RetryPolicy, RunState, TaskRecord,
    TaskState,
};
use dsh_tools::{ToolCall, ToolContext, ToolPipeline};
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStarted(String),
    TurnEnded(String),
    TextDelta(String),
    ReasoningDelta(String),
    ThoughtTick {
        t: usize,
        note: String,
    },
    ToolStarted {
        name: String,
        call_id: String,
    },
    ToolFinished {
        name: String,
        call_id: String,
        ok: bool,
        preview: String,
    },
    /// Codex on-request / untrusted: wait for human y/n before running the tool.
    ApprovalNeeded {
        request_id: String,
        call_id: String,
        name: String,
        summary: String,
    },
    Error(String),
    Done,
}

/// Correlation context for events emitted to a frontend transport.
///
/// The live UI event enum intentionally stays compact for existing TUI
/// callers.  This context lets CLI, App Server, and MCP adapters project the
/// same event into the versioned protocol envelope without duplicating the
/// execution loop.
#[derive(Debug, Clone, Default)]
pub struct AgentEventContext {
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub step_id: Option<String>,
}

impl AgentEventContext {
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            ..Self::default()
        }
    }
}

impl AgentEvent {
    /// Convert a live agent event to the stable transport envelope.
    ///
    /// `sequence` is assigned by the event store when the adapter persists the
    /// envelope.  A zero sequence therefore means a live event could not be
    /// persisted, while the schema and correlation fields remain available to
    /// the caller.
    pub fn to_protocol_event(&self, context: &AgentEventContext) -> EventEnvelope {
        let (event_type, source, payload) = match self {
            Self::TurnStarted(id) => (
                "agent.turn_started",
                EventSource::Core,
                json!({"turn_id": id}),
            ),
            Self::TurnEnded(id) => (
                "agent.turn_ended",
                EventSource::Core,
                json!({"turn_id": id}),
            ),
            Self::TextDelta(text) => (
                "agent.text_delta",
                EventSource::Model,
                json!({"text": text}),
            ),
            Self::ReasoningDelta(text) => (
                "agent.reasoning_delta",
                EventSource::Model,
                json!({"text": text}),
            ),
            Self::ThoughtTick { t, note } => (
                "agent.thought_tick",
                EventSource::Core,
                json!({"t": t, "note": note}),
            ),
            Self::ToolStarted { name, call_id } => (
                "agent.tool_started",
                EventSource::Tool,
                json!({"name": name, "call_id": call_id}),
            ),
            Self::ToolFinished {
                name,
                call_id,
                ok,
                preview,
            } => (
                "agent.tool_finished",
                EventSource::Tool,
                json!({"name": name, "call_id": call_id, "ok": ok, "preview": preview}),
            ),
            Self::ApprovalNeeded {
                request_id,
                call_id,
                name,
                summary,
            } => (
                "agent.approval_needed",
                EventSource::Core,
                json!({"request_id": request_id, "call_id": call_id, "name": name, "summary": summary}),
            ),
            Self::Error(message) => (
                "agent.error",
                EventSource::System,
                json!({"message": message}),
            ),
            Self::Done => ("agent.done", EventSource::Core, json!({})),
        };

        let mut event = EventEnvelope::new(event_type, payload).with_source(source);
        if let Some(id) = &context.task_id {
            event = event.with_task(id.clone());
        }
        if let Some(id) = &context.run_id {
            event = event.with_run(id.clone());
        }
        let turn_id = context.turn_id.clone().or_else(|| match self {
            Self::TurnStarted(id) | Self::TurnEnded(id) => Some(id.clone()),
            _ => None,
        });
        if let Some(id) = turn_id {
            event = event.with_turn(id);
        }
        if let Some(id) = &context.step_id {
            event = event.with_step(id.clone());
        }
        if let Some(id) = &context.session_id {
            event.payload["session_id"] = json!(id);
        }
        event
    }
}

#[derive(Clone)]
pub struct AgentHandle {
    pub session: Arc<RwLock<Session>>,
    cancel_tx: watch::Sender<bool>,
    pause_tx: watch::Sender<bool>,
}

impl AgentHandle {
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    pub fn pause(&self) {
        let _ = self.pause_tx.send(true);
    }
}

pub struct AgentLoop {
    runtime: Arc<Runtime>,
}

struct TurnControl {
    cancel_rx: watch::Receiver<bool>,
    pause_rx: watch::Receiver<bool>,
    handle: AgentHandle,
}

impl AgentLoop {
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self { runtime }
    }

    pub async fn run_turn(
        &self,
        session: Arc<RwLock<Session>>,
        user_text: String,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<AgentHandle> {
        self.spawn_turn(session, user_text, event_tx, None).await
    }

    /// Start a turn against a scheduler-reserved durable task.
    pub async fn run_task(
        &self,
        task_id: impl Into<String>,
        session: Arc<RwLock<Session>>,
        user_text: String,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<AgentHandle> {
        let task_id = task_id.into();
        self.runtime.reserve_task(task_id.clone());
        match self
            .spawn_turn(session, user_text, event_tx, Some(task_id.clone()))
            .await
        {
            Ok(handle) => Ok(handle),
            Err(error) => {
                self.runtime.release_task_reservation(&task_id);
                Err(error)
            }
        }
    }

    async fn spawn_turn(
        &self,
        session: Arc<RwLock<Session>>,
        user_text: String,
        event_tx: mpsc::Sender<AgentEvent>,
        task_override: Option<String>,
    ) -> anyhow::Result<AgentHandle> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(false);
        let handle = AgentHandle {
            session: session.clone(),
            cancel_tx,
            pause_tx,
        };

        let runtime = self.runtime.clone();
        let runtime_for_release = runtime.clone();
        let control_handle = handle.clone();
        let reservation_id = task_override.clone();
        tokio::spawn(async move {
            if let Err(err) = run_turn_inner(
                runtime,
                session,
                user_text,
                event_tx.clone(),
                task_override,
                TurnControl {
                    cancel_rx,
                    pause_rx,
                    handle: control_handle,
                },
            )
            .await
            {
                let _ = event_tx.send(AgentEvent::Error(err.to_string())).await;
            }
            if let Some(task_id) = reservation_id {
                runtime_for_release.release_task_reservation(&task_id);
            }
            let _ = event_tx.send(AgentEvent::Done).await;
        });

        Ok(handle)
    }
}

async fn run_turn_inner(
    runtime: Arc<Runtime>,
    session: Arc<RwLock<Session>>,
    user_text: String,
    event_tx: mpsc::Sender<AgentEvent>,
    task_override: Option<String>,
    control: TurnControl,
) -> anyhow::Result<()> {
    let TurnControl {
        mut cancel_rx,
        pause_rx,
        handle: control_handle,
    } = control;
    let turn_id = Uuid::new_v4().to_string();
    let (session_id, session_goal, goal_paused) = {
        let current = session.read();
        (
            current.id.clone(),
            current.goal.clone(),
            current.goal_paused,
        )
    };
    if goal_paused && session_goal.is_some() {
        anyhow::bail!("session goal is paused; use /goal resume before running it");
    }
    let goal_text = session_goal
        .filter(|goal| !goal.trim().is_empty())
        .unwrap_or_else(|| {
            if user_text.trim().is_empty() {
                "interactive turn".to_string()
            } else {
                user_text.clone()
            }
        });
    let task_id = if let Some(task_id) = task_override {
        let task = runtime
            .tasks
            .task(&task_id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;
        if task.session_id.as_deref() != Some(session_id.as_str()) {
            anyhow::bail!("task {task_id} is not attached to session {session_id}");
        }
        if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
            anyhow::bail!("task {task_id} is terminal ({:?})", task.state);
        }
        if task.state == TaskState::Failed {
            runtime.tasks.transition_task(&task.id, TaskState::Queued)?;
        }
        if matches!(
            runtime.tasks.task(&task.id).map(|task| task.state),
            Some(TaskState::Queued | TaskState::Paused)
        ) {
            runtime
                .tasks
                .transition_task(&task.id, TaskState::Running)?;
        }
        task_id
    } else {
        let reusable_task = runtime
            .tasks
            .task_for_session(&session_id)
            .filter(|task| task.goal.outcome == goal_text)
            .filter(|task| {
                matches!(
                    task.state,
                    TaskState::Queued | TaskState::Paused | TaskState::Failed
                )
            });
        if let Some(task) = reusable_task {
            if task.state == TaskState::Failed {
                runtime.tasks.transition_task(&task.id, TaskState::Queued)?;
            }
            runtime
                .tasks
                .transition_task(&task.id, TaskState::Running)?;
            task.id
        } else {
            let retry_policy = RetryPolicy {
                max_attempts: runtime.config.agent.retry_max_attempts,
                backoff_secs: runtime.config.agent.retry_backoff_secs,
                max_backoff_secs: runtime.config.agent.retry_max_backoff_secs,
            };
            let mut task =
                TaskRecord::new_with_retry_policy(GoalSpec::new(goal_text), retry_policy)
                    .map_err(anyhow::Error::msg)?;
            task.session_id = Some(session_id.clone());
            let task_id = task.id.clone();
            runtime.tasks.create_task(task)?;
            runtime
                .tasks
                .transition_task(&task_id, TaskState::Running)?;
            task_id
        }
    };
    let attempt = runtime
        .tasks
        .latest_run_for_task(&task_id)
        .map(|run| run.attempt.saturating_add(1))
        .unwrap_or(1);
    let run = runtime.tasks.create_run(&task_id, attempt)?;
    runtime.register_agent_handle(task_id.clone(), control_handle);
    let _active_agent_guard = ActiveAgentGuard {
        runtime: runtime.clone(),
        task_id: task_id.clone(),
    };
    let lease = runtime.workers.acquire(&run.id)?;
    let run_id = lease.run_id().to_string();
    {
        let mut s = session.write();
        s.append(SessionEvent::TurnStart {
            id: turn_id.clone(),
            at: Utc::now(),
        });
        s.append(SessionEvent::UserMessage {
            id: Uuid::new_v4().to_string(),
            text: user_text.clone(),
            at: Utc::now(),
        });
    }
    record_event(
        &runtime,
        EventEnvelope::new(
            "turn.started",
            json!({"session_id": session_id, "user_text": user_text.clone()}),
        )
        .with_source(EventSource::Core)
        .with_turn(turn_id.clone()),
        Some(&task_id),
        Some(&run_id),
    );
    record_event(
        &runtime,
        EventEnvelope::new("user.message", json!({"text": user_text}))
            .with_source(EventSource::User)
            .with_turn(turn_id.clone()),
        Some(&task_id),
        Some(&run_id),
    );
    let _ = event_tx
        .send(AgentEvent::TurnStarted(turn_id.clone()))
        .await;

    {
        let bypass = runtime.settings.read().bypass_hook_trust;
        let notes = runtime
            .hooks
            .read()
            .run_event("turn_start", &runtime.workspace_root, bypass);
        for n in notes {
            session.write().append(SessionEvent::SystemNote {
                id: Uuid::new_v4().to_string(),
                text: n,
                at: Utc::now(),
            });
        }
    }

    // Refresh model-size guidance after a `/model` or CLI override. The policy
    // is immutable for this turn, which keeps all steps internally consistent
    // even if another frontend changes the active model concurrently.
    let model_profile = runtime.sync_model_optimization();
    let mut model_policy = model_profile.policy.clone();
    let base_llm_config = runtime.llm.config();
    if model_profile.small_model {
        if let Some(max_tokens) = model_policy.max_tokens {
            let configured_max = if base_llm_config.max_tokens == 0 {
                256
            } else {
                base_llm_config.max_tokens
            };
            model_policy.max_tokens = Some(max_tokens.min(configured_max).max(1));
        }
        if let Some(max_chars) = model_policy.tool_result_max_chars {
            let configured_max = if runtime.config.agent.tool_result_max_chars == 0 {
                512
            } else {
                runtime.config.agent.tool_result_max_chars
            };
            model_policy.tool_result_max_chars = Some(max_chars.min(configured_max).max(1));
        }
    }

    // agent/pre-step analogue: one-pass cognition before first model request.
    let thought_notes = prepare_turn_cognition(&runtime, &user_text, model_profile.small_model);
    // Non-blocking UI ticks — do not delay llm/stream TTFB.
    for (t, note) in thought_notes {
        let _ = event_tx.try_send(AgentEvent::ThoughtTick { t, note });
    }

    let task_goal = runtime.tasks.task(&task_id).map(|task| task.goal);
    let max_steps = task_goal
        .as_ref()
        .and_then(|goal| goal.max_steps)
        .map(|steps| steps.min(usize::MAX as u64) as usize)
        .unwrap_or(runtime.config.agent.max_steps_per_turn);
    let max_steps = model_policy
        .max_steps
        .map(|limit| max_steps.min(limit))
        .unwrap_or(max_steps)
        .min(10_000);
    let deadline = task_goal.as_ref().and_then(|goal| goal.deadline);
    let mut completed_response = false;
    // Cache tool schemas across steps until registry generation changes.
    let mut tool_gen = u64::MAX;
    let mut tool_specs: Vec<ToolSpec> = Vec::new();

    for _step in 0..max_steps {
        if *cancel_rx.borrow() || *pause_rx.borrow() {
            break;
        }
        if deadline.is_some_and(|at| Utc::now() >= at) {
            record_event(
                &runtime,
                EventEnvelope::new("task.deadline_exceeded", json!({"deadline": deadline}))
                    .with_source(EventSource::System)
                    .with_turn(turn_id.clone()),
                Some(&task_id),
                Some(&run_id),
            );
            record_task_feedback(&runtime, &task_id, &run_id, false, "goal deadline exceeded");
            finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
            return Err(anyhow::anyhow!("goal deadline exceeded"));
        }

        let step_id = Uuid::new_v4().to_string();
        session.write().append(SessionEvent::StepStart {
            id: step_id.clone(),
            turn_id: turn_id.clone(),
            at: Utc::now(),
        });
        record_event(
            &runtime,
            EventEnvelope::new("step.started", json!({"step_index": _step + 1}))
                .with_source(EventSource::Core)
                .with_turn(turn_id.clone())
                .with_step(step_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );

        let system_limit = model_policy
            .context_chars
            .map(|chars| (chars / 3).max(2_048));
        let message_limit = model_policy
            .context_chars
            .map(|chars| chars.saturating_sub(system_limit.unwrap_or(0)).max(2_048));
        let system = runtime.prompt.read().render_with_limit(system_limit);
        let messages = session.read().derive_messages_with_budget(
            &system,
            model_policy.context_messages,
            message_limit,
            model_policy
                .thinking
                .unwrap_or(runtime.llm.config().thinking),
        );
        let gen = runtime.tools.generation();
        if gen != tool_gen {
            tool_gen = gen;
            let definitions = runtime.tools.definitions();
            let definitions =
                select_tools_for_query(&definitions, &user_text, model_policy.tool_budget);
            tool_specs = definitions
                .into_iter()
                .map(|d| ToolSpec {
                    kind: "function".into(),
                    function: ToolFunctionSpec {
                        name: d.name,
                        description: if model_policy.compact_tool_schemas {
                            compact_tool_description(&d.description, 320)
                        } else {
                            d.description
                        },
                        parameters: if model_policy.compact_tool_schemas {
                            compact_tool_schema(
                                &d.parameters,
                                model_policy.tool_schema_chars.unwrap_or(2_400),
                            )
                        } else {
                            d.parameters
                        },
                    },
                })
                .collect();
        }

        let (llm_tx, mut llm_rx) = mpsc::channel(256);
        let llm = runtime.llm.clone();
        let tools_for_llm = tool_specs.clone();
        let cancel_for_llm = cancel_rx.clone();
        let request_options = if model_policy.max_tokens.is_some()
            || model_policy.temperature.is_some()
            || model_policy.thinking.is_some()
            || model_policy.parallel_tool_calls.is_some()
        {
            Some(LlmRequestOptions {
                thinking: model_policy.thinking,
                max_tokens: model_policy.max_tokens,
                temperature: model_policy.temperature,
                parallel_tool_calls: model_policy.parallel_tool_calls,
                extra_body: None,
            })
        } else {
            None
        };
        let llm_task = tokio::spawn(async move {
            llm.stream_chat_cancellable_with_options(
                &messages,
                &tools_for_llm,
                llm_tx,
                Some(cancel_for_llm),
                request_options,
            )
            .await
        });

        // Stream path: forward to UI immediately; do NOT append per-token chunk
        // events (official keeps chunks for UI fidelity via live events; durable
        // assistant/message is written once after assemble — preserves token rate).
        while let Some(ev) = llm_rx.recv().await {
            match ev {
                LlmEvent::TextDelta(t) => {
                    let _ = event_tx.try_send(AgentEvent::TextDelta(t));
                }
                LlmEvent::ReasoningDelta(t) => {
                    let _ = event_tx.try_send(AgentEvent::ReasoningDelta(t));
                }
                LlmEvent::Error(e) => {
                    let _ = event_tx.send(AgentEvent::Error(e)).await;
                }
                _ => {}
            }
        }

        let assembled = match llm_task.await? {
            Ok(a) => a,
            Err(e) if e.to_string().contains("cancelled") => {
                session.write().append(SessionEvent::SystemNote {
                    id: Uuid::new_v4().to_string(),
                    text: "turn cancelled during LLM stream".into(),
                    at: Utc::now(),
                });
                runtime.sessions.persist_now(&session);
                record_event(
                    &runtime,
                    EventEnvelope::new("turn.cancelled", json!({"reason": "llm_cancelled"}))
                        .with_source(EventSource::System)
                        .with_turn(turn_id.clone()),
                    Some(&task_id),
                    Some(&run_id),
                );
                break;
            }
            Err(e) => {
                record_event(
                    &runtime,
                    EventEnvelope::new("turn.failed", json!({"error": e.to_string()}))
                        .with_source(EventSource::System)
                        .with_turn(turn_id.clone()),
                    Some(&task_id),
                    Some(&run_id),
                );
                record_task_feedback(&runtime, &task_id, &run_id, false, &e.to_string());
                finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
                return Err(e.into());
            }
        };
        session.write().append(SessionEvent::AssistantMessage {
            id: Uuid::new_v4().to_string(),
            text: assembled.text.clone(),
            reasoning: assembled.reasoning.clone(),
            at: Utc::now(),
        });
        record_event(
            &runtime,
            EventEnvelope::new(
                "assistant.message",
                json!({"text": assembled.text.clone(), "reasoning": assembled.reasoning.clone()}),
            )
            .with_source(EventSource::Model)
            .with_turn(turn_id.clone())
            .with_step(step_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );

        let tool_calls = match assembled.parsed_tool_calls() {
            Ok(tool_calls) => tool_calls,
            Err(err) => {
                record_task_feedback(&runtime, &task_id, &run_id, false, &err.to_string());
                finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
                return Err(err);
            }
        };
        let needs_tools =
            !tool_calls.is_empty() && assembled.finish_reason == FinishReason::ToolCalls;
        if !needs_tools {
            completed_response = true;
            session.write().append(SessionEvent::StepEnd {
                id: step_id.clone(),
                turn_id: turn_id.clone(),
                at: Utc::now(),
            });
            record_event(
                &runtime,
                EventEnvelope::new("step.ended", json!({"reason": "stop"}))
                    .with_source(EventSource::Core)
                    .with_turn(turn_id.clone())
                    .with_step(step_id.clone()),
                Some(&task_id),
                Some(&run_id),
            );
            save_step_checkpoint(&runtime, &session, &task_id, &run_id, _step);
            break;
        }

        let tool_ctx = ToolContext {
            cwd: runtime.workspace_root.clone(),
            outer_home: runtime.outer_home.clone(),
            workspace_outer: runtime.workspace_outer.clone(),
            cancel: cancel_rx.clone(),
        };

        for (call_id, name, args) in tool_calls {
            if *cancel_rx.borrow() || *pause_rx.borrow() {
                break;
            }
            if deadline.is_some_and(|at| Utc::now() >= at) {
                record_task_feedback(&runtime, &task_id, &run_id, false, "goal deadline exceeded");
                finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
                return Err(anyhow::anyhow!("goal deadline exceeded"));
            }
            session.write().append(SessionEvent::ToolCall {
                id: Uuid::new_v4().to_string(),
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: args.clone(),
                at: Utc::now(),
            });
            record_event(
                &runtime,
                EventEnvelope::new(
                    "tool.called",
                    json!({"call_id": call_id.clone(), "name": name.clone(), "arguments": args.clone()}),
                )
                .with_source(EventSource::Model)
                .with_turn(turn_id.clone())
                .with_step(step_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );
            let _ = event_tx
                .send(AgentEvent::ToolStarted {
                    name: name.clone(),
                    call_id: call_id.clone(),
                })
                .await;

            let call = ToolCall {
                id: call_id.clone(),
                name: name.clone(),
                arguments: args.clone(),
            };
            let perm = *runtime.permissions.read();
            let approval = runtime.settings.read().approval;
            let tool_result = match runtime.tools.preflight(&call) {
                Err(err) => dsh_tools::ToolResult::error(err.to_string()),
                Ok(definition) => {
                    let metadata_aware = !definition.metadata.capabilities.is_empty();
                    let allowed = if metadata_aware {
                        perm.allows_metadata(&definition.metadata)
                    } else {
                        perm.allows_tool(&name)
                    };
                    let needs_approval = if metadata_aware {
                        approval.requires_metadata(&definition.metadata)
                    } else {
                        approval.requires_approval(&name)
                    };
                    if !allowed {
                        let reason = perm.deny_reason(&name);
                        runtime
                            .approvals
                            .record_denial(crate::approvals::DeniedAction {
                                call_id: call_id.clone(),
                                name: name.clone(),
                                arguments: args.clone(),
                                reason: reason.clone(),
                            });
                        dsh_tools::ToolResult::error(reason)
                    } else if needs_approval {
                        transition_durable_state(
                            &runtime,
                            &task_id,
                            &run_id,
                            TaskState::WaitingApproval,
                            RunState::WaitingApproval,
                        );
                        let summary: String = args.to_string().chars().take(240).collect();
                        let allowed = runtime
                            .request_tool_approval_scoped(
                                &event_tx,
                                &call_id,
                                &name,
                                &summary,
                                ApprovalScope::new(Some(&task_id), Some(&run_id)),
                                &mut cancel_rx,
                            )
                            .await;
                        transition_durable_state(
                            &runtime,
                            &task_id,
                            &run_id,
                            TaskState::Running,
                            RunState::Running,
                        );
                        if !allowed {
                            let reason = format!(
                                "tool `{name}` denied by user (approval policy {})",
                                approval.label()
                            );
                            runtime
                                .approvals
                                .record_denial(crate::approvals::DeniedAction {
                                    call_id: call_id.clone(),
                                    name: name.clone(),
                                    arguments: args.clone(),
                                    reason: reason.clone(),
                                });
                            dsh_tools::ToolResult::error(reason)
                        } else {
                            execute_tool_with_timeout(
                                &runtime,
                                &call,
                                &tool_ctx,
                                definition.metadata.timeout_secs,
                            )
                            .await
                        }
                    } else {
                        execute_tool_with_timeout(
                            &runtime,
                            &call,
                            &tool_ctx,
                            definition.metadata.timeout_secs,
                        )
                        .await
                    }
                }
            };

            let mut content = tool_result.content.clone();
            let max = model_policy
                .tool_result_max_chars
                .unwrap_or(runtime.config.agent.tool_result_max_chars);
            if content.chars().count() > max {
                content = if model_policy.compact_tool_results {
                    compact_tool_result(&content, max)
                } else {
                    truncate_chars(&content, max)
                };
            }

            session.write().append(SessionEvent::ToolResult {
                id: Uuid::new_v4().to_string(),
                call_id: call_id.clone(),
                name: name.clone(),
                ok: tool_result.ok,
                content: content.clone(),
                at: Utc::now(),
            });
            record_event(
                &runtime,
                EventEnvelope::new(
                    "tool.result",
                    json!({"call_id": call_id.clone(), "name": name.clone(), "ok": tool_result.ok, "content": content.clone()}),
                )
                .with_source(EventSource::Tool)
                .with_turn(turn_id.clone())
                .with_step(step_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );

            if runtime.config.learn.enabled {
                let note = if name == "skill_load" {
                    format!(
                        "loaded skill {}",
                        args.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                    )
                } else {
                    content.chars().take(100).collect::<String>()
                };
                runtime.learn.record_tool_outcome_detailed(
                    &user_text,
                    &name,
                    args.get("name").and_then(|v| v.as_str()),
                    tool_result.ok,
                    note,
                );
            }
            if name == "skill_load" {
                if let Some(sk) = args.get("name").and_then(|v| v.as_str()) {
                    runtime.ctm.observe_skill_load(sk, tool_result.ok);
                }
            } else {
                runtime.ctm.observe_tool(&name, tool_result.ok);
            }

            let preview: String = content.chars().take(160).collect();
            let _ = event_tx
                .send(AgentEvent::ToolFinished {
                    name,
                    call_id,
                    ok: tool_result.ok,
                    preview,
                })
                .await;
        }

        session.write().append(SessionEvent::StepEnd {
            id: step_id.clone(),
            turn_id: turn_id.clone(),
            at: Utc::now(),
        });
        record_event(
            &runtime,
            EventEnvelope::new("step.ended", json!({"reason": "tool_calls"}))
                .with_source(EventSource::Core)
                .with_turn(turn_id.clone())
                .with_step(step_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );
        // Debounced durable flush between steps (not per tool / not per token).
        runtime.sessions.mark_dirty(&session);
        runtime.sessions.flush_dirty();
        runtime.learn.flush_dirty();
        save_step_checkpoint(&runtime, &session, &task_id, &run_id, _step);
    }

    if !completed_response && !*cancel_rx.borrow() && !*pause_rx.borrow() {
        let message = if max_steps == 0 {
            "goal has no executable steps"
        } else {
            "maximum agent steps exceeded"
        };
        record_event(
            &runtime,
            EventEnvelope::new("task.max_steps_exceeded", json!({"max_steps": max_steps}))
                .with_source(EventSource::System)
                .with_turn(turn_id.clone()),
            Some(&task_id),
            Some(&run_id),
        );
        record_task_feedback(&runtime, &task_id, &run_id, false, message);
        finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
        return Err(anyhow::anyhow!(message));
    }

    session.write().append(SessionEvent::TurnEnd {
        id: turn_id.clone(),
        at: Utc::now(),
    });
    runtime.sessions.persist_now(&session);
    runtime.learn.flush_dirty();
    if *cancel_rx.borrow() {
        record_task_feedback(&runtime, &task_id, &run_id, false, "run cancelled");
        finish_durable_run(&lease, TaskState::Cancelled, RunState::Cancelled);
    } else if *pause_rx.borrow() {
        record_task_feedback(&runtime, &task_id, &run_id, false, "run paused");
        match runtime.tasks.task(&task_id).map(|task| task.state) {
            Some(TaskState::Queued) => {
                finish_durable_run(&lease, TaskState::Queued, RunState::Cancelled)
            }
            Some(TaskState::Cancelled) => {
                finish_durable_run(&lease, TaskState::Cancelled, RunState::Cancelled)
            }
            Some(TaskState::Completed) => {
                finish_durable_run(&lease, TaskState::Completed, RunState::Completed)
            }
            _ => finish_durable_run(&lease, TaskState::Paused, RunState::Paused),
        }
    } else {
        let goal = runtime.tasks.task(&task_id).map(|task| task.goal);
        let report = goal.as_ref().map(|goal| {
            if goal.has_verification() {
                record_event(
                    &runtime,
                    EventEnvelope::new(
                        "verification.started",
                        json!({"criteria": goal.verification}),
                    )
                    .with_source(EventSource::Core)
                    .with_turn(turn_id.clone()),
                    Some(&task_id),
                    Some(&run_id),
                );
                verify_goal(&runtime.workspace_root, goal, &session.read())
            } else {
                record_event(
                    &runtime,
                    EventEnvelope::new("verification.skipped", json!({"reason": "no_criteria"}))
                        .with_source(EventSource::Core)
                        .with_turn(turn_id.clone()),
                    Some(&task_id),
                    Some(&run_id),
                );
                crate::verification::VerificationReport::skipped()
            }
        });
        let verification_passed = report.as_ref().is_none_or(|report| report.passed);
        if let Some(report) = report.as_ref().filter(|report| !report.checks.is_empty()) {
            let event_type = if report.passed {
                "verification.passed"
            } else {
                "verification.failed"
            };
            record_event(
                &runtime,
                EventEnvelope::new(event_type, json!(report))
                    .with_source(if report.passed {
                        EventSource::Core
                    } else {
                        EventSource::System
                    })
                    .with_turn(turn_id.clone()),
                Some(&task_id),
                Some(&run_id),
            );
        }
        if verification_passed {
            record_task_feedback(&runtime, &task_id, &run_id, true, "run completed");
            finish_durable_run(&lease, TaskState::Completed, RunState::Completed);
        } else {
            let detail = report
                .as_ref()
                .and_then(|report| report.checks.iter().find(|check| !check.passed))
                .map(|check| format!("{}: {}", check.criterion, check.detail))
                .unwrap_or_else(|| "goal verification failed".into());
            record_task_feedback(&runtime, &task_id, &run_id, false, &detail);
            finish_durable_run(&lease, TaskState::Failed, RunState::Failed);
            return Err(anyhow::anyhow!(detail));
        }
    }
    record_event(
        &runtime,
        EventEnvelope::new("turn.ended", json!({"session_id": session_id}))
            .with_source(EventSource::Core)
            .with_turn(turn_id.clone()),
        Some(&task_id),
        Some(&run_id),
    );
    let _ = event_tx.send(AgentEvent::TurnEnded(turn_id)).await;
    Ok(())
}

/// Official-aligned `agent/pre-step`: assemble routing sections once.
/// Returns CTM tick notes for non-blocking UI fan-out.
fn prepare_turn_cognition(
    runtime: &Runtime,
    user_text: &str,
    small_model: bool,
) -> Vec<(usize, String)> {
    let skill_topk = if small_model {
        runtime.config.agent.skill_prompt_topk.min(2)
    } else {
        runtime.config.agent.skill_prompt_topk
    };
    let plugin_topk = if small_model {
        runtime.config.agent.plugin_prompt_topk.min(1)
    } else {
        runtime.config.agent.plugin_prompt_topk
    };
    let weights = runtime.learn.weights();
    let mut boost = weights;

    // Cheap channel registration (names only) — no BM25 before CTM.
    let skill_names: Vec<String> = runtime
        .skills
        .read()
        .as_ref()
        .map(|c| c.list().into_iter().map(|s| s.name).take(64).collect())
        .unwrap_or_default();
    let plugin_ids: Vec<String> = runtime
        .plugins
        .read()
        .as_ref()
        .map(|p| p.ids())
        .unwrap_or_default();

    runtime.ctm.ensure_channels(&skill_names, &plugin_ids);
    let snapshot = runtime.ctm.think(user_text, &skill_names, &plugin_ids);

    for (i, name) in snapshot.recommended_skills.iter().enumerate() {
        let key = format!("skill:{name}");
        let bump = 1.8 - i as f32 * 0.15;
        *boost.entry(key).or_insert(0.0) += bump.max(0.4);
    }
    for (i, id) in snapshot.recommended_plugins.iter().enumerate() {
        let key = format!("plugin:{id}");
        let bump = 1.6 - i as f32 * 0.15;
        *boost.entry(key).or_insert(0.0) += bump.max(0.3);
    }

    let mut sections: HashMap<&str, String> = HashMap::new();
    if runtime.config.learn.enabled {
        sections.insert("learn", runtime.learn.prompt_section(user_text));
    }

    // Single-pass rank after CTM boosts (was double-ranked).
    if let Some(skills) = runtime.skills.read().clone() {
        let ranked = dsh_skill::rank_skills(&skills, user_text, &boost, skill_topk);
        sections.insert("skills", dsh_skill::prompt_topk_section(&ranked));
    }
    if let Some(plugins) = runtime.plugins.read().clone() {
        let ranked = dsh_plugin::rank_plugins(&plugins, user_text, &boost, plugin_topk);
        sections.insert("plugins", dsh_plugin::prompt_topk_section(&ranked));
    }
    if !snapshot.prompt_section.is_empty() {
        sections.insert("continuous_thought", snapshot.prompt_section);
    }

    {
        let mut prompt = runtime.prompt.write();
        prompt.set_sections(sections);
    }

    snapshot.ticks.into_iter().map(|t| (t.t, t.note)).collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated]");
    out
}

fn record_event(
    runtime: &Runtime,
    mut event: EventEnvelope,
    task_id: Option<&str>,
    run_id: Option<&str>,
) {
    if let Some(task_id) = task_id {
        event = event.with_task(task_id.to_string());
    }
    if let Some(run_id) = run_id {
        event = event.with_run(run_id.to_string());
    }
    if let Err(err) = runtime.record_event(event) {
        tracing::error!(error = %err, "failed to persist protocol event");
    }
}

fn save_step_checkpoint(
    runtime: &Runtime,
    session: &Arc<RwLock<Session>>,
    task_id: &str,
    run_id: &str,
    step: usize,
) {
    let mut checkpoint = CheckpointRecord::new(
        task_id.to_string(),
        run_id.to_string(),
        runtime.events.latest_sequence(),
    );
    checkpoint.step_index = (step + 1) as u32;
    checkpoint.session_event_count = session.read().events.len() as u64;
    if let Err(err) = runtime.tasks.save_checkpoint(checkpoint) {
        tracing::warn!(error = %err, task_id, run_id, step = step + 1, "failed to persist step checkpoint");
    }
}

fn finish_durable_run(lease: &RunLease, task_state: TaskState, run_state: RunState) {
    if let Err(err) = lease.finish(task_state, run_state) {
        tracing::warn!(
            error = %err,
            task_id = lease.task_id(),
            run_id = lease.run_id(),
            "failed to finish durable run"
        );
    }
}

struct ActiveAgentGuard {
    runtime: Arc<Runtime>,
    task_id: String,
}

impl Drop for ActiveAgentGuard {
    fn drop(&mut self) {
        self.runtime.clear_agent_handle(&self.task_id);
    }
}

fn record_task_feedback(runtime: &Runtime, task_id: &str, run_id: &str, ok: bool, note: &str) {
    let Some(task) = runtime.tasks.task(task_id) else {
        return;
    };
    let attempt = runtime
        .tasks
        .run(run_id)
        .map(|run| run.attempt)
        .unwrap_or(1);
    runtime
        .learn
        .record_task_outcome(task_id, run_id, &task.goal.outcome, attempt, ok, note);
    let _ = runtime.record_event(
        EventEnvelope::new(
            "learning.feedback",
            json!({
                "task_id": task_id,
                "run_id": run_id,
                "attempt": attempt,
                "goal": task.goal.outcome,
                "ok": ok,
                "note": note,
            }),
        )
        .with_source(EventSource::System)
        .with_task(task_id.to_string())
        .with_run(run_id.to_string()),
    );
    runtime.learn.flush_dirty();
}

fn transition_durable_state(
    runtime: &Runtime,
    task_id: &str,
    run_id: &str,
    task_state: TaskState,
    run_state: RunState,
) {
    if let Err(err) = runtime.tasks.transition_task(task_id, task_state) {
        tracing::warn!(error = %err, task_id, "failed to transition durable task state");
    }
    if let Err(err) = runtime.tasks.transition_run(run_id, run_state) {
        tracing::warn!(error = %err, run_id, "failed to transition durable run state");
    }
}

async fn execute_tool_with_timeout(
    runtime: &Runtime,
    call: &ToolCall,
    ctx: &ToolContext,
    metadata_timeout_secs: Option<u64>,
) -> dsh_tools::ToolResult {
    let timeout_secs = metadata_timeout_secs
        .unwrap_or(runtime.config.agent.tool_timeout_secs)
        .max(1);
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        runtime.pipeline.execute(call, ctx),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => dsh_tools::ToolResult::error("tool timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_protocol_projection_keeps_correlations() {
        let mut context = AgentEventContext::for_session("session-1");
        context.task_id = Some("task-1".into());
        context.run_id = Some("run-1".into());
        context.turn_id = Some("turn-1".into());
        context.step_id = Some("step-1".into());
        let event = AgentEvent::ToolFinished {
            name: "read_file".into(),
            call_id: "call-1".into(),
            ok: true,
            preview: "ok".into(),
        }
        .to_protocol_event(&context);
        assert_eq!(event.schema_version, dsh_protocol::SCHEMA_VERSION);
        assert_eq!(event.sequence, 0);
        assert_eq!(event.task_id.as_deref(), Some("task-1"));
        assert_eq!(event.run_id.as_deref(), Some("run-1"));
        assert_eq!(event.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(event.step_id.as_deref(), Some("step-1"));
        assert_eq!(event.payload["session_id"], "session-1");
    }
}
