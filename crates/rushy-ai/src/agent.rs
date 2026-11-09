//! Agent mode: plan, run approved commands, observe, repeat, up to a step limit.

use async_trait::async_trait;

use crate::assistant::{Assistant, run_command_tool};
use crate::context::Context;
use crate::provider::{ChatMessage, tail};
use crate::safety::Verdict;
use crate::{Result, prompts};

/// Cap on tool calls per goal when the user approves each step.
pub const MAX_STEPS: usize = 10;
/// Cap on tool calls per goal in autopilot, where no one approves each step.
pub const AUTOPILOT_MAX_STEPS: usize = 50;
const OUTPUT_LIMIT: usize = 6_000;

/// The user's answer for one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    /// Run it.
    Run,
    /// Do not run it; tell the model.
    Skip,
    /// Stop the agent.
    Abort,
}

/// Result of running one command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit status.
    pub status: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// What happened at a step, reported to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepEvent<'a> {
    /// The agent waits for the model.
    Thinking,
    /// The model wrote text together with a command.
    Said {
        /// What the model wrote.
        text: &'a str,
    },
    /// The guard refused the command.
    Blocked {
        /// Step number, from 1.
        step: usize,
        /// Command.
        command: &'a str,
        /// Verdict text.
        reason: String,
    },
    /// The command ran.
    Ran {
        /// Step number, from 1.
        step: usize,
        /// Command.
        command: &'a str,
        /// Output.
        output: &'a CommandOutput,
    },
}

/// The UI side of the agent loop.
#[async_trait]
pub trait AgentHost: Send {
    /// Asks whether to run `command` at `step`.
    async fn approve(&mut self, step: usize, command: &str, reason: &str) -> Approval;
    /// Runs an approved command.
    async fn run(&mut self, command: &str) -> CommandOutput;
    /// Reports progress.
    fn event(&mut self, event: StepEvent<'_>);
    /// Whether the user stopped the agent during the last command.
    fn stopped(&self) -> bool {
        false
    }
}

/// Why the agent stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The model gave a final answer.
    Done,
    /// The user aborted.
    Aborted,
    /// The step limit was reached.
    StepLimit,
}

/// Agent result.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Why it stopped.
    pub stop: Stop,
    /// The model's final text.
    pub summary: String,
    /// Full conversation, for the transcript.
    pub messages: Vec<ChatMessage>,
    /// Tool calls made.
    pub steps: usize,
}

fn render(out: &CommandOutput) -> String {
    format!(
        "exit status: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        tail(&out.stdout, OUTPUT_LIMIT),
        tail(&out.stderr, OUTPUT_LIMIT / 2)
    )
}

/// Runs the agent loop for `goal`, up to [`MAX_STEPS`] tool calls.
///
/// # Errors
///
/// Fails if the provider fails.
pub async fn run(
    assistant: &Assistant<'_>,
    goal: &str,
    ctx: &Context,
    host: &mut dyn AgentHost,
) -> Result<Outcome> {
    run_limited(assistant, goal, ctx, host, MAX_STEPS).await
}

/// Runs the agent loop for `goal`, up to `max_steps` tool calls.
///
/// Every command passes [`Assistant::check`] (rule guard plus LLM review) and
/// then `host.approve` before it runs.
///
/// # Errors
///
/// Fails if the provider fails.
pub async fn run_limited(
    assistant: &Assistant<'_>,
    goal: &str,
    ctx: &Context,
    host: &mut dyn AgentHost,
    max_steps: usize,
) -> Result<Outcome> {
    let mut req = assistant.request(prompts::AGENT, ctx, vec![ChatMessage::user(goal)]);
    req.tools = vec![run_command_tool()];
    let mut steps = 0;
    loop {
        host.event(StepEvent::Thinking);
        let res = assistant.provider().complete(req.clone()).await?;
        req.messages.push(ChatMessage::assistant(
            res.content.clone(),
            res.tool_calls.clone(),
        ));
        if res.tool_calls.is_empty() {
            return Ok(Outcome {
                stop: Stop::Done,
                summary: res.content,
                messages: req.messages,
                steps,
            });
        }
        if !res.content.trim().is_empty() {
            host.event(StepEvent::Said { text: &res.content });
        }
        for call in &res.tool_calls {
            if steps >= max_steps {
                req.messages.push(ChatMessage::tool(
                    &call.id,
                    "not run: the step limit was reached",
                ));
                continue;
            }
            steps += 1;
            let command = call.arguments["command"].as_str().unwrap_or_default();
            let reason = call.arguments["reason"].as_str().unwrap_or_default();
            let reply = if call.name != "run_command" || command.trim().is_empty() {
                format!("error: unknown tool `{}` or empty command", call.name)
            } else {
                match assistant.check(command, ctx).await {
                    v @ Verdict::Block { .. } => {
                        let text = v.to_string();
                        host.event(StepEvent::Blocked {
                            step: steps,
                            command,
                            reason: text.clone(),
                        });
                        format!("not run: {text}")
                    }
                    Verdict::Allow => match host.approve(steps, command, reason).await {
                        Approval::Run => {
                            let out = host.run(command).await;
                            host.event(StepEvent::Ran {
                                step: steps,
                                command,
                                output: &out,
                            });
                            render(&out)
                        }
                        Approval::Skip => "not run: the user declined this command".to_owned(),
                        Approval::Abort => {
                            req.messages
                                .push(ChatMessage::tool(&call.id, "the user stopped the agent"));
                            return Ok(Outcome {
                                stop: Stop::Aborted,
                                summary: String::new(),
                                messages: req.messages,
                                steps,
                            });
                        }
                    },
                }
            };
            req.messages.push(ChatMessage::tool(&call.id, reply));
            if host.stopped() {
                return Ok(Outcome {
                    stop: Stop::Aborted,
                    summary: String::new(),
                    messages: req.messages,
                    steps,
                });
            }
        }
        if steps >= max_steps {
            return Ok(Outcome {
                stop: Stop::StepLimit,
                summary: format!("stopped after {max_steps} steps"),
                messages: req.messages,
                steps,
            });
        }
    }
}
