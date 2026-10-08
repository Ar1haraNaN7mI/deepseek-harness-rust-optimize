//! JSON-RPC 2.0 control plane for local task and agent orchestration.
//!
//! The app-server deliberately delegates all state changes to `dsh-core` so
//! stdio and TCP clients observe the same durable task/event projections as
//! the TUI and CLI.

use crate::cloud;
use anyhow::{Context, Result};
use dsh_core::{
    AgentEvent, AgentEventContext, AgentLoop, EventStore, Runtime, TaskRecord, TaskState,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex as AsyncMutex, Semaphore};
use tokio::task::JoinSet;

const MAX_RPC_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_INFLIGHT_RPC: usize = 64;

type SessionTurnKeys = std::collections::HashSet<(usize, String)>;
static SESSION_TURNS: OnceLock<parking_lot::Mutex<SessionTurnKeys>> = OnceLock::new();

/// Reserve a session before returning an accepted response. Retaining the Arc
/// makes the runtime address unique for the entire lease, including task errors
/// and cancellation. No parking_lot guard is held across an await.
pub(crate) struct SessionTurnGuard {
    runtime: Arc<Runtime>,
    session_id: String,
}

impl SessionTurnGuard {
    pub(crate) fn acquire(
        runtime: &Arc<Runtime>,
        session_id: &str,
    ) -> std::result::Result<Self, RpcFailure> {
        let key = (Arc::as_ptr(runtime) as usize, session_id.to_owned());
        if !SESSION_TURNS
            .get_or_init(Default::default)
            .lock()
            .insert(key)
        {
            return Err(RpcFailure {
                code: -32003,
                message: "session is busy; wait for its current turn to finish".into(),
                data: Some(json!({"session_id":session_id})),
            });
        }
        Ok(Self {
            runtime: runtime.clone(),
            session_id: session_id.to_owned(),
        })
    }
}

impl Drop for SessionTurnGuard {
    fn drop(&mut self) {
        SESSION_TURNS
            .get_or_init(Default::default)
            .lock()
            .remove(&(Arc::as_ptr(&self.runtime) as usize, self.session_id.clone()));
    }
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug)]
pub(crate) struct RpcFailure {
    pub(crate) code: i64,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
}

impl RpcFailure {
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn internal(err: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: err.into(),
            data: None,
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: -32002,
            message: message.into(),
            data: None,
        }
    }
}

/// Serve JSON-RPC over stdio by default, or over one TCP listener when
/// `listen` is supplied (`127.0.0.1:4567` or `tcp://127.0.0.1:4567`).
pub async fn run_app_server(runtime: Arc<Runtime>, listen: Option<String>) -> Result<()> {
    let auth_token = std::env::var("DSH_APP_SERVER_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty());
    match listen.as_deref().map(str::trim) {
        None | Some("") | Some("stdio://") | Some("stdio") => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            serve_stream_with_auth(runtime, BufReader::new(stdin), stdout, auth_token).await
        }
        Some(address) => {
            let address = address.strip_prefix("tcp://").unwrap_or(address);
            if auth_token.is_none() && !is_loopback_address(address) {
                tracing::warn!(
                    address,
                    "app-server is listening on a non-loopback address without DSH_APP_SERVER_TOKEN"
                );
            }
            let listener = TcpListener::bind(address)
                .await
                .with_context(|| format!("bind app-server listener {address}"))?;
            tracing::info!(address, "dsh app-server listening");
            loop {
                let (stream, peer) = listener.accept().await?;
                tracing::info!(%peer, "app-server client connected");
                let runtime = runtime.clone();
                let auth_token = auth_token.clone();
                tokio::spawn(async move {
                    let (reader, writer) = stream.into_split();
                    if let Err(err) =
                        serve_stream_with_auth(runtime, BufReader::new(reader), writer, auth_token)
                            .await
                    {
                        tracing::debug!(error = %err, %peer, "app-server client ended");
                    }
                });
            }
        }
    }
}

#[cfg(test)]
async fn serve_stream<R, W>(runtime: Arc<Runtime>, reader: R, writer: W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_stream_with_auth(runtime, reader, writer, None).await
}

