//! Async client SDK for the local dsh-rust app-server.
//!
//! The client keeps transport details behind one JSON-RPC implementation and
//! exposes typed helpers for the durable task/event operations used by Codex
//! compatible frontends. Requests are intentionally serialized per client;
//! callers that need parallel work can open multiple connections.

use dsh_protocol::{CheckpointRecord, CloudArtifact, EventEnvelope, RunRecord, TaskRecord};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::io;
use std::ops::{Deref, DerefMut};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::sleep;

pub const JSONRPC_VERSION: &str = "2.0";
/// Bound a single line so a compromised app-server cannot make a client
/// allocate unbounded memory before JSON parsing. Artifact imports are still
/// comfortably below this limit in normal use.
pub const MAX_JSON_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Error)]
pub enum AppServerClientError {
    #[error("app-server I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("app-server JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("app-server RPC error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("app-server protocol error: {0}")]
    Protocol(String),
    #[error("app-server closed the connection")]
    Eof,
}

impl AppServerClientError {
    /// Transport failures are safe to retry only for explicitly idempotent
    /// requests. A lost response to a mutating request must be surfaced so a
    /// caller can decide whether replaying it is safe.
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Io(_) | Self::Eof)
    }
}

pub type Result<T> = std::result::Result<T, AppServerClientError>;

#[derive(Debug, Clone, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResponse {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: Value,
    #[serde(rename = "serverInfo")]
    pub server_info: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetails {
    pub task: TaskRecord,
    pub runs: Vec<RunRecord>,
    pub latest_checkpoint: Option<CheckpointRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsReadAfterResponse {
    pub events: Vec<EventEnvelope>,
    pub latest_sequence: u64,
    #[serde(default)]
    pub timed_out: bool,
}

/// Durable sequence cursor for long-running event consumers. The cursor only
/// advances to the last event actually delivered, so a client can safely
/// restart after a partial batch without skipping records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EventCursor {
    sequence: u64,
    limit: usize,
    timeout_ms: u64,
}

impl EventCursor {
    pub fn new(sequence: u64) -> Self {
        Self {
            sequence,
            limit: 500,
            timeout_ms: 30_000,
        }
    }

    pub fn with_batch(mut self, limit: usize) -> Self {
        self.limit = limit.clamp(1, 5000);
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms.min(120_000);
        self
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub async fn next<R, W>(
        &mut self,
        client: &mut AppServerClient<R, W>,
    ) -> Result<EventsReadAfterResponse>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let response = client
            .events_wait(self.sequence, self.limit, self.timeout_ms)
            .await?;
        if let Some(last) = response.events.last() {
            self.sequence = last.sequence;
        }
        Ok(response)
    }

