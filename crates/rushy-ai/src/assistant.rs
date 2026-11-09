//! High-level AI actions: suggest, explain, fix, and the combined safety check.

use serde::Deserialize;
use serde_json::json;

use crate::context::Context;
use crate::provider::{AiProvider, AiRequest, ChatMessage, ToolSpec, tail};
use crate::safety::{Verdict, check_command};
use crate::{Result, prompts};

const STDERR_LIMIT: usize = 4_000;

/// A proposed command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Suggestion {
    /// Command line; empty when the model declined.
    #[serde(default)]
    pub command: String,
    /// Why, in one or two sentences.
    #[serde(default)]
    pub explanation: String,
}

/// A command that failed, for [`Assistant::fix`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failure {
    /// Command line.
    pub command: String,
    /// Captured standard error (tail).
    pub stderr: String,
    /// Exit status.
    pub exit_code: i32,
    /// Working directory.
    pub cwd: String,
    /// `git status --short`, if in a repository.
    pub git_status: Option<String>,
}

#[derive(Deserialize)]
struct Review {
    safe: bool,
    #[serde(default)]
    reason: String,
}

/// Request settings shared by every call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Sampling temperature.
    pub temperature: f32,
    /// Output limit.
    pub max_tokens: u32,
    /// Context budget.
    pub max_context_tokens: u32,
    /// Run the LLM safety review after the rule guard.
    pub safety_review: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: 1_024,
            max_context_tokens: 8_000,
            safety_review: true,
        }
    }
}

/// Wraps a provider with rushy's prompts and safety checks.
pub struct Assistant<'a> {
    provider: &'a dyn AiProvider,
    settings: Settings,
}

impl std::fmt::Debug for Assistant<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assistant")
            .field("provider", &self.provider.name())
            .field("settings", &self.settings)
            .finish()
    }
}

/// Extracts the first JSON object from model text (tolerating code fences).
fn json_object<T: for<'de> Deserialize<'de>>(text: &str) -> Option<T> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// The `run_command` tool used by agent mode.
#[must_use]
pub fn run_command_tool() -> ToolSpec {
    ToolSpec {
        name: "run_command".into(),
        description: "Run one command in the user's xsh shell and return its exit status, stdout, and stderr.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command line to run."},
                "reason": {"type": "string", "description": "Why this step is needed."}
            },
            "required": ["command"]
        }),
    }
}

impl<'a> Assistant<'a> {
    /// An assistant using `provider`.
    #[must_use]
    pub fn new(provider: &'a dyn AiProvider, settings: Settings) -> Self {
        Self { provider, settings }
    }

    /// The provider.
    #[must_use]
    pub fn provider(&self) -> &'a dyn AiProvider {
        self.provider
    }

    /// Settings.
    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Builds a request with the system prompt plus context.
    #[must_use]
    pub fn request(&self, system: &str, ctx: &Context, messages: Vec<ChatMessage>) -> AiRequest {
        let mut all = vec![ChatMessage::system(format!(
            "{system}\n\n# Shell context\n{}",
            ctx.render(self.settings.max_context_tokens)
        ))];
        all.extend(messages);
        AiRequest {
            messages: all,
            tools: Vec::new(),
            temperature: self.settings.temperature,
            max_tokens: self.settings.max_tokens,
        }
    }

    /// Request for natural language to command.
    #[must_use]
    pub fn suggest_request(&self, nl: &str, ctx: &Context) -> AiRequest {
        self.request(prompts::NL_TO_COMMAND, ctx, vec![ChatMessage::user(nl)])
    }

    /// Turns natural language into a command.
    ///
    /// # Errors
    ///
    /// Fails if the provider fails.
    pub async fn suggest(&self, nl: &str, ctx: &Context) -> Result<Suggestion> {
        let res = self
            .provider
            .complete(self.suggest_request(nl, ctx))
            .await?;
        Ok(json_object(&res.content).unwrap_or_else(|| Suggestion {
            command: res
                .content
                .trim()
                .trim_matches('`')
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            explanation: String::new(),
        }))
    }

    /// Explains a command in plain English.
    ///
    /// # Errors
    ///
    /// Fails if the provider fails.
    pub async fn explain(&self, command: &str, ctx: &Context) -> Result<String> {
        let req = self.request(prompts::EXPLAIN, ctx, vec![ChatMessage::user(command)]);
        Ok(self.provider.complete(req).await?.content.trim().to_owned())
    }

    /// Request for `ai fix`.
    #[must_use]
    pub fn fix_request(&self, f: &Failure, ctx: &Context) -> AiRequest {
        let mut body = format!(
            "Command: {}\nExit code: {}\nWorking directory: {}\nStderr:\n{}\n",
            f.command,
            f.exit_code,
            f.cwd,
            tail(&f.stderr, STDERR_LIMIT)
        );
        if let Some(g) = &f.git_status {
            body.push_str("git status --short:\n");
            body.push_str(tail(g, 2_000));
        }
        self.request(prompts::FIX, ctx, vec![ChatMessage::user(body)])
    }

    /// Suggests a fix for a failed command.
    ///
    /// # Errors
    ///
    /// Fails if the provider fails.
    pub async fn fix(&self, failure: &Failure, ctx: &Context) -> Result<Suggestion> {
        let res = self
            .provider
            .complete(self.fix_request(failure, ctx))
            .await?;
        Ok(json_object(&res.content).unwrap_or(Suggestion {
            command: String::new(),
            explanation: res.content.trim().to_owned(),
        }))
    }

    /// Request for the second-opinion review.
    #[must_use]
    pub fn review_request(&self, command: &str, ctx: &Context) -> AiRequest {
        self.request(
            prompts::SAFETY_REVIEW,
            ctx,
            vec![ChatMessage::user(format!("Command: {command}"))],
        )
    }

    /// Rule guard first, then (if enabled) the LLM review. An unclear or failed
    /// review blocks the command.
    pub async fn check(&self, command: &str, ctx: &Context) -> Verdict {
        let rules = check_command(command);
        if !rules.is_allowed() || !self.settings.safety_review {
            return rules;
        }
        match self
            .provider
            .complete(self.review_request(command, ctx))
            .await
        {
            Ok(res) => match json_object::<Review>(&res.content) {
                Some(r) if r.safe => Verdict::Allow,
                Some(r) => Verdict::Block {
                    rule: "llm-review",
                    reason: if r.reason.is_empty() {
                        "the safety review flagged this command".into()
                    } else {
                        r.reason
                    },
                },
                None => Verdict::Block {
                    rule: "llm-review",
                    reason: "the safety review gave no clear answer".into(),
                },
            },
            Err(e) => Verdict::Block {
                rule: "llm-review",
                reason: format!("the safety review failed: {e}"),
            },
        }
    }
}