async fn serve_stream_with_auth<R, W>(
    runtime: Arc<Runtime>,
    mut reader: R,
    writer: W,
    auth_token: Option<String>,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut authenticated = auth_token.is_none();
    let writer = Arc::new(AsyncMutex::new(writer));
    let permits = Arc::new(Semaphore::new(MAX_INFLIGHT_RPC));
    let mut in_flight = JoinSet::new();
    let mut raw_line = Vec::new();
    loop {
        while in_flight.try_join_next().is_some() {}
        raw_line.clear();
        if reader.read_until(b'\n', &mut raw_line).await? == 0 {
            in_flight.abort_all();
            return Ok(());
        }
        if raw_line.len() > MAX_RPC_LINE_BYTES {
            write_shared_error(
                &writer,
                None,
                -32600,
                format!("JSON-RPC request exceeds {MAX_RPC_LINE_BYTES} bytes"),
                None,
            )
            .await?;
            // The stream is no longer frameable after an oversized line;
            // close it instead of attempting to parse the remaining bytes as
            // independent JSON-RPC requests.
            in_flight.abort_all();
            return Ok(());
        }
        let line = String::from_utf8_lossy(&raw_line);
        if line.trim().is_empty() {
            continue;
        }
        let request: RpcRequest = match serde_json::from_str(line.trim()) {
            Ok(request) => request,
            Err(err) => {
                write_shared_error(&writer, None, -32700, format!("parse error: {err}"), None)
                    .await?;
                continue;
            }
        };
        if request
            .jsonrpc
            .as_deref()
            .is_some_and(|version| version != "2.0")
        {
            if request.id.is_some() {
                write_shared_error(
                    &writer,
                    request.id,
                    -32600,
                    "jsonrpc must be \"2.0\"".into(),
                    None,
                )
                .await?;
            }
            continue;
        }
        let Some(method) = request.method else {
            if request.id.is_some() {
                write_shared_error(&writer, request.id, -32600, "missing method".into(), None)
                    .await?;
            }
            continue;
        };
        if method != "initialize" && !authenticated {
            if let Some(id) = request.id {
                write_shared_error(
                    &writer,
                    Some(id),
                    -32001,
                    "app-server authentication required".into(),
                    None,
                )
                .await?;
            }
            continue;
        }
        if method == "initialize" {
            if let Some(expected) = auth_token.as_deref() {
                authenticated = request
                    .params
                    .get("auth_token")
                    .and_then(Value::as_str)
                    .is_some_and(|provided| {
                        constant_time_eq(provided.as_bytes(), expected.as_bytes())
                    });
                if !authenticated {
                    if let Some(id) = request.id {
                        write_shared_error(
                            &writer,
                            Some(id),
                            -32001,
                            "invalid app-server authentication token".into(),
                            None,
                        )
                        .await?;
                    }
                    continue;
                }
            }
        }
        let permit = match permits.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                if let Some(id) = request.id {
                    write_shared_error(
                        &writer,
                        Some(id),
                        -32003,
                        "too many in-flight requests".into(),
                        None,
                    )
                    .await?;
                }
                continue;
            }
        };
        let runtime = runtime.clone();
        let writer = writer.clone();
        let method = method.to_string();
        let params = request.params;
        let id = request.id;
        in_flight.spawn(async move {
            let _permit = permit;
            let response = handle_request(runtime, &method, params).await;
            let Some(id) = id else {
                return;
            };
            let mut writer = writer.lock().await;
            let result = match response {
                Ok(result) => write_result(&mut *writer, id, result).await,
                Err(err) => {
                    write_error(&mut *writer, Some(id), err.code, err.message, err.data).await
                }
            };
            if let Err(error) = result {
                tracing::debug!(error = %error, "app-server response write failed");
            }
        });
    }
}

/// HTTP uses the same control plane as stdio/TCP; execution and event persistence
/// remain in the existing runtime. The HTTP host owns origin/token validation.
pub(crate) async fn dispatch_http(runtime: Arc<Runtime>, method: &str, params: Value) -> Value {
    if !params.is_null() && !params.is_object() {
        return json!({"error":{"code":-32602,"message":"params must be an object"}});
    }
    match handle_request(runtime, method, params).await {
        Ok(mut result) => {
            // Endpoint URLs can contain embedded credentials; the browser needs
            // model readiness, not connection secrets or fallback URLs.
            if method == "llm/status" {
                if let Some(object) = result.as_object_mut() {
                    object.remove("base_url");
                    object.remove("fallbacks");
                }
            }
            json!({"result":result})
        }
        Err(error) => {
            let mut value = json!({"error":{"code":error.code,"message":error.message}});
            if let Some(data) = error.data {
                value["error"]["data"] = data;
            }
            value
        }
    }
}

pub(crate) fn session_summaries(runtime: &Runtime, all: bool) -> Vec<Value> {
    runtime
        .sessions
        .list_summaries(all)
        .into_iter()
        .rev()
        .filter_map(|(id, name, archived)| {
            let session = runtime.sessions.get_or_load(&id).ok()?;
            let snapshot = session.read();
            let task = runtime.tasks.task_for_session(&id);
            Some(json!({
                "id":id,"name":name,"archived":archived,"updated_at":snapshot.last_activity_at(),
                "event_count":snapshot.events.len(),"task_id":task.as_ref().map(|task| &task.id),
                "state":task.map(|task| task.state),
            }))
        })
        .collect()
}

pub(crate) fn resolve_session(
    runtime: &Runtime,
    query: &str,
) -> std::result::Result<Arc<parking_lot::RwLock<dsh_core::Session>>, RpcFailure> {
    if query.contains(['/', '\\', ':'])
        || query.contains("..")
        || query.chars().any(char::is_control)
    {
        return Err(RpcFailure::invalid_params(
            "session must be an id, id prefix or name, not a path",
        ));
    }
    runtime
        .sessions
        .resolve(query)
        .map_err(|error| RpcFailure::invalid_params(error.to_string()))
}

fn session_result(
    runtime: &Runtime,
    session: &Arc<parking_lot::RwLock<dsh_core::Session>>,
) -> Value {
    let snapshot = session.read().clone();
    let task = runtime.tasks.task_for_session(&snapshot.id);
    json!({"session":snapshot,"task_id":task.as_ref().map(|task| &task.id),"state":task.map(|task| task.state)})
}

