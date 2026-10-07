use crate::types::*;
use anyhow::Context;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use parking_lot::RwLock;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct DeepSeekClient {
    http: Client,
    config: RwLock<LlmConfig>,
    send_local_api_key: RwLock<Option<bool>>,
}

/// Provider-neutral name for new integrations. `DeepSeekClient` remains the
/// compatibility name used by the existing core and CLI.
pub type OpenAiCompatibleClient = DeepSeekClient;

impl DeepSeekClient {
    /// Construct a client. Empty API key is allowed so the CLI/TUI can boot
    /// and configure credentials later (`set_api_key`).
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| LlmError::Http(error.to_string()))?;
        Ok(Self {
            http,
            config: RwLock::new(config),
            send_local_api_key: RwLock::new(None),
        })
    }

    pub fn config(&self) -> LlmConfig {
        self.config.read().clone()
    }

    pub fn has_api_key(&self) -> bool {
        !self.config.read().api_key.trim().is_empty()
    }

    /// Return whether at least one configured endpoint can be attempted with
    /// the currently available credentials. A local fallback therefore keeps
    /// the CLI usable even when the primary hosted provider is not configured.
    pub fn is_ready(&self) -> bool {
        let cfg = self.config.read();
        if !cfg.backend.requires_api_key() || !cfg.api_key.trim().is_empty() {
            return true;
        }
        cfg.fallbacks.iter().any(|endpoint| {
            let backend = endpoint
                .backend
                .or_else(|| LlmBackend::infer_from_base_url(&endpoint.base_url))
                .unwrap_or(LlmBackend::OpenAiCompatible);
            let endpoint_key = endpoint
                .api_key
                .clone()
                .or_else(|| {
                    endpoint
                        .api_key_env
                        .as_deref()
                        .and_then(|name| std::env::var(name).ok())
                })
                .unwrap_or_else(|| cfg.api_key.clone());
            !backend.requires_api_key() || !endpoint_key.trim().is_empty()
        })
    }

    pub fn set_api_key(&self, api_key: impl Into<String>) {
        self.config.write().api_key = api_key.into();
    }

    pub fn clear_api_key(&self) {
        self.config.write().api_key.clear();
    }

    pub fn set_model(&self, model: impl Into<String>) {
        self.config.write().model = model.into();
    }

    pub fn set_thinking(&self, thinking: bool) {
        self.config.write().thinking = thinking;
    }

    pub fn set_backend(&self, backend: LlmBackend) {
        self.config.write().backend = backend;
    }

    pub fn set_base_url(&self, base_url: impl Into<String>) {
        self.config.write().base_url = base_url.into();
    }

    pub fn set_extra_body(&self, extra_body: Option<Value>) {
        self.config.write().extra_body = extra_body;
    }

    pub fn set_fallbacks(&self, fallbacks: Vec<LlmEndpoint>) {
        self.config.write().fallbacks = fallbacks;
    }

    pub fn set_temperature(&self, temperature: f32) {
        self.config.write().temperature = temperature;
    }

    pub fn set_max_tokens(&self, max_tokens: u32) {
        self.config.write().max_tokens = max_tokens;
    }

    pub fn set_send_local_api_key(&self, enabled: bool) {
        *self.send_local_api_key.write() = Some(enabled);
    }

    pub fn sends_local_api_key(&self) -> bool {
        self.send_local_api_key.read().unwrap_or_else(|| {
            std::env::var("DSH_LLM_SEND_API_KEY")
                .ok()
                .is_some_and(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
        })
    }

    pub async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        tx: mpsc::Sender<LlmEvent>,
    ) -> Result<AssembledResponse, LlmError> {
        self.stream_chat_cancellable_with_options(messages, tools, tx, None, None)
            .await
    }

    pub async fn stream_chat_cancellable(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        tx: mpsc::Sender<LlmEvent>,
        cancel: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<AssembledResponse, LlmError> {
        self.stream_chat_cancellable_with_options(messages, tools, tx, cancel, None)
            .await
    }

    /// Stream a request while applying ephemeral per-request overrides. This
    /// is used by the <70B policy so concurrent sessions do not race on global
    /// client settings.
    pub async fn stream_chat_cancellable_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        tx: mpsc::Sender<LlmEvent>,
        mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
        options: Option<LlmRequestOptions>,
    ) -> Result<AssembledResponse, LlmError> {
        let (primary, fallbacks, default_thinking, default_temperature, default_max_tokens) = {
            let cfg = self.config.read();
            let primary = ResolvedEndpoint {
                api_key: cfg.api_key.clone(),
                base_url: cfg.base_url.clone(),
                model: cfg.model.clone(),
                backend: cfg.backend,
                extra_body: cfg.extra_body.clone(),
            };
            let fallbacks = cfg
                .fallbacks
                .iter()
                .map(|endpoint| ResolvedEndpoint {
                    api_key: endpoint
                        .api_key
                        .clone()
                        .or_else(|| {
                            endpoint
                                .api_key_env
                                .as_deref()
                                .and_then(|name| std::env::var(name).ok())
                        })
                        .unwrap_or_else(|| cfg.api_key.clone()),
                    base_url: endpoint.base_url.clone(),
                    model: endpoint.model.clone().unwrap_or_else(|| cfg.model.clone()),
                    backend: endpoint
                        .backend
                        .or_else(|| LlmBackend::infer_from_base_url(&endpoint.base_url))
                        .unwrap_or(LlmBackend::OpenAiCompatible),
                    extra_body: merge_extra_values(
                        cfg.extra_body.clone(),
                        endpoint.extra_body.clone(),
                    ),
                })
                .collect::<Vec<_>>();
            (
                primary,
                fallbacks,
                cfg.thinking,
                cfg.temperature,
                cfg.max_tokens,
            )
        };

        let request_options = options.as_ref();
        if cancel.as_ref().is_some_and(|receiver| *receiver.borrow()) {
            let _ = tx.send(LlmEvent::Error("cancelled".into())).await;
            return Err(LlmError::Api("cancelled".into()));
        }
        let thinking = request_options
            .and_then(|o| o.thinking)
            .unwrap_or(default_thinking);
        let temperature = request_options
            .and_then(|o| o.temperature)
            .unwrap_or(default_temperature);
        let max_tokens = request_options
            .and_then(|o| o.max_tokens)
            .unwrap_or(default_max_tokens);

        // Fail over only before consuming a stream. Once deltas have been
        // emitted, retrying could duplicate visible output and tool calls.
        let mut response = None;
        let mut last_error = None;
        let total_attempts = 1 + fallbacks.len();
        for (attempt, endpoint) in std::iter::once(primary).chain(fallbacks).enumerate() {
            if cancel.as_ref().is_some_and(|receiver| *receiver.borrow()) {
                let _ = tx.send(LlmEvent::Error("cancelled".into())).await;
                return Err(LlmError::Api("cancelled".into()));
            }
            if endpoint.backend.requires_api_key() && endpoint.api_key.trim().is_empty() {
                last_error = Some(LlmError::MissingApiKey);
                continue;
            }
            let base_url = if endpoint.base_url.trim().is_empty() {
                endpoint.backend.default_base_url().to_string()
            } else {
                endpoint.base_url.clone()
            };
            let url = completion_url(&base_url);
            let tuning = RequestTuning {
                thinking,
                temperature,
                max_tokens,
                parallel_tool_calls: request_options.and_then(|o| o.parallel_tool_calls),
            };
            let body = request_body(
                messages,
                tools,
                &endpoint,
                tuning,
                request_options.and_then(|o| o.extra_body.as_ref()),
            );
            let mut request = self
                .http
                .post(&url)
                .header("Content-Type", "application/json");
            if should_send_api_key(&endpoint, &base_url, self.sends_local_api_key()) {
                request = request.bearer_auth(&endpoint.api_key);
            }
            match request.json(&body).send().await {
                Ok(candidate) if candidate.status().is_success() => {
                    response = Some(candidate);
                    if attempt > 0 {
                        tracing::info!(
                            backend = endpoint.backend.label(),
                            attempt,
                            "LLM request succeeded on fallback endpoint"
                        );
                    }
                    break;
                }
                Ok(candidate) => {
                    let status = candidate.status();
                    let text = candidate.text().await.unwrap_or_default();
                    last_error = Some(LlmError::Api(format!("{status}: {text}")));
                    if attempt + 1 < total_attempts {
                        tracing::warn!(
                            backend = endpoint.backend.label(),
                            status = %status,
                            "LLM endpoint rejected request; trying fallback"
                        );
                    }
                }
                Err(error) => {
                    last_error = Some(LlmError::Http(error.to_string()));
                    if attempt + 1 < total_attempts {
                        tracing::warn!(
                            backend = endpoint.backend.label(),
                            error = %error,
                            "LLM endpoint unavailable; trying fallback"
                        );
                    }
                }
            }
        }
        let response = response.ok_or_else(|| {
            last_error.unwrap_or_else(|| LlmError::Http("no LLM endpoint configured".into()))
        })?;

        // A few OpenAI-compatible local servers ignore `stream=true` unless a
        // feature flag is enabled and return one JSON completion instead. It
        // is still safe to expose that response through the same event API.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Missing content-type was common in older llama.cpp builds and those
        // responses are SSE, so preserve the streaming path unless the server
        // explicitly declares a JSON (non-stream) payload.
        if content_type.contains("application/json")
            || content_type.contains("application/problem+json")
        {
            if cancel.as_ref().is_some_and(|receiver| *receiver.borrow()) {
                let _ = tx.send(LlmEvent::Error("cancelled".into())).await;
                return Err(LlmError::Api("cancelled".into()));
            }
            let payload = response
                .text()
                .await
                .map_err(|error| LlmError::Http(error.to_string()))?;
            return assemble_non_stream_response(&payload, tx).await;
        }

        let mut stream = response.bytes_stream().eventsource();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_acc: BTreeMap<usize, AccumToolCall> = BTreeMap::new();
        let mut finish = FinishReason::Stop;

        loop {
            if let Some(c) = cancel.as_mut() {
                if *c.borrow() {
                    let _ = tx.send(LlmEvent::Error("cancelled".into())).await;
                    return Err(LlmError::Api("cancelled".into()));
                }
            }

            let next = tokio::select! {
                item = stream.next() => item,
                _ = async {
                    if let Some(c) = cancel.as_mut() {
                        let _ = c.changed().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if cancel.as_ref().map(|c| *c.borrow()).unwrap_or(false) {
                        let _ = tx.send(LlmEvent::Error("cancelled".into())).await;
                        return Err(LlmError::Api("cancelled".into()));
                    }
                    continue;
                }
            };

            let Some(item) = next else {
                break;
            };
            let event = item.map_err(|e| LlmError::Http(e.to_string()))?;
            if event.data.trim() == "[DONE]" {
                break;
            }
            let chunk: StreamChunk = match serde_json::from_str(&event.data) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let Some(choice) = chunk.choices.first() else {
                continue;
            };
            if let Some(delta) = &choice.delta {
                if let Some(c) = &delta.content {
                    if !c.is_empty() {
                        text.push_str(c);
                        let _ = tx.send(LlmEvent::TextDelta(c.clone())).await;
                    }
                }
                if let Some(r) = &delta.reasoning_content {
                    if !r.is_empty() {
                        reasoning.push_str(r);
                        let _ = tx.send(LlmEvent::ReasoningDelta(r.clone())).await;
                    }
                }
                if let Some(calls) = &delta.tool_calls {
                    for call in calls {
                        let idx = call.index.unwrap_or(0);
                        let entry = tool_acc.entry(idx).or_default();
                        if let Some(id) = &call.id {
                            entry.id = id.clone();
                        }
                        if let Some(func) = &call.function {
                            if let Some(name) = &func.name {
                                entry.name.push_str(name);
                            }
                            if let Some(args) = &func.arguments {
                                entry.arguments.push_str(args);
                            }
                        }
                        let _ = tx.send(LlmEvent::ToolCallDelta(call.clone())).await;
                    }
                }
            }
            if let Some(reason) = &choice.finish_reason {
                finish = match reason.as_str() {
                    "stop" => FinishReason::Stop,
                    "tool_calls" => FinishReason::ToolCalls,
                    "length" => FinishReason::Length,
                    other => FinishReason::Other(other.to_string()),
                };
            }
        }

        let tool_calls: Vec<ToolCallDelta> = tool_acc
            .into_iter()
            .map(|(index, acc)| ToolCallDelta {
                index: Some(index),
                id: if acc.id.is_empty() {
                    Some(format!("call_{index}"))
                } else {
                    Some(acc.id)
                },
                r#type: Some("function".into()),
                function: Some(FunctionCallDelta {
                    name: Some(acc.name),
                    arguments: Some(acc.arguments),
                }),
            })
            .collect();

        if !tool_calls.is_empty() {
            finish = FinishReason::ToolCalls;
        }

        let _ = tx.send(LlmEvent::Finished(finish.clone())).await;

        Ok(AssembledResponse {
            text,
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            tool_calls,
            finish_reason: finish,
        })
    }

    pub async fn complete_once(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssembledResponse, LlmError> {
        let (tx, mut rx) = mpsc::channel(64);
        let handle = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = self.stream_chat(messages, tools, tx).await;
        let _ = handle.await;
        result
    }
}

#[derive(Debug, Clone)]
struct ResolvedEndpoint {
    api_key: String,
    base_url: String,
    model: String,
    backend: LlmBackend,
    extra_body: Option<Value>,
}

#[derive(Debug, Clone, Copy)]
struct RequestTuning {
    thinking: bool,
    temperature: f32,
    max_tokens: u32,
    parallel_tool_calls: Option<bool>,
}

/// Build one provider-neutral request. Provider-specific fields are supplied
/// through `extra_body`; reserved OpenAI fields always win so a config typo
/// cannot silently change the active model, messages, or streaming mode.
fn request_body(
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    endpoint: &ResolvedEndpoint,
    tuning: RequestTuning,
    request_extra: Option<&Value>,
) -> Value {
    let mut body = json!({
        "model": endpoint.model,
        "messages": messages,
        "stream": true,
        "temperature": tuning.temperature,
        "max_tokens": tuning.max_tokens,
    });

    if endpoint.backend.supports_deepseek_thinking_field() {
        body["thinking"] = if tuning.thinking {
            json!({ "type": "enabled" })
        } else {
            json!({ "type": "disabled" })
        };
    }

    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
        if let Some(parallel) = tuning.parallel_tool_calls {
            body["parallel_tool_calls"] = json!(parallel);
        }
    }

    merge_extra_body(&mut body, endpoint.extra_body.as_ref());
    merge_extra_body(&mut body, request_extra);
    body
}

