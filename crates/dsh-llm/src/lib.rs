//! OpenAI-compatible streaming client for DeepSeek and local open-source
//! inference servers.

mod client;
mod types;

pub use client::{AssembledResponse, DeepSeekClient, OpenAiCompatibleClient};
pub use types::{
    ChatMessage, ContentPart, FinishReason, FunctionCallDelta, LlmBackend, LlmConfig, LlmEndpoint,
    LlmError, LlmEvent, LlmRequestOptions, Role, ToolCallDelta, ToolFunctionSpec, ToolSpec,
};