async fn handle_request(
    runtime: Arc<Runtime>,
    method: &str,
    params: Value,
) -> std::result::Result<Value, RpcFailure> {
    if let Some(result) = crate::model_service::dispatch(&runtime, method, &params).await {
        return result;
    }
    if let Some(result) = crate::workspace_settings::dispatch(&runtime, method, &params).await {
        return result;
    }
    if let Some(result) = crate::harness_extensions::dispatch(&runtime, method, &params) {
        return result;
    }
    if let Some(result) = crate::harness_settings::dispatch(&runtime, method, &params) {
        return result;
    }
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2.0",
            "capabilities": {
                "sessions": true,
                "tasks": true,
                "events": true,
                "eventsWait": true,
                "approvals": true,
                "tools": true,
                "computer": true,
                "agent": true,
                "cloud": true,
                "llm": true,
                "settings": true,
                "memoryManagement": true,
                "sessionManagement": true
            },
            "serverInfo": {
                "name": "dsh-rust",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "ping" => Ok(json!({"ok": true})),
        "sessions/list" => Ok(
            json!({"sessions":session_summaries(&runtime, optional_bool(&params, "all")?.unwrap_or(false))}),
        ),
        "sessions/get" => {
            let id = required_string(&params, "id")?;
            let session = resolve_session(&runtime, &id)?;
            Ok(session_result(&runtime, &session))
        }
        "sessions/create" => {
            let name = match params.get("name") {
                None | Some(Value::Null) => None,
                Some(Value::String(name))
                    if !name.trim().is_empty()
                        && name.chars().count() <= 100
                        && !name.chars().any(char::is_control) =>
                {
                    Some(name.trim().to_owned())
                }
                Some(_) => {
                    return Err(RpcFailure::invalid_params(
                        "name must contain 1 to 100 printable characters",
                    ))
                }
            };
            let session = runtime.sessions.create();
            {
                let mut snapshot = session.write();
                snapshot.name = name;
                snapshot.cwd = Some(runtime.workspace_root.to_string_lossy().into_owned());
            }
            runtime
                .sessions
                .persist_now_result(&session)
                .map_err(|error| RpcFailure::internal(error.to_string()))?;
            Ok(session_result(&runtime, &session))
        }
        "llm/status" => {
            let config = runtime.llm.config();
            Ok(json!({
                "model": config.model,
                "backend": config.backend.label(),
                "base_url": config.base_url,
                "ready": runtime.llm.is_ready(),
                "local": config.backend.is_local(),
                "tool_calling_template_dependent": config.backend.tool_calling_is_template_dependent(),
                "fallbacks": config.fallbacks.iter().map(|endpoint| {
                    serde_json::json!({
                        "backend": endpoint
                            .backend
                            .or_else(|| dsh_llm::LlmBackend::infer_from_base_url(&endpoint.base_url))
                            .unwrap_or(dsh_llm::LlmBackend::OpenAiCompatible)
                            .label(),
                        "base_url": endpoint.base_url,
                        "model": endpoint.model,
                    })
                }).collect::<Vec<_>>(),
                "optimization": runtime.model_profile(),
            }))
        }
        "tasks/list" => {
            let all = optional_bool(&params, "all")?.unwrap_or(false);
            let tasks: Vec<_> = runtime
                .tasks
                .tasks()
                .into_iter()
                .filter(|task| {
                    all || !matches!(task.state, TaskState::Completed | TaskState::Cancelled)
                })
                .collect();
            Ok(json!({"tasks": tasks}))
        }
        "tasks/get" => {
            let id = required_string(&params, "id")?;
            let task = resolve_task(&runtime, &id)?;
            let runs: Vec<_> = runtime
                .tasks
                .runs()
                .into_iter()
                .filter(|run| run.task_id == task.id)
                .collect();
            Ok(json!({
                "task": task,
                "runs": runs,
                "latest_checkpoint": runtime.tasks.latest_checkpoint(&task.id),
            }))
        }
        "tasks/resume" => {
            require_api_key(&runtime)?;
            let wait = optional_bool(&params, "wait")?.unwrap_or(false);
            let id = required_string(&params, "id")?;
            let task = resolve_task(&runtime, &id)?;
            if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                return Err(RpcFailure::invalid_params(format!(
                    "task {} is terminal ({:?})",
                    task.id, task.state
                )));
            }
            if (task.state == TaskState::Running
                && runtime.active_agents.lock().contains_key(&task.id))
                || runtime.is_task_reserved(&task.id)
            {
                return Err(RpcFailure::invalid_params("task is already running"));
            }
            let session_id = task
                .session_id
                .clone()
                .ok_or_else(|| RpcFailure::invalid_params("task has no session"))?;
            let session_guard = SessionTurnGuard::acquire(&runtime, &session_id)?;
            let session = runtime
                .sessions
                .get_or_load(&session_id)
                .map_err(|err| RpcFailure::internal(err.to_string()))?;
            if session.read().goal_paused {
                session.write().goal_paused = false;
                runtime.sessions.persist_now(&session);
            }
            let prompt = params
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or(&task.goal.outcome)
                .to_string();
            let task_id = task.id;
            if wait {
                run_agent(runtime, Some(task_id), session, prompt, session_guard).await
            } else {
                let session_id = session.read().id.clone();
                runtime.reserve_task(task_id.clone());
                spawn_agent(
                    runtime,
                    Some(task_id.clone()),
                    session,
                    prompt,
                    session_guard,
                );
                Ok(json!({
                    "accepted": true,
                    "task_id": task_id,
                    "session_id": session_id,
                    "events": "use events/read_after or events/wait to follow progress"
                }))
            }
        }
        "tasks/verify" => {
            let id = required_string(&params, "id")?;
            let task = resolve_task(&runtime, &id)?;
            let clear = optional_bool(&params, "clear")?.unwrap_or(false);
            let criterion = params.get("criterion").and_then(Value::as_str);
            if !clear
                && criterion
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
            {
                return Err(RpcFailure::invalid_params(
                    "criterion must be provided unless clear=true",
                ));
            }
            if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                return Err(RpcFailure::invalid_params(
                    "terminal tasks cannot change verification criteria",
                ));
            }
            let updated = runtime
                .tasks
                .update_task(&task.id, |task| {
                    if clear {
                        task.goal.verification.clear();
                    } else if let Some(criterion) = criterion {
                        if !task
                            .goal
                            .verification
                            .iter()
                            .any(|value| value == criterion)
                        {
                            task.goal.verification.push(criterion.to_string());
                        }
                    }
                })
                .map_err(|err| RpcFailure::internal(err.to_string()))?;
            Ok(json!({"task": updated}))
        }
        "tasks/pause" => {
            let id = required_string(&params, "id")?;
            let task = resolve_task(&runtime, &id)?;
            if matches!(
                task.state,
                TaskState::Queued
                    | TaskState::Running
                    | TaskState::WaitingApproval
                    | TaskState::WaitingEvent
            ) {
                let requested = runtime.pause_active_task(&task.id);
                let updated = runtime
                    .tasks
                    .transition_task(&task.id, TaskState::Paused)
                    .map_err(|err| RpcFailure::internal(err.to_string()))?;
                Ok(json!({"task": updated, "requested": requested}))
            } else {
                Ok(json!({"task": task, "unchanged": true}))
            }
        }
        "tasks/cancel" => {
            let id = required_string(&params, "id")?;
            let task = resolve_task(&runtime, &id)?;
            let updated = if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                task
            } else {
                let _ = runtime.cancel_active_task(&task.id);
                runtime
                    .tasks
                    .transition_task(&task.id, TaskState::Cancelled)
                    .map_err(|err| RpcFailure::internal(err.to_string()))?
            };
            Ok(json!({"task": updated}))
        }
        "events/read_after" => {
            let sequence = optional_u64(&params, "sequence")?.unwrap_or(0);
            let limit = event_limit(&params)?;
            let (events, latest_sequence) = read_event_batch(&runtime, sequence, limit)?;
            Ok(json!({
                "events": events,
                "latest_sequence": latest_sequence,
            }))
        }
        "events/wait" => {
            let sequence = optional_u64(&params, "sequence")?.unwrap_or(0);
            let limit = event_limit(&params)?;
            let timeout_ms = optional_u64(&params, "timeout_ms")?
                .unwrap_or(30_000)
                .min(120_000);
            wait_for_events(&runtime, sequence, limit, timeout_ms).await
        }
        "approvals/list" => Ok(json!({"approvals": runtime.approvals.pending()})),
        "approvals/resolve" => {
            let request_id = required_string(&params, "request_id")?;
            let allow = params
                .get("allow")
                .and_then(Value::as_bool)
                .ok_or_else(|| RpcFailure::invalid_params("allow must be boolean"))?;
            if !runtime.resolve_approval_request(&request_id, allow) {
                return Err(RpcFailure::invalid_params("approval request not found"));
            }
            Ok(json!({"request_id": request_id, "allow": allow}))
        }
        "tools/list" => Ok(json!({"tools": runtime.tools.definitions()})),
        "computer/status" => Ok(runtime.computer_status()),
        "computer/setEnabled" => {
            let enabled=params.get("enabled").and_then(Value::as_bool)
                .ok_or_else(||RpcFailure::invalid_params("enabled must be boolean"))?;
            runtime.set_computer_enabled(enabled).map_err(|error|RpcFailure::invalid_params(error.to_string()))
        }
        "computer/windows" => {
            let (_cancel,rx)=tokio::sync::watch::channel(false);
            runtime.computer.windows(rx).await.map_err(|error|RpcFailure::internal(error.to_string()))
        }
        "computer/observe" => {
            let window_id=required_string(&params,"window_id")?;
            let (_cancel,rx)=tokio::sync::watch::channel(false);
            runtime.computer.observe(&window_id,true,rx).await.map_err(|error|RpcFailure::internal(error.to_string()))
        }
        "computer/act" => {
            let (_cancel,rx)=tokio::sync::watch::channel(false);
            runtime.computer_operator_action(params,rx).await.map_err(|error|RpcFailure::invalid_params(error.to_string()))
        }
        "cloud/list" => {
            let artifacts = cloud::list_artifacts(&runtime.outer_home)
                .map_err(|err| RpcFailure::internal(err.to_string()))?;
            Ok(json!({"artifacts": artifacts}))
        }
        "cloud/get" => {
            // The app-server boundary accepts an artifact id only. In
            // particular, do not resolve arbitrary paths from a remote
            // client against the server's filesystem.
            let query = params
                .get("query")
                .or_else(|| params.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    RpcFailure::invalid_params("query must be a non-empty artifact id")
                })?;
            let artifact = cloud::load_artifact_id(&runtime.outer_home, &query)
                .map_err(|err| RpcFailure::invalid_params(err.to_string()))?;
            Ok(json!({"artifact": artifact}))
        }
        "cloud/import" => {
            let workspace = required_string(&params, "workspace")?;
            let source = required_string(&params, "source")?;
            let text = required_string(&params, "text")?;
            if text.len() > MAX_RPC_LINE_BYTES {
                return Err(RpcFailure::invalid_params(format!(
                    "artifact text exceeds {MAX_RPC_LINE_BYTES} bytes"
                )));
            }
            let artifact = match cloud::import_artifact_text(
                &runtime.outer_home,
                &PathBuf::from(&workspace),
                source.clone(),
                text,
            ) {
                Ok(artifact) => artifact,
                Err(err) => {
                    let _ = runtime.record_event(
                        dsh_protocol::EventEnvelope::new(
                            "cloud.artifact.rejected",
                            json!({
                                "source": source,
                                "workspace": workspace,
                                "error": err.to_string(),
                            }),
                        )
                        .with_source(dsh_protocol::EventSource::External),
                    );
                    return Err(RpcFailure::invalid_params(err.to_string()));
                }
            };
            if let Err(err) = runtime.record_event(
                dsh_protocol::EventEnvelope::new(
                    "cloud.artifact.imported",
                    json!({
                        "artifact_id": artifact.id,
                        "sha256": artifact.sha256,
                        "source": artifact.source,
                        "workspace": artifact.workspace,
                    }),
                )
                .with_source(dsh_protocol::EventSource::External),
            ) {
                tracing::warn!(error = %err, "cloud artifact imported but audit event failed");
            }
            Ok(json!({"artifact": artifact}))
        }
        "agent/turn" => {
            require_api_key(&runtime)?;
            let wait = optional_bool(&params, "wait")?.unwrap_or(false);
            let prompt = required_string(&params, "prompt")?;
            let session = if let Some(session_id) = params.get("session_id").and_then(Value::as_str)
            {
                resolve_session(&runtime, session_id)?
            } else {
                runtime.sessions.create()
            };
            let task_id = params
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let session_id = session.read().id.clone();
            let session_guard = SessionTurnGuard::acquire(&runtime, &session_id)?;
            // A delete may have completed between resolution and reservation.
            let session = runtime
                .sessions
                .get_or_load(&session_id)
                .map_err(|error| RpcFailure::invalid_params(error.to_string()))?;
            if wait {
                run_agent(runtime, task_id, session, prompt, session_guard).await
            } else {
                let session_id = session.read().id.clone();
                if let Some(task_id) = &task_id {
                    runtime.reserve_task(task_id.clone());
                }
                spawn_agent(runtime, task_id.clone(), session, prompt, session_guard);
                Ok(json!({
                    "accepted": true,
                    "task_id": task_id,
                    "session_id": session_id,
                    "events": "use events/read_after or events/wait to follow progress"
                }))
            }
        }
        other => Err(RpcFailure {
            code: -32601,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}

fn event_limit(params: &Value) -> std::result::Result<usize, RpcFailure> {
    Ok(optional_u64(params, "limit")?.unwrap_or(500).clamp(1, 5000) as usize)
}

fn optional_u64(params: &Value, key: &str) -> std::result::Result<Option<u64>, RpcFailure> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| RpcFailure::invalid_params(format!("{key} must be an unsigned integer")))
}