fn merge_extra_body(body: &mut Value, extra: Option<&Value>) {
    let Some(extra) = extra.and_then(Value::as_object) else {
        return;
    };
    let Some(target) = body.as_object_mut() else {
        return;
    };
    for (key, value) in extra {
        // Request options are intentionally additive. The canonical fields
        // below are controlled by dsh-rust and cannot be overridden here.
        if !matches!(
            key.as_str(),
            "model"
                | "messages"
                | "stream"
                | "temperature"
                | "max_tokens"
                | "thinking"
                | "tools"
                | "tool_choice"
                | "parallel_tool_calls"
        ) {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn merge_extra_values(base: Option<Value>, overlay: Option<Value>) -> Option<Value> {
    match (base, overlay) {
        (Some(mut base), Some(overlay)) => {
            if let (Some(target), Some(source)) = (base.as_object_mut(), overlay.as_object()) {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
                Some(base)
            } else {
                Some(overlay)
            }
        }
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (None, None) => None,
    }
}

/// Avoid leaking a hosted credential to a loopback OSS server when a user
/// switches backends in one process. Explicitly opt in with
/// `DSH_LLM_SEND_API_KEY=1` when a local gateway intentionally requires auth.
fn should_send_api_key(
    endpoint: &ResolvedEndpoint,
    base_url: &str,
    send_local_api_key: bool,
) -> bool {
    if endpoint.api_key.trim().is_empty() {
        return false;
    }
    if endpoint.backend.requires_api_key() {
        return true;
    }
    if send_local_api_key {
        return true;
    }
    !is_loopback_url(base_url)
}

fn is_loopback_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    let authority = lower
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(lower.as_str())
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

async fn assemble_non_stream_response(
    payload: &str,
    tx: mpsc::Sender<LlmEvent>,
) -> Result<AssembledResponse, LlmError> {
    let response: CompletionResponse = serde_json::from_str(payload)
        .map_err(|error| LlmError::Api(format!("invalid non-stream completion: {error}")))?;
    let Some(choice) = response.choices.into_iter().next() else {
        return Err(LlmError::Api(
            "non-stream completion contained no choices".into(),
        ));
    };
    let message = choice.message.unwrap_or_default();
    let text = message.content.or(choice.text).unwrap_or_default();
    if !text.is_empty() {
        let _ = tx.send(LlmEvent::TextDelta(text.clone())).await;
    }
    let reasoning = message.reasoning_content.filter(|value| !value.is_empty());
    if let Some(value) = &reasoning {
        let _ = tx.send(LlmEvent::ReasoningDelta(value.clone())).await;
    }
    let tool_calls = message.tool_calls.unwrap_or_default();
    for call in &tool_calls {
        let _ = tx.send(LlmEvent::ToolCallDelta(call.clone())).await;
    }
    let finish_reason = if !tool_calls.is_empty() {
        FinishReason::ToolCalls
    } else {
        finish_reason(choice.finish_reason.as_deref())
    };
    let _ = tx.send(LlmEvent::Finished(finish_reason.clone())).await;
    Ok(AssembledResponse {
        text,
        reasoning,
        tool_calls,
        finish_reason,
    })
}

fn finish_reason(reason: Option<&str>) -> FinishReason {
    match reason.unwrap_or("stop") {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        other => FinishReason::Other(other.to_string()),
    }
}

fn completion_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

#[derive(Default)]
struct AccumToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    choices: Vec<CompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct CompletionChoice {
    #[serde(default)]
    message: Option<CompletionMessage>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CompletionMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Clone)]
pub struct AssembledResponse {
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: FinishReason,
}

impl AssembledResponse {
    pub fn as_assistant_message(&self) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: if self.text.is_empty() {
                None
            } else {
                Some(ContentPart::Text(self.text.clone()))
            },
            name: None,
            tool_call_id: None,
            tool_calls: if self.tool_calls.is_empty() {
                None
            } else {
                Some(self.tool_calls.clone())
            },
            reasoning_content: self.reasoning.clone(),
        }
    }

    pub fn parsed_tool_calls(&self) -> anyhow::Result<Vec<(String, String, Value)>> {
        let mut out = Vec::new();
        for call in &self.tool_calls {
            let id = call.id.clone().unwrap_or_else(|| "call".into());
            let func = call
                .function
                .as_ref()
                .context("tool call missing function")?;
            let name = func.name.clone().unwrap_or_default();
            let args_raw = func.arguments.clone().unwrap_or_else(|| "{}".into());
            let args: Value = serde_json::from_str(&args_raw).unwrap_or_else(|_| json!({}));
            out.push((id, name, args));
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: Option<StreamDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[cfg(test)]
mod tests {
    use super::{
        completion_url, is_loopback_url, merge_extra_values, request_body, should_send_api_key,
        RequestTuning, ResolvedEndpoint,
    };
    use crate::types::{
        ChatMessage, FinishReason, LlmBackend, LlmConfig, LlmEndpoint, LlmEvent, ToolSpec,
    };
    use serde_json::json;

    #[test]
    fn completion_url_accepts_base_or_full_endpoint() {
        assert_eq!(
            completion_url("http://127.0.0.1:8000/v1"),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(
            completion_url("http://127.0.0.1:8000/v1/chat/completions/"),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
    }

    #[test]
    fn extra_body_is_additive_and_reserved_fields_are_protected() {
        let endpoint = ResolvedEndpoint {
            api_key: String::new(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            model: "qwen2.5:14b".into(),
            backend: LlmBackend::Ollama,
            extra_body: Some(json!({
                "cache_prompt": true,
                "model": "wrong-model"
            })),
        };
        let tool = ToolSpec {
            kind: "function".into(),
            function: crate::types::ToolFunctionSpec {
                name: "read_file".into(),
                description: "read".into(),
                parameters: json!({"type": "object"}),
            },
        };
        let body = request_body(
            &[ChatMessage::user("hi")],
            &[tool],
            &endpoint,
            RequestTuning {
                thinking: false,
                temperature: 0.1,
                max_tokens: 256,
                parallel_tool_calls: Some(false),
            },
            Some(&json!({"top_k": 20, "stream": false})),
        );
        assert_eq!(body["model"], "qwen2.5:14b");
        assert_eq!(body["stream"], true);
        assert_eq!(body["cache_prompt"], true);
        assert_eq!(body["top_k"], 20);
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn endpoint_extra_body_overlays_without_dropping_global_fields() {
        let merged = merge_extra_values(
            Some(json!({"cache_prompt": true, "top_k": 20})),
            Some(json!({"top_k": 10})),
        )
        .unwrap();
        assert_eq!(merged["cache_prompt"], true);
        assert_eq!(merged["top_k"], 10);
    }

    #[test]
    fn hosted_keys_are_not_sent_to_loopback_local_backends_by_default() {
        let endpoint = ResolvedEndpoint {
            api_key: "secret".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen".into(),
            backend: LlmBackend::Ollama,
            extra_body: None,
        };
        assert!(is_loopback_url(&endpoint.base_url));
        if std::env::var("DSH_LLM_SEND_API_KEY").is_err() {
            assert!(!should_send_api_key(&endpoint, &endpoint.base_url, false));
            assert!(should_send_api_key(&endpoint, &endpoint.base_url, true));
        }
        let remote = ResolvedEndpoint {
            base_url: "https://llm.example/v1".into(),
            ..endpoint
        };
        assert!(should_send_api_key(&remote, &remote.base_url, false));
    }

    #[test]
    fn local_fallback_makes_missing_primary_key_ready() {
        let config = LlmConfig {
            fallbacks: vec![LlmEndpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: None,
                backend: None,
                api_key: None,
                api_key_env: None,
                extra_body: None,
            }],
            ..LlmConfig::default()
        };
        let client = super::DeepSeekClient::new(config).unwrap();
        assert!(client.is_ready());
    }

    #[tokio::test]
    async fn fallback_endpoint_is_used_before_streaming() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"fallback-ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let config = LlmConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            backend: LlmBackend::OpenAiCompatible,
            fallbacks: vec![LlmEndpoint {
                base_url: format!("http://{address}/v1"),
                model: None,
                backend: Some(LlmBackend::OpenAiCompatible),
                api_key: None,
                api_key_env: None,
                extra_body: None,
            }],
            ..LlmConfig::default()
        };
        let client = super::DeepSeekClient::new(config).unwrap();
        let result = client
            .complete_once(&[ChatMessage::user("hello")], &[])
            .await
            .unwrap();
        assert_eq!(result.text, "fallback-ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn non_stream_json_response_is_projected_to_events() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let response = super::assemble_non_stream_response(
            r#"{"choices":[{"message":{"content":"json-ok"},"finish_reason":"stop"}]}"#,
            tx,
        )
        .await
        .unwrap();
        assert_eq!(response.text, "json-ok");
        assert!(matches!(rx.recv().await, Some(LlmEvent::TextDelta(text)) if text == "json-ok"));
        assert!(matches!(
            rx.recv().await,
            Some(LlmEvent::Finished(FinishReason::Stop))
        ));
    }
}