    pub async fn next_reconnecting(
        &mut self,
        client: &mut ReconnectingTcpAppServerClient,
    ) -> Result<EventsReadAfterResponse> {
        let response = client
            .events_wait(self.sequence, self.limit, self.timeout_ms)
            .await?;
        if let Some(last) = response.events.last() {
            self.sequence = last.sequence;
        }
        Ok(response)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnAccepted {
    pub accepted: bool,
    pub task_id: Option<String>,
    pub session_id: String,
    pub events: String,
}

pub struct AppServerClient<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R, W> AppServerClient<R, W>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
        }
    }

    /// Send a request and deserialize its result into the requested type.
    pub async fn request<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let value = self.request_value(method, params).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Send a request and return the raw JSON result for forward-compatible
    /// methods not yet represented by this SDK.
    pub async fn request_value(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.allocate_id();
        let request = json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_json(request).await?;

        let mut raw_line = Vec::new();
        loop {
            raw_line.clear();
            if self.reader.read_until(b'\n', &mut raw_line).await? == 0 {
                return Err(AppServerClientError::Eof);
            }
            if raw_line.len() > MAX_JSON_LINE_BYTES {
                return Err(AppServerClientError::Protocol(format!(
                    "JSON-RPC response exceeds {} bytes",
                    MAX_JSON_LINE_BYTES
                )));
            }
            let line = String::from_utf8_lossy(&raw_line);
            if line.trim().is_empty() {
                continue;
            }
            let response: RpcResponse = serde_json::from_str(line.trim())?;
            if response
                .jsonrpc
                .as_deref()
                .is_some_and(|version| version != JSONRPC_VERSION)
            {
                return Err(AppServerClientError::Protocol(
                    "response jsonrpc version is not 2.0".into(),
                ));
            }
            // Servers may emit notifications (for example progress events)
            // while a request is in flight. They have no id and must not be
            // mistaken for a malformed response to the pending request.
            let Some(response_id) = response.id else {
                if response.result.is_none() && response.error.is_none() {
                    continue;
                }
                return Err(AppServerClientError::Protocol(
                    "response with result/error is missing id".into(),
                ));
            };
            if response_id != json!(id) {
                return Err(AppServerClientError::Protocol(format!(
                    "response id mismatch: expected {id}, got {}",
                    response_id
                )));
            }
            if let Some(error) = response.error {
                return Err(AppServerClientError::Rpc {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                });
            }
            return response
                .result
                .ok_or_else(|| AppServerClientError::Protocol("response has no result".into()));
        }
    }

    fn allocate_id(&mut self) -> u64 {
        let id = if self.next_id == 0 { 1 } else { self.next_id };
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        id
    }

    /// Send a JSON-RPC notification. Notifications intentionally do not read
    /// a response, matching the app-server transport contract.
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_json(json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": method,
            "params": params,
        }))
        .await
    }

    pub async fn initialize(&mut self) -> Result<InitializeResponse> {
        self.request("initialize", json!({})).await
    }

    pub async fn initialize_with_token(
        &mut self,
        token: Option<&str>,
    ) -> Result<InitializeResponse> {
        let params = token
            .map(|token| json!({"auth_token": token}))
            .unwrap_or_else(|| json!({}));
        self.request("initialize", params).await
    }

    pub async fn ping(&mut self) -> Result<PingResponse> {
        self.request("ping", json!({})).await
    }

    pub async fn llm_status(&mut self) -> Result<Value> {
        self.request_value("llm/status", json!({})).await
    }

    pub async fn tasks_list(&mut self, all: bool) -> Result<Vec<TaskRecord>> {
        #[derive(Deserialize)]
        struct Response {
            tasks: Vec<TaskRecord>,
        }
        Ok(self
            .request::<Response>("tasks/list", json!({"all": all}))
            .await?
            .tasks)
    }

    pub async fn task_get(&mut self, id: &str) -> Result<TaskDetails> {
        self.request("tasks/get", json!({"id": id})).await
    }

    pub async fn cloud_list(&mut self) -> Result<Vec<CloudArtifact>> {
        #[derive(Deserialize)]
        struct Response {
            artifacts: Vec<CloudArtifact>,
        }
        Ok(self
            .request::<Response>("cloud/list", json!({}))
            .await?
            .artifacts)
    }

    pub async fn cloud_get(&mut self, workspace: &str, query: &str) -> Result<CloudArtifact> {
        #[derive(Deserialize)]
        struct Response {
            artifact: CloudArtifact,
        }
        Ok(self
            .request::<Response>("cloud/get", json!({"workspace": workspace, "query": query}))
            .await?
            .artifact)
    }

    pub async fn cloud_import(
        &mut self,
        workspace: &str,
        source: &str,
        text: &str,
    ) -> Result<CloudArtifact> {
        #[derive(Deserialize)]
        struct Response {
            artifact: CloudArtifact,
        }
        Ok(self
            .request::<Response>(
                "cloud/import",
                json!({"workspace": workspace, "source": source, "text": text}),
            )
            .await?
            .artifact)
    }

    pub async fn task_verify(
        &mut self,
        id: &str,
        criterion: Option<&str>,
        clear: bool,
    ) -> Result<TaskRecord> {
        let mut params = json!({"id": id, "clear": clear});
        if let Some(criterion) = criterion {
            params["criterion"] = json!(criterion);
        }
        #[derive(Deserialize)]
        struct Response {
            task: TaskRecord,
        }
        Ok(self.request::<Response>("tasks/verify", params).await?.task)
    }

    pub async fn events_read_after(
        &mut self,
        sequence: u64,
        limit: usize,
    ) -> Result<EventsReadAfterResponse> {
        self.request(
            "events/read_after",
            json!({"sequence": sequence, "limit": limit}),
        )
        .await
    }

    /// Wait until at least one event after `sequence` exists or the bounded
    /// server-side timeout expires.
    pub async fn events_wait(
        &mut self,
        sequence: u64,
        limit: usize,
        timeout_ms: u64,
    ) -> Result<EventsReadAfterResponse> {
        self.request(
            "events/wait",
            json!({
                "sequence": sequence,
                "limit": limit,
                "timeout_ms": timeout_ms,
            }),
        )
        .await
    }

    pub async fn approvals_list(&mut self) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Response {
            approvals: Vec<Value>,
        }
        Ok(self
            .request::<Response>("approvals/list", json!({}))
            .await?
            .approvals)
    }

    pub async fn approval_resolve(&mut self, request_id: &str, allow: bool) -> Result<Value> {
        self.request_value(
            "approvals/resolve",
            json!({"request_id": request_id, "allow": allow}),
        )
        .await
    }

    pub async fn tools_list(&mut self) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Response {
            tools: Vec<Value>,
        }
        Ok(self
            .request::<Response>("tools/list", json!({}))
            .await?
            .tools)
    }

    pub async fn agent_turn(
        &mut self,
        prompt: &str,
        session_id: Option<&str>,
        task_id: Option<&str>,
        wait: bool,
    ) -> Result<Value> {
        let mut params = json!({"prompt": prompt, "wait": wait});
        if let Some(session_id) = session_id {
            params["session_id"] = json!(session_id);
        }
        if let Some(task_id) = task_id {
            params["task_id"] = json!(task_id);
        }
        self.request_value("agent/turn", params).await
    }

    async fn write_json(&mut self, value: Value) -> Result<()> {
        self.writer.write_all(value.to_string().as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }
}