fn optional_bool(params: &Value, key: &str) -> std::result::Result<Option<bool>, RpcFailure> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| RpcFailure::invalid_params(format!("{key} must be boolean")))
}

fn read_event_batch(
    runtime: &Runtime,
    sequence: u64,
    limit: usize,
) -> std::result::Result<(Vec<dsh_protocol::EventEnvelope>, u64), RpcFailure> {
    // The JSONL backend validates the complete sequence while only retaining
    // the requested suffix in memory. This keeps long-lived event consumers
    // bounded even when the durable log has millions of records.
    let events = runtime
        .events
        .read_after_limit(sequence, limit)
        .map_err(|err| RpcFailure::internal(err.to_string()))?;
    let latest_sequence = runtime.events.latest_sequence();
    Ok((events, latest_sequence))
}

async fn wait_for_events(
    runtime: &Runtime,
    sequence: u64,
    limit: usize,
    timeout_ms: u64,
) -> std::result::Result<Value, RpcFailure> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let (events, latest_sequence) = read_event_batch(runtime, sequence, limit)?;
        if !events.is_empty() || timeout_ms == 0 || Instant::now() >= deadline {
            let timed_out = events.is_empty();
            return Ok(json!({
                "events": events,
                "latest_sequence": latest_sequence,
                "timed_out": timed_out,
            }));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Wake immediately for events produced by this runtime, while keeping
        // a short bounded poll for writers in another process that cannot
        // share the in-process Notify handle.
        let notified = runtime.event_notify.notified();
        tokio::select! {
            _ = notified => {},
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(50))) => {},
        }
    }
}

