//! Provider-agnostic request and response types and the [`AiProvider`] trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Instructions.
    System,
    /// The user.
    User,
    /// The model.
    Assistant,
    /// A tool result.
    Tool,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id, echoed in the result.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON arguments.
    pub arguments: serde_json::Value,
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author.
    pub role: Role,
    /// Text content.
    pub content: String,
    /// Tool calls (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Call this message answers (tool messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    /// An assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::new(Role::Assistant, content)
        }
    }

    /// A tool result.
    #[must_use]
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call_id.into()),
            ..Self::new(Role::Tool, content)
        }
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// JSON Schema of the arguments.
    pub parameters: serde_json::Value,
}

/// A completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiRequest {
    /// Conversation so far.
    pub messages: Vec<ChatMessage>,
    /// Tools for agentic loops.
    pub tools: Vec<ToolSpec>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Output token limit.
    pub max_tokens: u32,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// Finished normally.
    End,
    /// Wants tool results.
    ToolUse,
    /// Hit `max_tokens`.
    Length,
    /// Anything else.
    Other,
}

/// A completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiResponse {
    /// Text output.
    pub content: String,
    /// Requested tool calls.
    pub tool_calls: Vec<ToolCall>,
    /// Stop reason.
    pub stop: StopReason,
}

/// A chat model backend.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Runs one completion.
    async fn complete(&self, req: AiRequest) -> Result<AiResponse>;
    /// Provider name, such as `openai`.
    fn name(&self) -> &str;
}

impl std::fmt::Debug for dyn AiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AiProvider({})", self.name())
    }
}

/// Cuts `text` to at most `max` bytes on a character boundary, keeping the end.
#[must_use]
pub fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default()
}
