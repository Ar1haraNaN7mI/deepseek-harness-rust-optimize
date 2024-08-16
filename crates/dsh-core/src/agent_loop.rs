use crate::session::{Session, SessionEvent};
use crate::Runtime;
use chrono::Utc;
use dsh_llm::{FinishReason, LlmEvent, ToolFunctionSpec, ToolSpec};
use dsh_tools::{ToolCall, ToolContext, ToolPipeline};
use parking_lot::RwLock;
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
    ThoughtTick { t: usize, note: String },
    ToolStarted { name: String, call_id: String },
    ToolFinished {
        name: String,
        call_id: String,
        ok: bool,
        preview: String,
    },
    /// Codex on-request / untrusted: wait for human y/n before running the tool.
    ApprovalNeeded {
        call_id: String,
        name: String,
        summary: String,
    },
    Error(String),
    Done,
}

pub struct AgentHandle {
    pub session: Arc<RwLock<Session>>,
    cancel_tx: watch::Sender<bool>,
}

impl AgentHandle {
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }
}

pub struct AgentLoop {
    runtime: Arc<Runtime>,
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
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handle = AgentHandle {
            session: session.clone(),
            cancel_tx,
        };

        let runtime = self.runtime.clone();
        tokio::spawn(async move {
            if let Err(err) =
                run_turn_inner(runtime, session, user_text, event_tx.clone(), cancel_rx).await
            {
                let _ = event_tx.send(AgentEvent::Error(err.to_string())).await;
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
    mut cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let turn_id = Uuid::new_v4().to_string();
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
    let _ = event_tx.send(AgentEvent::TurnStarted(turn_id.clone())).await;

    {
        let bypass = runtime.settings.read().bypass_hook_trust;
        let notes = runtime.hooks.read().run_event(
            "turn_start",
            &runtime.workspace_root,
            bypass,
        );
        for n in notes {
            session.write().append(SessionEvent::SystemNote {
                id: Uuid::new_v4().to_string(),
                text: n,
                at: Utc::now(),
            });
        }
    }

    // agent/pre-step analogue: one-pass cognition before first model request.
    let thought_notes = prepare_turn_cognition(&runtime, &user_text);
    // Non-blocking UI ticks — do not delay llm/stream TTFB.
    for (t, note) in thought_notes {
        let _ = event_tx.try_send(AgentEvent::ThoughtTick { t, note });
    }

    let max_steps = runtime.config.agent.max_steps_per_turn;
    // Cache tool schemas across steps until registry generation changes.
    let mut tool_gen = u64::MAX;
    let mut tool_specs: Vec<ToolSpec> = Vec::new();

    for _step in 0..max_steps {
        if *cancel_rx.borrow() {
            break;
        }

        let step_id = Uuid::new_v4().to_string();
        session.write().append(SessionEvent::StepStart {
            id: step_id.clone(),
            turn_id: turn_id.clone(),
            at: Utc::now(),
        });

        let system = runtime.prompt.read().render();
        let messages = session.read().derive_messages(&system);
        let gen = runtime.tools.generation();
        if gen != tool_gen {
            tool_gen = gen;
            tool_specs = runtime
                .tools
                .definitions()
                .into_iter()
                .map(|d| ToolSpec {
                    kind: "function".into(),
                    function: ToolFunctionSpec {
                        name: d.name,
                        description: d.description,
                        parameters: d.parameters,
                    },
                })
                .collect();
        }

        let (llm_tx, mut llm_rx) = mpsc::channel(256);
        let llm = runtime.llm.clone();
        let tools_for_llm = tool_specs.clone();
        let cancel_for_llm = cancel_rx.clone();
        let llm_task = tokio::spawn(async move {
            llm.stream_chat_cancellable(&messages, &tools_for_llm, llm_tx, Some(cancel_for_llm))
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
                break;
            }
            Err(e) => return Err(e.into()),
        };
        session.write().append(SessionEvent::AssistantMessage {
            id: Uuid::new_v4().to_string(),
            text: assembled.text.clone(),
            reasoning: assembled.reasoning.clone(),
            at: Utc::now(),
        });

        let tool_calls = assembled.parsed_tool_calls()?;
        let needs_tools =
            !tool_calls.is_empty() && assembled.finish_reason == FinishReason::ToolCalls;
        if !needs_tools {
            session.write().append(SessionEvent::StepEnd {
                id: step_id,
                turn_id: turn_id.clone(),
                at: Utc::now(),
            });
            break;
        }

        let tool_ctx = ToolContext {
            cwd: runtime.workspace_root.clone(),
            outer_home: runtime.outer_home.clone(),
            workspace_outer: runtime.workspace_outer.clone(),
            cancel: cancel_rx.clone(),
        };

        for (call_id, name, args) in tool_calls {
            if *cancel_rx.borrow() {
                break;
            }
            session.write().append(SessionEvent::ToolCall {
                id: Uuid::new_v4().to_string(),
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: args.clone(),
                at: Utc::now(),
            });
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
            let tool_result = if !perm.allows_tool(&name) {
                let reason = perm.deny_reason(&name);
                runtime.approvals.record_denial(crate::approvals::DeniedAction {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: args.clone(),
                    reason: reason.clone(),
                });
                dsh_tools::ToolResult::error(reason)
            } else if approval.requires_approval(&name) {
                let summary: String = args.to_string().chars().take(240).collect();
                let allowed = runtime
                    .request_tool_approval(
                        &event_tx,
                        &call_id,
                        &name,
                        &summary,
                        &mut cancel_rx,
                    )
                    .await;
                if !allowed {
                    let reason = format!(
                        "tool `{name}` denied by user (approval policy {})",
                        approval.label()
                    );
                    runtime.approvals.record_denial(crate::approvals::DeniedAction {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: args.clone(),
                        reason: reason.clone(),
                    });
                    dsh_tools::ToolResult::error(reason)
                } else {
                    let timeout =
                        std::time::Duration::from_secs(runtime.config.agent.tool_timeout_secs);
                    let result = tokio::time::timeout(
                        timeout,
                        runtime.pipeline.execute(&call, &tool_ctx),
                    )
                    .await;
                    match result {
                        Ok(r) => r,
                        Err(_) => dsh_tools::ToolResult::error("tool timed out"),
                    }
                }
            } else {
                let timeout =
                    std::time::Duration::from_secs(runtime.config.agent.tool_timeout_secs);
                let result =
                    tokio::time::timeout(timeout, runtime.pipeline.execute(&call, &tool_ctx)).await;
                match result {
                    Ok(r) => r,
                    Err(_) => dsh_tools::ToolResult::error("tool timed out"),
                }
            };

            let mut content = tool_result.content.clone();
            let max = runtime.config.agent.tool_result_max_chars;
            if content.chars().count() > max {
                content = truncate_chars(&content, max);
            }

            session.write().append(SessionEvent::ToolResult {
                id: Uuid::new_v4().to_string(),
                call_id: call_id.clone(),
                name: name.clone(),
                ok: tool_result.ok,
                content: content.clone(),
                at: Utc::now(),
            });

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
            id: step_id,
            turn_id: turn_id.clone(),
            at: Utc::now(),
        });
        // Debounced durable flush between steps (not per tool / not per token).
        runtime.sessions.mark_dirty(&session);
        runtime.sessions.flush_dirty();
        runtime.learn.flush_dirty();
    }

    session.write().append(SessionEvent::TurnEnd {
        id: turn_id.clone(),
        at: Utc::now(),
    });
    runtime.sessions.persist_now(&session);
    runtime.learn.flush_dirty();
    let _ = event_tx.send(AgentEvent::TurnEnded(turn_id)).await;
    Ok(())
}

/// Official-aligned `agent/pre-step`: assemble routing sections once.
/// Returns CTM tick notes for non-blocking UI fan-out.
fn prepare_turn_cognition(runtime: &Runtime, user_text: &str) -> Vec<(usize, String)> {
    let weights = runtime.learn.weights();
    let mut boost = weights;

    // Cheap channel registration (names only) — no BM25 before CTM.
    let skill_names: Vec<String> = runtime
        .skills
        .read()
        .as_ref()
        .map(|c| {
            c.list()
                .into_iter()
                .map(|s| s.name)
                .take(64)
                .collect()
        })
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
        let ranked = dsh_skill::rank_skills(
            &skills,
            user_text,
            &boost,
            runtime.config.agent.skill_prompt_topk,
        );
        sections.insert("skills", dsh_skill::prompt_topk_section(&ranked));
    }
    if let Some(plugins) = runtime.plugins.read().clone() {
        let ranked = dsh_plugin::rank_plugins(
            &plugins,
            user_text,
            &boost,
            runtime.config.agent.plugin_prompt_topk,
        );
        sections.insert("plugins", dsh_plugin::prompt_topk_section(&ranked));
    }
    if !snapshot.prompt_section.is_empty() {
        sections.insert("continuous_thought", snapshot.prompt_section);
    }

    {
        let mut prompt = runtime.prompt.write();
        prompt.set_sections(sections);
    }

    snapshot
        .ticks
        .into_iter()
        .map(|t| (t.t, t.note))
        .collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…[truncated]");
    out
}