async fn run_agent(
    runtime: Arc<Runtime>,
    task_id: Option<String>,
    session: Arc<parking_lot::RwLock<dsh_core::Session>>,
    prompt: String,
    _session_guard: SessionTurnGuard,
) -> std::result::Result<Value, RpcFailure> {
    let session_id = session.read().id.clone();
    let agent = AgentLoop::new(runtime.clone());
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let handle = if let Some(task_id) = task_id.clone() {
        agent
            .run_task(task_id, session.clone(), prompt, event_tx)
            .await
            .map_err(|err| RpcFailure::internal(err.to_string()))?
    } else {
        agent
            .run_turn(session.clone(), prompt, event_tx)
            .await
            .map_err(|err| RpcFailure::internal(err.to_string()))?
    };
    if let Some(task_id) = task_id.clone() {
        runtime.register_agent_handle(task_id, handle.clone());
    }

    let mut context = AgentEventContext::for_session(session_id.clone());
    context.task_id = task_id;
    let mut events = Vec::new();
    let mut error = None;
    while let Some(event) = event_rx.recv().await {
        if let AgentEvent::TurnStarted(turn_id) = &event {
            context.turn_id = Some(turn_id.clone());
            if context.task_id.is_none() {
                context.task_id = runtime
                    .tasks
                    .task_for_session(&session_id)
                    .map(|task| task.id);
            }
            context.run_id = context
                .task_id
                .as_deref()
                .and_then(|id| runtime.tasks.latest_run_for_task(id))
                .map(|run| run.id);
            if let Some(task_id) = context.task_id.clone() {
                runtime.register_agent_handle(task_id, handle.clone());
            }
        }
        if let AgentEvent::Error(message) = &event {
            error = Some(message.clone());
        }
        let envelope = event.to_protocol_event(&context);
        let persisted = match runtime.record_event(envelope) {
            Ok(persisted) => persisted,
            Err(err) => {
                if let Some(task_id) = context.task_id.as_deref() {
                    runtime.clear_agent_handle(task_id);
                }
                return Err(RpcFailure::internal(err.to_string()));
            }
        };
        events.push(persisted);
        if matches!(event, AgentEvent::Done) {
            break;
        }
    }
    if let Some(task_id) = context.task_id.as_deref() {
        runtime.clear_agent_handle(task_id);
    }
    Ok(json!({
        "session_id": session_id,
        "task_id": context.task_id,
        "run_id": context.run_id,
        "events": events,
        "ok": error.is_none(),
        "error": error,
    }))
}