pub type TcpAppServerClient =
    AppServerClient<BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf>;

pub async fn connect_tcp(address: impl ToSocketAddrs) -> Result<TcpAppServerClient> {
    let stream = TcpStream::connect(address).await?;
    let (reader, writer) = stream.into_split();
    Ok(AppServerClient::new(BufReader::new(reader), writer))
}

/// Bounded retry policy for a reconnecting TCP app-server client.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    /// Number of reconnect attempts after the initial transport failure.
    pub max_attempts: usize,
    /// Initial delay between reconnect attempts.
    pub initial_backoff_ms: u64,
    /// Maximum exponential backoff delay.
    pub max_backoff_ms: u64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff_ms: 100,
            max_backoff_ms: 5_000,
        }
    }
}

impl ReconnectPolicy {
    pub fn normalized(mut self) -> Self {
        self.initial_backoff_ms = self.initial_backoff_ms.max(1);
        self.max_backoff_ms = self.max_backoff_ms.max(self.initial_backoff_ms);
        self
    }

    fn delay(self, attempt: usize) -> Duration {
        let exponent = attempt.min(20) as u32;
        let millis = self
            .initial_backoff_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.max_backoff_ms.max(self.initial_backoff_ms));
        Duration::from_millis(millis)
    }
}

/// TCP client that re-establishes the app-server session after a transport
/// drop. Only read-only/idempotent methods are retried automatically.
pub struct ReconnectingTcpAppServerClient {
    address: String,
    policy: ReconnectPolicy,
    client: Option<TcpAppServerClient>,
    initialized: Option<InitializeResponse>,
    auth_token: Option<String>,
}

