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

/// Extracts the first JSON object of type `T` from model text, ignoring
/// prose and code fences around it (even prose with its own braces).
fn json_object<T: for<'de> Deserialize<'de>>(text: &str) -> Option<T> {
    text.match_indices('{').find_map(|(i, _)| {
        serde_json::Deserializer::from_str(text.get(i..)?)
            .into_iter::<T>()
            .next()?
            .ok()
    })
}

/// The first command line in a plain reply, skipping a code fence and its
/// language tag.
fn first_command_line(text: &str) -> String {
    let t = text.trim();
    let body = t
        .strip_prefix("```")
        .map_or(t, |rest| rest.split_once('\n').map_or(rest, |(_, b)| b));
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("```"))
        .unwrap_or_default()
        .trim_matches('`')
        .trim()
        .to_owned()
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
            command: first_command_line(&res.content),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_ignores_surrounding_braces() {
        let s: Option<Suggestion> = json_object(
            "Here you go:\n```json\n{\"command\": \"ls -la\", \"explanation\": \"lists\"}\n```\nUse {braces} for globs.",
        );
        assert_eq!(s.map(|s| s.command), Some("ls -la".to_owned()));
        let r: Option<Review> =
            json_object("In {short}: {\"safe\": true, \"reason\": \"read only\"} done}");
        assert!(r.is_some_and(|r| r.safe));
        assert!(json_object::<Review>("{\"safe\": ").is_none());
        assert!(json_object::<Review>("no json").is_none());
        assert!(json_object::<Review>("{\"reason\": \"missing safe\"}").is_none());
    }

    #[test]
    fn first_command_line_skips_fences() {
        assert_eq!(first_command_line("```bash\nls -la\n```"), "ls -la");
        assert_eq!(first_command_line("```\n\ncargo build\n```"), "cargo build");
        assert_eq!(first_command_line("`git status`"), "git status");
        assert_eq!(first_command_line("```ls```"), "ls");
        assert_eq!(first_command_line("  pwd  \nmore"), "pwd");
        assert_eq!(first_command_line(""), "");
        assert_eq!(first_command_line("```"), "");
    }
}