fn spawn_agent(
    runtime: Arc<Runtime>,
    task_id: Option<String>,
    session: Arc<parking_lot::RwLock<dsh_core::Session>>,
    prompt: String,
    session_guard: SessionTurnGuard,
) {
    tokio::spawn(async move {
        let session_id = session.read().id.clone();
        if let Err(err) = run_agent(
            runtime.clone(),
            task_id.clone(),
            session,
            prompt,
            session_guard,
        )
        .await
        {
            let mut event = dsh_protocol::EventEnvelope::new(
                "agent.server_error",
                json!({"error": err.message, "session_id":session_id}),
            )
            .with_source(dsh_protocol::EventSource::System);
            if let Some(task_id) = task_id {
                event = event.with_task(task_id);
            }
            if let Err(record_err) = runtime.record_event(event) {
                tracing::debug!(error = %record_err, "failed to persist app-server agent error");
            }
        }
    });
}

fn required_string(params: &Value, key: &str) -> std::result::Result<String, RpcFailure> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RpcFailure::invalid_params(format!("{key} must be a non-empty string")))
}

fn is_loopback_address(address: &str) -> bool {
    let host = address
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(address)
        .trim_matches(['[', ']']);
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        diff |= left.get(index).copied().unwrap_or(0) as usize
            ^ right.get(index).copied().unwrap_or(0) as usize;
    }
    diff == 0
}

fn resolve_task(runtime: &Runtime, query: &str) -> std::result::Result<TaskRecord, RpcFailure> {
    let tasks = runtime.tasks.tasks();
    if let Some(task) = tasks.iter().find(|task| task.id == query) {
        return Ok(task.clone());
    }
    let matches: Vec<_> = tasks
        .iter()
        .filter(|task| task.id.starts_with(query))
        .collect();
    match matches.as_slice() {
        [task] => Ok((*task).clone()),
        [] => Err(RpcFailure::invalid_params(format!(
            "task not found: {query}"
        ))),
        _ => Err(RpcFailure::invalid_params("task prefix is ambiguous")),
    }
}

fn require_api_key(runtime: &Runtime) -> std::result::Result<(), RpcFailure> {
    let config = runtime.llm.config();
    if runtime.llm.is_ready() {
        Ok(())
    } else {
        Err(RpcFailure::unavailable(format!(
            "no usable LLM endpoint for {}; configure credentials or [llm.fallbacks] before agent/turn or tasks/resume",
            config.backend.label()
        )))
    }
}

async fn write_shared_error<W: AsyncWrite + Unpin>(
    writer: &Arc<AsyncMutex<W>>,
    id: Option<Value>,
    code: i64,
    message: String,
    data: Option<Value>,
) -> Result<()> {
    let mut writer = writer.lock().await;
    write_error(&mut *writer, id, code, message, data).await
}

async fn write_result<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: Value,
    result: Value,
) -> Result<()> {
    write_json(
        writer,
        json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
    .await
}

async fn write_error<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: Option<Value>,
    code: i64,
    message: String,
    data: Option<Value>,
) -> Result<()> {
    let mut body = json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message}
    });
    if let Some(data) = data {
        body["error"]["data"] = data;
    }
    write_json(writer, body).await
}