impl ReconnectingTcpAppServerClient {
    pub async fn connect(address: impl Into<String>, policy: ReconnectPolicy) -> Result<Self> {
        let auth_token = std::env::var("DSH_APP_SERVER_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty());
        Self::connect_with_token(address, policy, auth_token).await
    }

    pub async fn connect_with_token(
        address: impl Into<String>,
        policy: ReconnectPolicy,
        auth_token: Option<String>,
    ) -> Result<Self> {
        let address = normalize_tcp_address(address.into())?;
        let policy = policy.normalized();
        let mut client = Self {
            address,
            policy,
            client: None,
            initialized: None,
            auth_token,
        };
        client.ensure_connected().await?;
        Ok(client)
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    pub async fn reconnect(&mut self) -> Result<InitializeResponse> {
        self.client = None;
        self.initialized = None;
        self.ensure_connected().await
    }

    pub async fn initialize(&mut self) -> Result<InitializeResponse> {
        self.ensure_connected().await
    }

    pub async fn request<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> Result<T> {
        let value = self.request_value(method, params).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Send a notification on the current connection. Notifications are not
    /// replayed after a transport failure because their side effects are
    /// unknown to the client.
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let result = match self.ensure_connected().await {
            Ok(_) => match self.client.as_mut() {
                Some(client) => client.notify(method, params).await,
                None => Err(AppServerClientError::Protocol(
                    "connection disappeared after initialize".into(),
                )),
            },
            Err(error) => Err(error),
        };
        if let Err(error) = &result {
            if error.is_transport() {
                self.client = None;
                self.initialized = None;
            }
        }
        result
    }

    pub async fn request_value(&mut self, method: &str, params: Value) -> Result<Value> {
        let retryable = is_reconnect_safe_method(method);
        let mut attempt = 0;
        loop {
            let result = match self.ensure_connected().await {
                Ok(_) => match self.client.as_mut() {
                    Some(client) => client.request_value(method, params.clone()).await,
                    None => Err(AppServerClientError::Protocol(
                        "connection disappeared after initialize".into(),
                    )),
                },
                Err(error) => Err(error),
            };
            match result {
                Ok(value) => return Ok(value),
                Err(error)
                    if retryable && error.is_transport() && attempt < self.policy.max_attempts =>
                {
                    self.client = None;
                    self.initialized = None;
                    sleep(self.policy.delay(attempt)).await;
                    attempt += 1;
                }
                Err(error) => {
                    if error.is_transport() {
                        self.client = None;
                        self.initialized = None;
                    }
                    return Err(error);
                }
            }
        }
    }

    pub async fn ping(&mut self) -> Result<PingResponse> {
        self.request("ping", json!({})).await
    }

    pub async fn llm_status(&mut self) -> Result<Value> {
        self.request_value("llm/status", json!({})).await
    }

    pub async fn tasks_list(&mut self, all: bool) -> Result<Vec<TaskRecord>> {
        #[derive(Deserialize)]
        struct Response {
            tasks: Vec<TaskRecord>,
        }
        Ok(self
            .request::<Response>("tasks/list", json!({"all": all}))
            .await?
            .tasks)
    }

    pub async fn task_get(&mut self, id: &str) -> Result<TaskDetails> {
        self.request("tasks/get", json!({"id": id})).await
    }

    pub async fn task_verify(
        &mut self,
        id: &str,
        criterion: Option<&str>,
        clear: bool,
    ) -> Result<TaskRecord> {
        let mut params = json!({"id": id, "clear": clear});
        if let Some(criterion) = criterion {
            params["criterion"] = json!(criterion);
        }
        #[derive(Deserialize)]
        struct Response {
            task: TaskRecord,
        }
        Ok(self.request::<Response>("tasks/verify", params).await?.task)
    }

    pub async fn approvals_list(&mut self) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Response {
            approvals: Vec<Value>,
        }
        Ok(self
            .request::<Response>("approvals/list", json!({}))
            .await?
            .approvals)
    }

    pub async fn approval_resolve(&mut self, request_id: &str, allow: bool) -> Result<Value> {
        self.request_value(
            "approvals/resolve",
            json!({"request_id": request_id, "allow": allow}),
        )
        .await
    }

    pub async fn tools_list(&mut self) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct Response {
            tools: Vec<Value>,
        }
        Ok(self
            .request::<Response>("tools/list", json!({}))
            .await?
            .tools)
    }

