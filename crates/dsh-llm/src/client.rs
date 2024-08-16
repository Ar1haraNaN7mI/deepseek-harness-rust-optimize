use crate::types::*;
use anyhow::Context;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use parking_lot::RwLock;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc;

pub struct DeepSeekClient {
    http: Client,
    config: RwLock<LlmConfig>,
}

impl DeepSeekClient {
    /// Construct a client. Empty API key is allowed so the CLI/TUI can boot
    /// and configure credentials later (`set_api_key`).
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        Ok(Self {
            http: Client::new(),
            config: RwLock::new(config),
        })
    }

    pub fn config(&self) -> LlmConfig {
        self.config.read().clone()
    }

    pub fn has_api_key(&self) -> bool {
        !self.config.read().api_key.trim().is_empty()
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

    pub fn set_temperature(&self, temperature: f32) {
        self.config.write().temperature = temperature;
    }

    pub async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        tx: mpsc::Sender<LlmEvent>,
    ) -> Result<AssembledResponse, LlmError> {
        self.stream_chat_cancellable(messages, tools, tx, None)
            .await
    }

    pub async fn stream_chat_cancellable(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        tx: mpsc::Sender<LlmEvent>,
        mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<AssembledResponse, LlmError> {
        let (api_key, base_url, model, thinking, temperature, max_tokens) = {
            let cfg = self.config.read();
            if cfg.api_key.trim().is_empty() {
                return Err(LlmError::MissingApiKey);
            }
            (
                cfg.api_key.clone(),
                cfg.base_url.clone(),
                cfg.model.clone(),
                cfg.thinking,
                cfg.temperature,
                cfg.max_tokens,
            )
        };

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });

        if thinking {
            body["thinking"] = json!({ "type": "enabled" });
        } else {
            body["thinking"] = json!({ "type": "disabled" });
        }

        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        let response = self
            .http
            .post(&url)
            .bearer_auth(&api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Http(e.to_string()))?;


        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!("{status}: {text}")));
        }

        let mut stream = response.bytes_stream().eventsource();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_acc: BTreeMap<usize, AccumToolCall> = BTreeMap::new();
        let mut finish = FinishReason::Stop;

        loop {
            if let Some(c) = cancel.as_mut() {
                if *c.borrow() {
                    let _ = tx
                        .send(LlmEvent::Error("cancelled".into()))
                        .await;
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
        let handle = tokio::spawn(async move {
            while rx.recv().await.is_some() {}
        });
        let result = self.stream_chat(messages, tools, tx).await;
        let _ = handle.await;
        result
    }
}

#[derive(Default)]
struct AccumToolCall {
    id: String,
    name: String,
    arguments: String,
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
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
}
