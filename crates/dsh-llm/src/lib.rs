//! DeepSeek V4 OpenAI-compatible streaming client.

mod client;
mod types;

pub use client::{AssembledResponse, DeepSeekClient};
pub use types::{
    ChatMessage, ContentPart, FinishReason, FunctionCallDelta, LlmConfig, LlmError, LlmEvent,
    Role, ToolCallDelta, ToolFunctionSpec, ToolSpec,
};