    pub async fn agent_turn(
        &mut self,
        prompt: &str,
        session_id: Option<&str>,
        task_id: Option<&str>,
        wait: bool,
    ) -> Result<Value> {
        let mut params = json!({"prompt": prompt, "wait": wait});
        if let Some(session_id) = session_id {
            params["session_id"] = json!(session_id);
        }
        if let Some(task_id) = task_id {
            params["task_id"] = json!(task_id);
        }
        self.request_value("agent/turn", params).await
    }

    pub async fn events_read_after(
        &mut self,
        sequence: u64,
        limit: usize,
    ) -> Result<EventsReadAfterResponse> {
        self.request(
            "events/read_after",
            json!({"sequence": sequence, "limit": limit}),
        )
        .await
    }

    pub async fn events_wait(
        &mut self,
        sequence: u64,
        limit: usize,
        timeout_ms: u64,
    ) -> Result<EventsReadAfterResponse> {
        self.request(
            "events/wait",
            json!({
                "sequence": sequence,
                "limit": limit,
                "timeout_ms": timeout_ms,
            }),
        )
        .await
    }

    pub async fn cloud_list(&mut self) -> Result<Vec<CloudArtifact>> {
        #[derive(Deserialize)]
        struct Response {
            artifacts: Vec<CloudArtifact>,
        }
        Ok(self
            .request::<Response>("cloud/list", json!({}))
            .await?
            .artifacts)
    }

    pub async fn cloud_get(&mut self, workspace: &str, query: &str) -> Result<CloudArtifact> {
        #[derive(Deserialize)]
        struct Response {
            artifact: CloudArtifact,
        }
        Ok(self
            .request::<Response>("cloud/get", json!({"workspace": workspace, "query": query}))
            .await?
            .artifact)
    }

    pub async fn cloud_import(
        &mut self,
        workspace: &str,
        source: &str,
        text: &str,
    ) -> Result<CloudArtifact> {
        #[derive(Deserialize)]
        struct Response {
            artifact: CloudArtifact,
        }
        Ok(self
            .request::<Response>(
                "cloud/import",
                json!({"workspace": workspace, "source": source, "text": text}),
            )
            .await?
            .artifact)
    }

    async fn ensure_connected(&mut self) -> Result<InitializeResponse> {
        if let Some(initialized) = &self.initialized {
            return Ok(initialized.clone());
        }
        let mut client = connect_tcp(self.address.clone()).await?;
        let initialized = client
            .initialize_with_token(self.auth_token.as_deref())
            .await?;
        self.client = Some(client);
        self.initialized = Some(initialized.clone());
        Ok(initialized)
    }
}

fn normalize_tcp_address(address: String) -> Result<String> {
    let address = address
        .strip_prefix("tcp://")
        .unwrap_or(&address)
        .trim()
        .to_string();
    if address.is_empty() {
        return Err(AppServerClientError::Protocol(
            "TCP app-server address is empty".into(),
        ));
    }
    if address
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(AppServerClientError::Protocol(
            "TCP app-server address contains whitespace or control characters".into(),
        ));
    }
    if address.starts_with("stdio") {
        return Err(AppServerClientError::Protocol(
            "TCP client requires a host:port address".into(),
        ));
    }
    Ok(address)
}

fn is_reconnect_safe_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "ping"
            | "tasks/list"
            | "tasks/get"
            | "events/read_after"
            | "events/wait"
            | "approvals/list"
            | "tools/list"
            | "cloud/list"
            | "cloud/get"
    )
}