async fn write_json<W: AsyncWrite + Unpin>(writer: &mut W, value: Value) -> Result<()> {
    writer.write_all(value.to_string().as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_core::{AppConfig, GoalSpec, TaskRecord};
    use dsh_llm::DeepSeekClient;
    use dsh_tools::ToolRegistry;
    use std::path::PathBuf;
    use tokio::io::{duplex, split, AsyncBufReadExt, AsyncWriteExt};
    use uuid::Uuid;

    fn test_runtime(label: &str) -> (Arc<Runtime>, PathBuf) {
        let root = std::env::temp_dir().join(format!("dsh-app-server-{label}-{}", Uuid::new_v4()));
        let mut config = AppConfig::builtin_default();
        config.paths.outer_home = root.join("outer").display().to_string();
        config.agent.scheduler_enabled = false;
        let llm = DeepSeekClient::new(config.to_llm_config(String::new())).expect("llm");
        let runtime = Runtime::bootstrap(config, root.clone(), llm, Arc::new(ToolRegistry::new()))
            .expect("runtime");
        (runtime, root)
    }

    async fn roundtrip<R, W>(reader: &mut R, writer: &mut W, request: Value) -> Value
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        writer
            .write_all(format!("{}\n", request).as_bytes())
            .await
            .expect("write request");
        writer.flush().await.expect("flush request");
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut line),
        )
        .await
        .expect("response timeout")
        .expect("read response");
        serde_json::from_str(line.trim()).expect("json response")
    }

    #[test]
    fn required_string_rejects_empty_values() {
        assert!(required_string(&json!({"id": ""}), "id").is_err());
        assert_eq!(
            required_string(&json!({"id": "task-1"}), "id").unwrap(),
            "task-1"
        );
    }

    #[tokio::test]
    async fn computer_rpc_is_opt_in_and_rejects_unvalidated_operator_actions() {
        let (runtime,root)=test_runtime("computer");
        let status=handle_request(runtime.clone(),"computer/status",json!({})).await.unwrap();
        assert_eq!(status["enabled"],false);assert_eq!(status["tools"],json!([]));
        assert!(handle_request(runtime.clone(),"computer/windows",json!({})).await.is_err());
        assert!(handle_request(runtime.clone(),"computer/setEnabled",json!({"enabled":"yes"})).await.is_err());
        assert!(handle_request(runtime.clone(),"computer/act",json!({"action":"click","x":0})).await.is_err());
        assert!(runtime.approvals.pending().is_empty());
        if status["supported"]==true {
            handle_request(runtime.clone(),"computer/setEnabled",json!({"enabled":true})).await.unwrap();
            assert!(dsh_core::load_settings(&runtime.outer_home).computer_enabled);
            assert!(runtime.tools.get("computer_act").is_some());
            runtime.set_permissions(dsh_core::PermissionMode::ReadOnly).unwrap();
            let id=Uuid::new_v4().to_string();
            let result=handle_request(runtime.clone(),"computer/act",json!({"action":"key","window_id":id,"snapshot_id":id,"node_id":"n0","key":"ENTER"})).await.unwrap_err();
            assert!(result.message.contains("read-only"));
            handle_request(runtime.clone(),"computer/setEnabled",json!({"enabled":false})).await.unwrap();
            assert!(runtime.tools.get("computer_act").is_none());
        }
        drop(runtime);std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tcp_prefix_is_accepted_by_listener_parser() {
        assert_eq!(
            "tcp://127.0.0.1:1".strip_prefix("tcp://"),
            Some("127.0.0.1:1")
        );
    }

    #[tokio::test]
    async fn stdio_stream_handles_notifications_parse_errors_and_unknown_methods() {
        let (runtime, root) = test_runtime("protocol");
        let (client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server);
        let server_task = tokio::spawn(serve_stream(
            runtime,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client);
        let mut reader = BufReader::new(client_reader);

        client_writer
            .write_all(b"{not-json}\n")
            .await
            .expect("parse request");
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut line),
        )
        .await
        .expect("parse response timeout")
        .expect("parse response");
        let parse_error: Value = serde_json::from_str(line.trim()).expect("parse json");
        assert_eq!(parse_error["error"]["code"], -32700);

        let initialized = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"}),
        )
        .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2.0");

        let unknown = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 1, "method": "missing/method"}),
        )
        .await;
        assert_eq!(unknown["error"]["code"], -32601);

        client_writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}\n")
            .await
            .expect("notification");
        let pong = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        )
        .await;
        assert_eq!(pong["result"]["ok"], true);

        drop(reader);
        drop(client_writer);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn token_auth_requires_initialize_before_other_requests() {
        let (runtime, root) = test_runtime("auth");
        let (client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server);
        let server_task = tokio::spawn(serve_stream_with_auth(
            runtime,
            BufReader::new(server_reader),
            server_writer,
            Some("secret-token".into()),
        ));
        let (client_reader, mut client_writer) = split(client);
        let mut reader = BufReader::new(client_reader);

        let unauthenticated = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
        )
        .await;
        assert_eq!(unauthenticated["error"]["code"], -32001);

        let wrong = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": {"auth_token": "wrong"}
            }),
        )
        .await;
        assert_eq!(wrong["error"]["code"], -32001);

        let initialized = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "initialize",
                "params": {"auth_token": "secret-token"}
            }),
        )
        .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2.0");
        let ping = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
        )
        .await;
        assert_eq!(ping["result"]["ok"], true);

        drop(reader);
        drop(client_writer);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stdio_stream_exposes_task_events_and_missing_key_error() {
        let (runtime, root) = test_runtime("tasks");
        let task = runtime
            .tasks
            .create_task(TaskRecord::new(GoalSpec::new("verify result")).expect("task"))
            .expect("create task");
        let (client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server);
        let server_task = tokio::spawn(serve_stream(
            runtime,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client);
        let mut reader = BufReader::new(client_reader);

        let list = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tasks/list"}),
        )
        .await;
        assert_eq!(list["result"]["tasks"].as_array().expect("tasks").len(), 1);

        let verified = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tasks/verify",
                "params": {"id": task.id, "criterion": "file_exists:result.txt"}
            }),
        )
        .await;
        assert_eq!(
            verified["result"]["task"]["goal"]["verification"][0],
            "file_exists:result.txt"
        );

        let events = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 5, "method": "events/read_after", "params": {"sequence": 0}}),
        )
        .await;
        assert!(events["result"]["events"].as_array().expect("events").len() >= 2);

        let wait = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 5_1,
                "method": "events/wait",
                "params": {"sequence":  u64::MAX, "timeout_ms": 0}
            }),
        )
        .await;
        assert_eq!(wait["result"]["timed_out"], true);

        let missing_key = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "agent/turn",
                "params": {"prompt": "hello"}
            }),
        )
        .await;
        assert_eq!(missing_key["error"]["code"], -32002);

        drop(reader);
        drop(client_writer);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn events_wait_wakes_after_a_new_durable_event() {
        let (runtime, root) = test_runtime("event-wait");
        let sequence = runtime.events.latest_sequence();
        let writer_runtime = runtime.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            writer_runtime
                .record_system_event("test.wait", &json!({"ok": true}))
                .expect("event");
        });

        let response = wait_for_events(&runtime, sequence, 10, 1000)
            .await
            .expect("wait response");
        assert_eq!(response["timed_out"], false);
        assert_eq!(response["events"][0]["event_type"], "test.wait");
        writer.await.expect("writer");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn long_poll_does_not_block_a_second_request_on_same_connection() {
        let (runtime, root) = test_runtime("concurrent-requests");
        let sequence = runtime.events.latest_sequence();
        let (client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server);
        let server_task = tokio::spawn(serve_stream(
            runtime.clone(),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client);
        let mut reader = BufReader::new(client_reader);
        client_writer
            .write_all(
                format!(
                    "{}\n{}\n",
                    json!({
                        "jsonrpc": "2.0",
                        "id": 10,
                        "method": "events/wait",
                        "params": {"sequence": sequence, "timeout_ms": 1000}
                    }),
                    json!({"jsonrpc": "2.0", "id": 11, "method": "ping"})
                )
                .as_bytes(),
            )
            .await
            .expect("write concurrent requests");
        client_writer.flush().await.expect("flush requests");

        let mut saw_ping = false;
        for _ in 0..2 {
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_millis(500),
                reader.read_line(&mut line),
            )
            .await
            .expect("ping response timeout")
            .expect("read response");
            let response: Value = serde_json::from_str(line.trim()).expect("response json");
            if response["id"] == json!(11) {
                saw_ping = response["result"]["ok"] == json!(true);
                break;
            }
        }
        assert!(
            saw_ping,
            "ping should complete while events/wait is pending"
        );
        runtime
            .record_system_event("test.concurrent", &json!({"ok": true}))
            .expect("wake long poll");
        drop(reader);
        drop(client_writer);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cloud_rpc_round_trip_uses_safe_ids_and_records_import_event() {
        let (runtime, root) = test_runtime("cloud");
        let (client, server) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server);
        let server_task = tokio::spawn(serve_stream(
            runtime.clone(),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client);
        let mut reader = BufReader::new(client_reader);

        let initialized = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
        )
        .await;
        assert_eq!(initialized["result"]["capabilities"]["cloud"], true);

        let imported = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "cloud/import",
                "params": {
                    "workspace": root.display().to_string(),
                    "source": "remote.patch",
                    "text": "*** Add File: remote.txt\n<<<<<<< SEARCH\n=======\nremote\n>>>>>>> REPLACE\n"
                }
            }),
        )
        .await;
        let artifact_id = imported["result"]["artifact"]["id"]
            .as_str()
            .expect("artifact id")
            .to_string();
        assert!(artifact_id.starts_with("art-"));

        let listed = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({"jsonrpc": "2.0", "id": 3, "method": "cloud/list"}),
        )
        .await;
        assert_eq!(listed["result"]["artifacts"].as_array().unwrap().len(), 1);

        let fetched = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "cloud/get",
                "params": {"workspace": root.display().to_string(), "query": artifact_id}
            }),
        )
        .await;
        assert_eq!(fetched["result"]["artifact"]["source"], "remote.patch");

        let rejected_path = roundtrip(
            &mut reader,
            &mut client_writer,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "cloud/get",
                "params": {"query": "..\\outside.json"}
            }),
        )
        .await;
        assert_eq!(rejected_path["error"]["code"], -32602);

        let events = runtime.events.read_all().expect("events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "cloud.artifact.imported"));

        drop(reader);
        drop(client_writer);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(root);
    }
}
