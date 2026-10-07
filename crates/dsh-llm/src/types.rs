use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use thiserror::Error;

/// OpenAI-compatible transport presets.  The client deliberately keeps one
/// wire format so local open-source servers can be swapped without changing
/// the agent loop or tool protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    #[serde(alias = "deepseek", alias = "deep_seek")]
    #[default]
    DeepSeek,
    Ollama,
    #[serde(alias = "llamacpp", alias = "llama.cpp")]
    LlamaCpp,
    Vllm,
    Sglang,
    #[serde(alias = "lite_llm")]
    LiteLlm,
    #[serde(alias = "openai", alias = "compatible")]
    OpenAiCompatible,
    /// LocalAI's OpenAI-compatible gateway.
    #[serde(alias = "local-ai", alias = "localai")]
    LocalAi,
    /// Hugging Face Text Generation Inference OpenAI-compatible router.
    #[serde(
        alias = "text-generation-inference",
        alias = "text_generation_inference"
    )]
    Tgi,
    /// The MLX-LM OpenAI-compatible server, primarily for Apple Silicon.
    #[serde(alias = "mlx", alias = "mlx_lm")]
    MlxLm,
    /// LM Studio's local OpenAI-compatible server.
    #[serde(alias = "lm-studio", alias = "lmstudio", alias = "lm studio")]
    LmStudio,
}

impl LlmBackend {
    pub const ALL: [Self; 11] = [
        Self::DeepSeek,
        Self::Ollama,
        Self::LlamaCpp,
        Self::Vllm,
        Self::Sglang,
        Self::LiteLlm,
        Self::OpenAiCompatible,
        Self::LocalAi,
        Self::Tgi,
        Self::MlxLm,
        Self::LmStudio,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::Ollama => "ollama",
            Self::LlamaCpp => "llama.cpp",
            Self::Vllm => "vLLM",
            Self::Sglang => "SGLang",
            Self::LiteLlm => "LiteLLM",
            Self::OpenAiCompatible => "openai-compatible",
            Self::LocalAi => "LocalAI",
            Self::Tgi => "TGI",
            Self::MlxLm => "MLX-LM",
            Self::LmStudio => "LM Studio",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::Ollama => "ollama",
            Self::LlamaCpp => "llama_cpp",
            Self::Vllm => "vllm",
            Self::Sglang => "sglang",
            Self::LiteLlm => "litellm",
            Self::OpenAiCompatible => "openai_compatible",
            Self::LocalAi => "localai",
            Self::Tgi => "tgi",
            Self::MlxLm => "mlx_lm",
            Self::LmStudio => "lm_studio",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::DeepSeek => "https://api.deepseek.com",
            Self::Ollama => "http://127.0.0.1:11434/v1",
            Self::LlamaCpp => "http://127.0.0.1:8080/v1",
            Self::Vllm => "http://127.0.0.1:8000/v1",
            Self::Sglang => "http://127.0.0.1:30000/v1",
            Self::LiteLlm => "http://127.0.0.1:4000/v1",
            Self::OpenAiCompatible => "http://127.0.0.1:8000/v1",
            // LocalAI, TGI and MLX-LM commonly use port 8080.  Keep the
            // presets explicit even though users may run more than one on a
            // host and override the URL in that case.
            Self::LocalAi => "http://127.0.0.1:8080/v1",
            Self::Tgi => "http://127.0.0.1:8080/v1",
            Self::MlxLm => "http://127.0.0.1:8080/v1",
            Self::LmStudio => "http://127.0.0.1:1234/v1",
        }
    }

    /// Local OSS servers (including a local LiteLLM gateway) generally accept
    /// an empty bearer token. DeepSeek's hosted endpoint requires one; a
    /// remote compatible endpoint can still enforce auth server-side and
    /// return its own error.
    pub fn requires_api_key(self) -> bool {
        matches!(self, Self::DeepSeek)
    }

    pub fn supports_deepseek_thinking_field(self) -> bool {
        matches!(self, Self::DeepSeek)
    }

    /// Whether the preset is intended for a local, self-hosted runtime. This
    /// is informational and does not bypass server-side authentication.
    pub fn is_local(self) -> bool {
        matches!(
            self,
            Self::Ollama
                | Self::LlamaCpp
                | Self::Vllm
                | Self::Sglang
                | Self::LiteLlm
                | Self::LocalAi
                | Self::Tgi
                | Self::MlxLm
                | Self::LmStudio
        )
    }

    /// Most presets can carry OpenAI tool definitions, but TGI/MLX-LM/LM
    /// Studio only support them when the selected chat template exposes
    /// function calling.
    /// The client keeps sending tools by default; this capability is exposed
    /// to frontends so they can warn or choose a fallback before a run.
    pub fn tool_calling_is_template_dependent(self) -> bool {
        matches!(
            self,
            Self::Tgi | Self::MlxLm | Self::LlamaCpp | Self::LmStudio
        )
    }

    /// Best-effort inference for a fallback URL when its TOML entry omits a
    /// backend. Explicit `backend` always wins; unknown URLs deliberately
    /// return `None` and are handled as generic OpenAI-compatible endpoints.
    pub fn infer_from_base_url(base_url: &str) -> Option<Self> {
        let lower = base_url.to_ascii_lowercase();
        if lower.contains(":11434") {
            Some(Self::Ollama)
        } else if lower.contains(":30000") {
            Some(Self::Sglang)
        } else if lower.contains(":4000") {
            Some(Self::LiteLlm)
        } else if lower.contains(":1234") {
            Some(Self::LmStudio)
        } else if lower.contains(":8000") {
            Some(Self::Vllm)
        } else if lower.contains(":8080") {
            Some(Self::OpenAiCompatible)
        } else {
            None
        }
    }
}