pub struct SpawnedAppServerClient {
    client: AppServerClient<BufReader<ChildStdout>, ChildStdin>,
    child: Child,
}

impl SpawnedAppServerClient {
    pub fn child_id(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.child.kill().await?;
        Ok(())
    }
}

impl Deref for SpawnedAppServerClient {
    type Target = AppServerClient<BufReader<ChildStdout>, ChildStdin>;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl DerefMut for SpawnedAppServerClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.client
    }
}

pub async fn spawn_stdio(
    program: impl AsRef<OsStr>,
    args: &[String],
) -> Result<SpawnedAppServerClient> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppServerClientError::Protocol("stdio child has no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppServerClientError::Protocol("stdio child has no stdout".into()))?;
    Ok(SpawnedAppServerClient {
        client: AppServerClient::new(BufReader::new(stdout), stdin),
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_protocol::TaskState;
    use tokio::io::{duplex, split, AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    #[tokio::test]
    async fn typed_requests_and_rpc_errors_round_trip() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (server_reader, mut server_writer) = split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .expect("initialize request");
            server_writer
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2.0\",\"capabilities\":{},\"serverInfo\":{}}}\n",
                )
                .await
                .expect("initialize response");
            server_writer.flush().await.expect("flush");
            line.clear();
            reader.read_line(&mut line).await.expect("ping request");
            server_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32002,\"message\":\"missing key\"}}\n")
                .await
                .expect("error response");
            server_writer.flush().await.expect("flush");
        });
        let (client_reader, client_writer) = split(client_stream);
        let mut client = AppServerClient::new(BufReader::new(client_reader), client_writer);
        let initialized = client.initialize().await.expect("initialize");
        assert_eq!(initialized.protocol_version, "2.0");
        let error = client.ping().await.expect_err("rpc error");
        assert!(matches!(
            error,
            AppServerClientError::Rpc { code: -32002, .. }
        ));
        server.await.expect("server");
    }

    #[tokio::test]
    async fn request_skips_interleaved_server_notifications() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (server_reader, mut server_writer) = split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("request");
            server_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"progress\",\"params\":{\"n\":1}}\n")
                .await
                .expect("notification");
            server_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n")
                .await
                .expect("response");
            server_writer.flush().await.expect("flush");
        });
        let (client_reader, client_writer) = split(client_stream);
        let mut client = AppServerClient::new(BufReader::new(client_reader), client_writer);
        let response = client.ping().await.expect("ping");
        assert!(response.ok);
        server.await.expect("server");
    }

    #[tokio::test]
    async fn task_and_event_helpers_deserialize_protocol_models() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (server_reader, mut server_writer) = split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("tasks request");
            server_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tasks\":[{\"id\":\"task-1\",\"goal\":{\"outcome\":\"build\"},\"state\":\"queued\",\"created_at\":\"2026-01-01T00:00:00Z\",\"updated_at\":\"2026-01-01T00:00:00Z\"}]}}\n")
                .await
                .expect("tasks response");
            server_writer.flush().await.expect("flush");
            line.clear();
            reader.read_line(&mut line).await.expect("events request");
            server_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"events\":[],\"latest_sequence\":4}}\n")
                .await
                .expect("events response");
            server_writer.flush().await.expect("flush");
        });
        let (client_reader, client_writer) = split(client_stream);
        let mut client = AppServerClient::new(BufReader::new(client_reader), client_writer);
        let tasks = client.tasks_list(false).await.expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-1");
        assert_eq!(tasks[0].state, TaskState::Queued);
        let events = client.events_read_after(0, 10).await.expect("events");
        assert_eq!(events.latest_sequence, 4);
        assert!(events.events.is_empty());
        server.await.expect("server");
    }

    #[tokio::test]
    async fn event_cursor_uses_wait_and_advances_only_delivered_events() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (server_reader, mut server_writer) = split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("wait request");
            server_writer
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"events\":[{\"sequence\":7,\"event_type\":\"test\",\"payload\":{},\"occurred_at\":\"2026-01-01T00:00:00Z\"}],\"latest_sequence\":8,\"timed_out\":false}}\n",
                )
                .await
                .expect("wait response");
            server_writer.flush().await.expect("flush");
        });
        let (client_reader, client_writer) = split(client_stream);
        let mut client = AppServerClient::new(BufReader::new(client_reader), client_writer);
        let mut cursor = EventCursor::new(0).with_batch(10).with_timeout_ms(1000);
        let response = cursor.next(&mut client).await.expect("cursor response");
        assert_eq!(response.events.len(), 1);
        assert_eq!(cursor.sequence(), 7);
        assert!(!response.timed_out);
        server.await.expect("server");
    }

    #[tokio::test]
    async fn reconnecting_client_reinitializes_after_transport_drop_for_ping() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address").to_string();
        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("first connection");
            let (first_reader, mut first_writer) = first.into_split();
            let mut first_reader = BufReader::new(first_reader);
            let mut line = String::new();
            first_reader
                .read_line(&mut line)
                .await
                .expect("first initialize");
            assert!(line.contains("\"method\":\"initialize\""));
            first_writer
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2.0\",\"capabilities\":{},\"serverInfo\":{}}}\n",
                )
                .await
                .expect("first initialize response");
            first_writer.flush().await.expect("first flush");
            drop(first_reader);
            drop(first_writer);

            let (second, _) = listener.accept().await.expect("second connection");
            let (second_reader, mut second_writer) = second.into_split();
            let mut second_reader = BufReader::new(second_reader);
            line.clear();
            second_reader
                .read_line(&mut line)
                .await
                .expect("second initialize");
            assert!(line.contains("\"method\":\"initialize\""));
            second_writer
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2.0\",\"capabilities\":{\"cloud\":true},\"serverInfo\":{}}}\n",
                )
                .await
                .expect("second initialize response");
            second_writer.flush().await.expect("second flush");
            line.clear();
            second_reader
                .read_line(&mut line)
                .await
                .expect("ping request");
            assert!(line.contains("\"method\":\"ping\""));
            second_writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n")
                .await
                .expect("ping response");
            second_writer.flush().await.expect("ping flush");
        });

        let mut client = ReconnectingTcpAppServerClient::connect(
            address,
            ReconnectPolicy {
                max_attempts: 2,
                initial_backoff_ms: 1,
                max_backoff_ms: 2,
            },
        )
        .await
        .expect("initial connection");
        let ping = client.ping().await.expect("reconnected ping");
        assert!(ping.ok);
        assert!(client.is_connected());
        timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server timeout")
            .expect("server task");
    }

    #[tokio::test]
    async fn reconnecting_client_does_not_replay_mutating_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address").to_string();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("connection");
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("initialize");
            writer
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2.0\",\"capabilities\":{},\"serverInfo\":{}}}\n",
                )
                .await
                .expect("initialize response");
            writer.flush().await.expect("initialize flush");
            line.clear();
            reader.read_line(&mut line).await.expect("mutating request");
            assert!(line.contains("\"method\":\"agent/turn\""));
            drop(reader);
            drop(writer);

            // A non-idempotent transport failure must not cause a second
            // connection. Keep the listener alive briefly so an accidental
            // replay would be observable as a test failure.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });

        let mut client = ReconnectingTcpAppServerClient::connect(
            address,
            ReconnectPolicy {
                max_attempts: 3,
                initial_backoff_ms: 1,
                max_backoff_ms: 2,
            },
        )
        .await
        .expect("initial connection");
        let error = client
            .request_value("agent/turn", json!({"prompt": "mutate"}))
            .await
            .expect_err("mutating request should surface transport error");
        assert!(error.is_transport());
        assert!(!client.is_connected());
        timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("server timeout")
            .expect("server task");
    }
}
