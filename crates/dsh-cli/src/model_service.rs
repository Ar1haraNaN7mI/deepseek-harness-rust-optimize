//! DSH model connection settings, backed by the same client used by agent turns.
use crate::app_server::RpcFailure;
use dsh_core::{Runtime, SettingsPatch};
use dsh_llm::{ChatMessage, DeepSeekClient, LlmBackend, LlmEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

type RpcResult = Result<Value, RpcFailure>;
static CONNECTION_WRITE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(crate) async fn dispatch(
    runtime: &Arc<Runtime>,
    method: &str,
    params: &Value,
) -> Option<RpcResult> {
    Some(match method {
        "model/service" => Ok(snapshot(runtime)),
        "model/update" => {
            let _guard = CONNECTION_WRITE.get_or_init(Default::default).lock().await;
            update(runtime, params)
        }
        "model/credential" => {
            let _guard = CONNECTION_WRITE.get_or_init(Default::default).lock().await;
            credential(runtime, params)
        }
        "model/test" => probe(runtime).await,
        _ => return None,
    })
}

fn internal(error: impl std::fmt::Display) -> RpcFailure {
    RpcFailure::internal(error.to_string())
}

fn safe_endpoint(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(value.to_owned())
}

fn snapshot(runtime: &Runtime) -> Value {
    let config = runtime.llm.config();
    json!({
        "backend":config.backend.id(), "base_url":safe_endpoint(&config.base_url),
        "model":config.model, "temperature":config.temperature,"max_tokens":config.max_tokens,
        "thinking":config.thinking,"credential_configured":runtime.llm.has_api_key(),
        "send_local_api_key":runtime.llm.sends_local_api_key(),
        "saved_credential":dsh_core::credentials::credentials_path(&runtime.outer_home).is_file(),
        "ready":runtime.llm.is_ready(),"fallback_count":config.fallbacks.len(),
        "backends": LlmBackend::ALL.iter().map(|backend| json!({"id":backend.id(),"label":if *backend == LlmBackend::OpenAiCompatible {"兼容协议"} else {backend.label()},"base_url":backend.default_base_url(),"requires_key":backend.requires_api_key()})).collect::<Vec<_>>()
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionPatch {
    backend: String,
    base_url: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
    thinking: bool,
    #[serde(default)]
    send_local_api_key: bool,
}

fn update(runtime: &Runtime, params: &Value) -> RpcResult {
    let patch: ConnectionPatch = serde_json::from_value(params.clone()).map_err(|_| {
        RpcFailure::invalid_params(
            "Expected backend, base_url, model, temperature, max_tokens and thinking",
        )
    })?;
    let backend = patch
        .backend
        .parse::<LlmBackend>()
        .map_err(RpcFailure::invalid_params)?;
    runtime
        .update_settings(SettingsPatch {
            backend: Some(backend),
            base_url: Some(patch.base_url),
            model: Some(patch.model),
            temperature: Some(patch.temperature),
            max_tokens: Some(patch.max_tokens),
            thinking: Some(patch.thinking),
            send_local_api_key: Some(patch.send_local_api_key),
            ..Default::default()
        })
        .map_err(internal)?;
    Ok(snapshot(runtime))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialPatch {
    action: String,
    key: Option<String>,
}

fn credential(runtime: &Runtime, params: &Value) -> RpcResult {
    let input: CredentialPatch = serde_json::from_value(params.clone())
        .map_err(|_| RpcFailure::invalid_params("Expected action and optional key"))?;
    match input.action.as_str() {
        "save" => {
            let key = input
                .key
                .ok_or_else(|| RpcFailure::invalid_params("key is required"))?;
            dsh_core::save_api_key(&runtime.outer_home, &key).map_err(internal)?;
            runtime.llm.set_api_key(key.trim());
        }
        "clear" if input.key.is_none() => {
            dsh_core::clear_api_key(&runtime.outer_home).map_err(internal)?;
            runtime.llm.clear_api_key();
        }
        _ => return Err(RpcFailure::invalid_params("action must be save or clear")),
    }
    Ok(snapshot(runtime))
}

/// One tiny, explicit inference request. Never sends conversation history or
/// silently tests a fallback instead of the service the form describes.
async fn probe(runtime: &Runtime) -> RpcResult {
    let mut config = runtime.llm.config();
    if config.backend.requires_api_key() && config.api_key.trim().is_empty() {
        return Err(RpcFailure::invalid_params("请先配置模型服务的 API Key"));
    }
    if safe_endpoint(&config.base_url).is_none() {
        return Err(RpcFailure::invalid_params("请先保存不含内嵌凭据的服务地址"));
    }
    config.fallbacks.clear();
    config.thinking = false;
    config.max_tokens = 16;
    config.extra_body = None;
    let client =
        DeepSeekClient::new(config).map_err(|_| RpcFailure::internal("无法创建模型连接"))?;
    client.set_send_local_api_key(runtime.llm.sends_local_api_key());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<LlmEvent>(32);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let started = Instant::now();
    let messages = vec![ChatMessage::user("Reply with OK.")];
    let result = tokio::time::timeout(
        Duration::from_secs(25),
        client.stream_chat(&messages, &[], tx),
    )
    .await;
    drain.abort();
    match result {
        Ok(Ok(response)) if !response.text.trim().is_empty() => Ok(
            json!({"ok":true,"latency_ms":started.elapsed().as_millis(),"message":"已收到当前模型的真实回复；未发送聊天记录。"}),
        ),
        Ok(Ok(_)) => Err(RpcFailure::internal(
            "服务已响应，但没有返回文本；请检查模型名称和接口协议",
        )),
        Ok(Err(_)) => Err(RpcFailure::internal(
            "模型请求失败，请检查服务地址、模型名称、API Key 和服务端状态",
        )),
        Err(_) => Err(RpcFailure::internal("模型测试超过 25 秒，请检查服务状态")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_core::AppConfig;
    use dsh_tools::ToolRegistry;
    fn fixture() -> (Arc<Runtime>, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("dsh-model-settings-{}", uuid::Uuid::new_v4()));
        let mut config = AppConfig::builtin_default();
        config.paths.outer_home = root.join("outer").display().to_string();
        config.agent.scheduler_enabled = false;
        let client = DeepSeekClient::new(config.to_llm_config("test-secret".into())).unwrap();
        let runtime =
            Runtime::bootstrap(config, root.clone(), client, Arc::new(ToolRegistry::new()))
                .unwrap();
        (runtime, root)
    }
    #[tokio::test]
    async fn model_update_is_durable_and_applies_to_actual_client() {
        let (runtime, root) = fixture();
        let params = json!({"backend":"ollama","base_url":"http://127.0.0.1:11434/v1","model":"qwen3:8b","temperature":0.4,"max_tokens":2048,"thinking":false,"send_local_api_key":true});
        let result = dispatch(&runtime, "model/update", &params)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["model"], "qwen3:8b");
        assert!(!result.to_string().contains("test-secret"));
        assert_eq!(runtime.llm.config().base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(runtime.llm.config().max_tokens, 2048);
        let loaded = dsh_core::load_settings(&runtime.outer_home);
        assert_eq!(loaded.backend, Some(LlmBackend::Ollama));
        assert_eq!(loaded.max_tokens, Some(2048));
        let restored = Runtime::bootstrap(
            runtime.config.clone(),
            root.clone(),
            DeepSeekClient::new(runtime.config.to_llm_config(String::new())).unwrap(),
            Arc::new(ToolRegistry::new()),
        )
        .unwrap();
        assert_eq!(restored.llm.config().base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(restored.llm.config().model, "qwen3:8b");
        assert_eq!(restored.llm.config().max_tokens, 2048);
        assert!(restored.llm.sends_local_api_key());
        let mut bad = params.clone();
        bad["base_url"] = json!("https://user:secret@example.com/v1");
        assert!(dispatch(&runtime, "model/update", &bad)
            .await
            .unwrap()
            .is_err());
        assert_eq!(runtime.llm.config().base_url, "http://127.0.0.1:11434/v1");
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn endpoint_read_does_not_expose_embedded_credentials() {
        assert!(safe_endpoint("https://user:secret@host/v1").is_none());
        assert!(safe_endpoint("https://host/v1?key=secret").is_none());
        assert_eq!(
            safe_endpoint("http://localhost:8000/v1").as_deref(),
            Some("http://localhost:8000/v1")
        );
    }
    #[tokio::test]
    async fn model_test_makes_a_real_request_without_chat_history() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let end = request.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
            let body: Value = serde_json::from_slice(&request[end + 4..]).unwrap();
            assert_eq!(body["messages"].as_array().unwrap().len(), 1);
            assert_eq!(body["messages"][0]["content"], "Reply with OK.");
            assert_eq!(body["max_tokens"], 16);
            let response = r#"{"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}]}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
        });
        let (runtime, root) = fixture();
        runtime.llm.set_base_url(format!("http://{address}/v1"));
        let result = probe(&runtime).await.unwrap();
        assert_eq!(result["ok"], true);
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