impl FromStr for LlmBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '.', ' '], "_")
            .as_str()
        {
            "deepseek" | "deep_seek" => Ok(Self::DeepSeek),
            "ollama" => Ok(Self::Ollama),
            "llama_cpp" | "llamacpp" => Ok(Self::LlamaCpp),
            "vllm" => Ok(Self::Vllm),
            "sglang" => Ok(Self::Sglang),
            "litellm" | "lite_llm" => Ok(Self::LiteLlm),
            "openai" | "openai_compatible" | "compatible" => Ok(Self::OpenAiCompatible),
            "localai" | "local_ai" => Ok(Self::LocalAi),
            "tgi" | "text_generation_inference" => Ok(Self::Tgi),
            "mlx" | "mlx_lm" => Ok(Self::MlxLm),
            "lm_studio" | "lmstudio" => Ok(Self::LmStudio),
            other => Err(format!(
                "unknown LLM backend `{other}` (expected deepseek|ollama|llama_cpp|vllm|sglang|litellm|openai_compatible|localai|tgi|mlx_lm|lm_studio)"
            )),
        }
    }
}

/// An optional OpenAI-compatible endpoint used when the primary endpoint is
/// unavailable before streaming starts.  Keeping this as data in `LlmConfig`
/// lets a local setup fail over from (for example) Ollama to llama.cpp without
/// coupling the agent loop to a particular serving framework.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmEndpoint {
    pub base_url: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub backend: Option<LlmBackend>,
    /// Prefer an environment variable for secrets. This field is accepted for
    /// backwards-compatible config files but is never serialized by clients.
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    /// Name of an environment variable containing the endpoint key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub extra_body: Option<Value>,
}

impl LlmEndpoint {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: None,
            // Leave the preset open so a well-known port can be inferred
            // (11434 → Ollama, 4000 → LiteLLM, etc.). Unknown URLs fall back
            // to the generic OpenAI-compatible transport in the client.
            backend: None,
            api_key: None,
            api_key_env: None,
            extra_body: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub thinking: bool,
    pub max_tokens: u32,
    pub temperature: f32,
    pub backend: LlmBackend,
    /// Optional provider-specific JSON fields, merged without overwriting
    /// reserved request fields (`model`, `messages`, `stream`, etc.).
    pub extra_body: Option<Value>,
    /// Endpoints tried in order if the primary request cannot be established
    /// or returns a non-success status before any stream is consumed.
    pub fallbacks: Vec<LlmEndpoint>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-v4-pro".into(),
            thinking: true,
            max_tokens: 8192,
            temperature: 0.2,
            backend: LlmBackend::DeepSeek,
            extra_body: None,
            fallbacks: Vec::new(),
        }
    }
}

