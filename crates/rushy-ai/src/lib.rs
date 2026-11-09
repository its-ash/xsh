//! LLM providers, agent loop, and safety checker for rushy.
//!
//! * [`AiProvider`] is the provider-agnostic trait, implemented by
//!   [`OpenAiProvider`], [`AnthropicProvider`], [`OllamaProvider`], and
//!   [`MockProvider`].
//! * [`Assistant`] adds rushy's prompts: natural language to command,
//!   `explain`, `fix`, and [`Assistant::check`], which runs the rule-based
//!   [`check_command`] guard and then a second-opinion LLM review.
//! * [`agent::run`] is agent mode, capped at [`agent::MAX_STEPS`] steps with
//!   per-step approval.
//! * [`TranscriptStore`] keeps prompts and transcripts encrypted on disk.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

pub mod agent;
mod assistant;
pub mod config;
mod context;
mod error;
pub mod prompts;
mod provider;
mod providers;
pub mod safety;
mod transcript;

pub use assistant::{Assistant, Failure, Settings, Suggestion, run_command_tool};
pub use config::{AiConfig, KeySpec, ProviderConfig, Secret, delete_api_key, store_api_key};
pub use context::Context;
pub use error::{Error, Result};
pub use provider::{
    AiProvider, AiRequest, AiResponse, ChatMessage, Role, StopReason, ToolCall, ToolSpec, tail,
};
pub use providers::{
    AnthropicProvider, MockProvider, OllamaProvider, OpenAiProvider, from_config, list_models,
};
pub use safety::{Verdict, check_command};
pub use transcript::{KEYRING_SERVICE, Transcript, TranscriptStore};

impl From<&config::AiConfig> for Settings {
    fn from(c: &config::AiConfig) -> Self {
        Self {
            temperature: c.default.temperature,
            max_tokens: c.default.max_tokens,
            max_context_tokens: c.default.max_context_tokens,
            safety_review: c.default.safety_review,
        }
    }
}