/// Per-request overrides used by the model-size policy.  `None` preserves the
/// client defaults, so providers and existing callers remain compatible.
#[derive(Debug, Clone, Default)]
pub struct LlmRequestOptions {
    pub thinking: Option<bool>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Ask providers to serialize tool calls. `None` leaves their default.
    pub parallel_tool_calls: Option<bool>,
    /// Additional provider-specific JSON fields. Reserved request fields are
    /// never overwritten by this object.
    pub extra_body: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentPart {
    Text(String),
    Parts(Vec<Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
    /// DeepSeek reasoning / thinking content when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(ContentPart::Text(text.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(ContentPart::Text(text.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(ContentPart::Text(text.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(ContentPart::Text(content.into())),
            name: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    pub fn text(&self) -> String {
        match &self.content {
            Some(ContentPart::Text(t)) => t.clone(),
            Some(ContentPart::Parts(_)) => String::new(),
            None => String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionCallDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_json")]
    pub arguments: Option<String>,
}

/// OpenAI uses a JSON string for streamed function arguments, while several
/// local servers (notably Ollama-compatible adapters) return an object in a
/// non-stream response. Normalize both forms to the string representation the
/// agent loop already validates.
fn deserialize_string_or_json<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(other) => serde_json::to_string(&other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionCallDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta(ToolCallDelta),
    Finished(FinishReason),
    Error(String),
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("missing LLM API key — run `dsh config set-api-key <KEY>` or TUI `/apikey <KEY>`")]
    MissingApiKey,
    #[error("http error: {0}")]
    Http(String),
    #[error("api error: {0}")]
    Api(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_aliases_and_presets_are_stable() {
        assert_eq!(
            "deepseek".parse::<LlmBackend>().unwrap(),
            LlmBackend::DeepSeek
        );
        assert_eq!(
            "llama.cpp".parse::<LlmBackend>().unwrap(),
            LlmBackend::LlamaCpp
        );
        assert_eq!(
            LlmBackend::Ollama.default_base_url(),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            LlmBackend::LiteLlm.default_base_url(),
            "http://127.0.0.1:4000/v1"
        );
        assert_eq!(
            LlmBackend::LmStudio.default_base_url(),
            "http://127.0.0.1:1234/v1"
        );
        assert!(!LlmBackend::Ollama.requires_api_key());
        assert!(LlmBackend::DeepSeek.requires_api_key());
    }

    #[test]
    fn backend_config_serde_accepts_deepseek_alias() {
        let parsed: LlmBackend = serde_json::from_str("\"deepseek\"").unwrap();
        assert_eq!(parsed, LlmBackend::DeepSeek);
    }

    #[test]
    fn backend_config_serde_accepts_lm_studio_alias() {
        let parsed: LlmBackend = serde_json::from_str("\"lm-studio\"").unwrap();
        assert_eq!(parsed, LlmBackend::LmStudio);
    }

    #[test]
    fn function_arguments_accept_string_and_object_forms() {
        let string_form: FunctionCallDelta =
            serde_json::from_value(serde_json::json!({"arguments": "{\"x\":1}"})).unwrap();
        assert_eq!(string_form.arguments.as_deref(), Some("{\"x\":1}"));
        let object_form: FunctionCallDelta =
            serde_json::from_value(serde_json::json!({"arguments": {"x": 1}})).unwrap();
        assert_eq!(object_form.arguments.as_deref(), Some("{\"x\":1}"));
    }

    #[test]
    fn open_source_backend_aliases_are_stable() {
        assert_eq!(
            "local-ai".parse::<LlmBackend>().unwrap(),
            LlmBackend::LocalAi
        );
        assert_eq!(
            "text-generation-inference".parse::<LlmBackend>().unwrap(),
            LlmBackend::Tgi
        );
        assert_eq!("mlx".parse::<LlmBackend>().unwrap(), LlmBackend::MlxLm);
        assert_eq!(
            "lm-studio".parse::<LlmBackend>().unwrap(),
            LlmBackend::LmStudio
        );
        assert_eq!(
            "lmstudio".parse::<LlmBackend>().unwrap(),
            LlmBackend::LmStudio
        );
        assert_eq!(
            "lm studio".parse::<LlmBackend>().unwrap(),
            LlmBackend::LmStudio
        );
        assert!(LlmBackend::Tgi.tool_calling_is_template_dependent());
        assert!(LlmBackend::LmStudio.tool_calling_is_template_dependent());
        assert!(LlmBackend::LocalAi.is_local());
        assert!(LlmBackend::LmStudio.is_local());
        assert!(!LlmBackend::LmStudio.requires_api_key());
        assert!(!LlmBackend::OpenAiCompatible.is_local());
        assert_eq!(
            LlmBackend::infer_from_base_url("http://127.0.0.1:11434/v1"),
            Some(LlmBackend::Ollama)
        );
        assert_eq!(
            LlmBackend::infer_from_base_url("http://127.0.0.1:1234/v1"),
            Some(LlmBackend::LmStudio)
        );
        assert!(LlmEndpoint::new("http://127.0.0.1:11434/v1")
            .backend
            .is_none());
    }
}
