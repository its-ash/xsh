//! OpenAI, Anthropic, Ollama, and custom OpenAI-compatible providers, plus a
//! scripted mock for tests.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use url::Url;

use crate::config::{AiConfig, Secret, endpoint_base};
use crate::provider::{AiProvider, AiRequest, AiResponse, ChatMessage, Role, StopReason, ToolCall};
use crate::{Error, Result};

const TIMEOUT: Duration = Duration::from_secs(120);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const ANTHROPIC_VERSION: &str = "2023-06-01";

fn validate_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| Error::Url(raw.to_owned()))?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    );
    match url.scheme() {
        "https" => Ok(url),
        "http" if local => Ok(url),
        _ => Err(Error::Url(raw.to_owned())),
    }
}

fn client(https_only: bool) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(https_only)
        .timeout(TIMEOUT)
        .user_agent(concat!("xsh/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

async fn post(provider: &str, req: reqwest::RequestBuilder, body: &Value) -> Result<Value> {
    read_json(provider, req.json(body).send().await?).await
}

async fn read_json(provider: &str, res: reqwest::Response) -> Result<Value> {
    let status = res.status();
    let text = res.text().await?;
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.pointer("/error/message")
                    .or_else(|| v.get("error"))
                    .map(|m| m.as_str().map_or_else(|| m.to_string(), str::to_owned))
            })
            .unwrap_or_else(|| crate::provider::tail(&text, 300).to_owned());
        return Err(Error::Provider {
            provider: provider.to_owned(),
            status: status.as_u16(),
            message,
        });
    }
    Ok(serde_json::from_str(&text)?)
}

/// Model names from `{data: [...]}`, `{models: [...]}`, or a bare list, where
/// each item is a string or an object with `id`, `name`, or `model`.
fn model_names(v: &Value) -> Vec<String> {
    let list = v.as_array().or_else(|| {
        ["data", "models"]
            .into_iter()
            .find_map(|k| v.get(k)?.as_array())
    });
    let mut names: Vec<String> = list
        .into_iter()
        .flatten()
        .filter_map(|m| {
            m.as_str().or_else(|| {
                ["id", "name", "model"]
                    .into_iter()
                    .find_map(|k| m.get(k)?.as_str())
            })
        })
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Fetches the model names from `<endpoint>/models`.
///
/// # Errors
///
/// Fails on a disallowed URL, a failed request, or a reply with no models.
pub async fn list_models(endpoint: &str, key: Option<&Secret>) -> Result<Vec<String>> {
    let base = endpoint_base(endpoint).ok_or_else(|| Error::Url(endpoint.to_owned()))?;
    let base = validate_url(base.as_str())?;
    let url = base
        .join("models")
        .map_err(|_| Error::Url(endpoint.to_owned()))?;
    let mut req = client(url.scheme() == "https")?
        .get(url)
        .timeout(LIST_TIMEOUT);
    if let Some(k) = key {
        req = req.bearer_auth(k.expose());
    }
    let names = model_names(&read_json("custom", req.send().await?).await?);
    if names.is_empty() {
        return Err(Error::BadResponse("the address lists no models".into()));
    }
    Ok(names)
}

fn split_system(messages: &[ChatMessage]) -> (String, Vec<&ChatMessage>) {
    let system = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let rest = messages.iter().filter(|m| m.role != Role::System).collect();
    (system, rest)
}

fn openai_tools(req: &AiRequest) -> Value {
    Value::Array(
        req.tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {"name": t.name, "description": t.description, "parameters": t.parameters}
                })
            })
            .collect(),
    )
}

/// OpenAI Chat Completions, at OpenAI or at a compatible service.
pub struct OpenAiProvider {
    name: &'static str,
    http: reqwest::Client,
    base: Url,
    key: Option<Secret>,
    model: String,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base", &self.base.as_str())
            .field("model", &self.model)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// Provider for `model` at `base_url` (default `https://api.openai.com/v1/`).
    ///
    /// # Errors
    ///
    /// Fails on a disallowed URL or client setup error.
    pub fn new(key: Secret, model: impl Into<String>, base_url: Option<&str>) -> Result<Self> {
        let base = validate_url(base_url.unwrap_or("https://api.openai.com/v1/"))?;
        Ok(Self {
            name: "openai",
            http: client(base.scheme() == "https")?,
            base,
            key: Some(key),
            model: model.into(),
        })
    }

    /// Provider for `model` at a compatible service. The key is optional
    /// because a local server often needs none.
    ///
    /// # Errors
    ///
    /// Fails on a disallowed URL or client setup error.
    pub fn custom(key: Option<Secret>, model: impl Into<String>, base_url: &str) -> Result<Self> {
        let base = validate_url(base_url)?;
        Ok(Self {
            name: "custom",
            http: client(base.scheme() == "https")?,
            base,
            key,
            model: model.into(),
        })
    }

    /// Request body for `req`.
    #[must_use]
    pub fn body(&self, req: &AiRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| match m.role {
                Role::Tool => json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id,
                    "content": m.content,
                }),
                Role::Assistant if !m.tool_calls.is_empty() => json!({
                    "role": "assistant",
                    "content": m.content,
                    "tool_calls": m.tool_calls.iter().map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": {"name": c.name, "arguments": c.arguments.to_string()},
                    })).collect::<Vec<_>>(),
                }),
                role => json!({"role": role, "content": m.content}),
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
        });
        if !req.tools.is_empty() {
            body["tools"] = openai_tools(req);
        }
        body
    }
}

#[async_trait]
impl AiProvider for OpenAiProvider {
    async fn complete(&self, req: AiRequest) -> Result<AiResponse> {
        let url = self
            .base
            .join("chat/completions")
            .map_err(|_| Error::Url(self.base.to_string()))?;
        let mut http = self.http.post(url);
        if let Some(k) = &self.key {
            http = http.bearer_auth(k.expose());
        }
        let v = post(self.name, http, &self.body(&req)).await?;
        let choice = v
            .pointer("/choices/0")
            .ok_or_else(|| Error::BadResponse("no choices".into()))?;
        let msg = &choice["message"];
        let tool_calls = msg["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c["id"].as_str().unwrap_or_default().to_owned(),
                        name: c["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: c["function"]["arguments"]
                            .as_str()
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(AiResponse {
            content: msg["content"].as_str().unwrap_or_default().to_owned(),
            tool_calls,
            stop: match choice["finish_reason"].as_str() {
                Some("stop") => StopReason::End,
                Some("tool_calls") => StopReason::ToolUse,
                Some("length") => StopReason::Length,
                _ => StopReason::Other,
            },
        })
    }

    fn name(&self) -> &'static str {
        self.name
    }
}

/// Anthropic Messages API.
pub struct AnthropicProvider {
    http: reqwest::Client,
    base: Url,
    key: Secret,
    model: String,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base", &self.base.as_str())
            .field("model", &self.model)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Provider for `model` at `base_url` (default `https://api.anthropic.com/v1/`).
    ///
    /// # Errors
    ///
    /// Fails on a disallowed URL or client setup error.
    pub fn new(key: Secret, model: impl Into<String>, base_url: Option<&str>) -> Result<Self> {
        let base = validate_url(base_url.unwrap_or("https://api.anthropic.com/v1/"))?;
        Ok(Self {
            http: client(base.scheme() == "https")?,
            base,
            key,
            model: model.into(),
        })
    }

    /// Request body for `req`. Consecutive tool results merge into one user turn.
    #[must_use]
    pub fn body(&self, req: &AiRequest) -> Value {
        let (system, rest) = split_system(&req.messages);
        let mut messages: Vec<Value> = Vec::new();
        for m in rest {
            match m.role {
                Role::Tool => {
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id,
                        "content": m.content,
                    });
                    let merged = messages.last_mut().is_some_and(|last| {
                        let is_results = last["role"] == "user"
                            && last["content"]
                                .as_array()
                                .and_then(|a| a.first())
                                .is_some_and(|b| b["type"] == "tool_result");
                        if is_results {
                            if let Some(a) = last["content"].as_array_mut() {
                                a.push(block.clone());
                            }
                        }
                        is_results
                    });
                    if !merged {
                        messages.push(json!({"role": "user", "content": [block]}));
                    }
                }
                Role::Assistant => {
                    let mut blocks = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({"type": "text", "text": m.content}));
                    }
                    blocks.extend(m.tool_calls.iter().map(|c| {
                        json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments})
                    }));
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
                _ => messages.push(json!({"role": "user", "content": m.content})),
            }
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = Value::String(system);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                    .collect(),
            );
        }
        body
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    async fn complete(&self, req: AiRequest) -> Result<AiResponse> {
        let url = self
            .base
            .join("messages")
            .map_err(|_| Error::Url(self.base.to_string()))?;
        let v = post(
            "anthropic",
            self.http
                .post(url)
                .header("x-api-key", self.key.expose())
                .header("anthropic-version", ANTHROPIC_VERSION),
            &self.body(&req),
        )
        .await?;
        let blocks = v["content"]
            .as_array()
            .ok_or_else(|| Error::BadResponse("no content".into()))?;
        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for b in blocks {
            match b["type"].as_str() {
                Some("text") => content.push_str(b["text"].as_str().unwrap_or_default()),
                Some("tool_use") => tool_calls.push(ToolCall {
                    id: b["id"].as_str().unwrap_or_default().to_owned(),
                    name: b["name"].as_str().unwrap_or_default().to_owned(),
                    arguments: b["input"].clone(),
                }),
                _ => {}
            }
        }
        Ok(AiResponse {
            content,
            tool_calls,
            stop: match v["stop_reason"].as_str() {
                Some("end_turn" | "stop_sequence") => StopReason::End,
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::Length,
                _ => StopReason::Other,
            },
        })
    }

    fn name(&self) -> &'static str {
        "anthropic"
    }
}

/// Local Ollama server.
#[derive(Debug)]
pub struct OllamaProvider {
    http: reqwest::Client,
    base: Url,
    model: String,
}

impl OllamaProvider {
    /// Provider for `model` at `base_url` (default `http://localhost:11434/`).
    ///
    /// # Errors
    ///
    /// Fails on a disallowed URL or client setup error.
    pub fn new(model: impl Into<String>, base_url: Option<&str>) -> Result<Self> {
        let base = validate_url(base_url.unwrap_or("http://localhost:11434/"))?;
        Ok(Self {
            http: client(base.scheme() == "https")?,
            base,
            model: model.into(),
        })
    }

    /// Request body for `req`.
    #[must_use]
    pub fn body(&self, req: &AiRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let mut v = json!({"role": m.role, "content": m.content});
                if !m.tool_calls.is_empty() {
                    v["tool_calls"] = m
                        .tool_calls
                        .iter()
                        .map(|c| json!({"function": {"name": c.name, "arguments": c.arguments}}))
                        .collect();
                }
                v
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "options": {"temperature": req.temperature, "num_predict": req.max_tokens},
        });
        if !req.tools.is_empty() {
            body["tools"] = openai_tools(req);
        }
        body
    }
}

#[async_trait]
impl AiProvider for OllamaProvider {
    async fn complete(&self, req: AiRequest) -> Result<AiResponse> {
        let url = self
            .base
            .join("api/chat")
            .map_err(|_| Error::Url(self.base.to_string()))?;
        let v = post("ollama", self.http.post(url), &self.body(&req)).await?;
        let msg = &v["message"];
        let tool_calls: Vec<ToolCall> = msg["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .enumerate()
                    .map(|(i, c)| ToolCall {
                        id: format!("call_{i}"),
                        name: c["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: c["function"]["arguments"].clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(AiResponse {
            content: msg["content"].as_str().unwrap_or_default().to_owned(),
            stop: if tool_calls.is_empty() {
                StopReason::End
            } else {
                StopReason::ToolUse
            },
            tool_calls,
        })
    }

    fn name(&self) -> &'static str {
        "ollama"
    }
}

/// A provider that replays scripted responses and records requests.
#[derive(Debug, Default)]
pub struct MockProvider {
    replies: Mutex<VecDeque<AiResponse>>,
    requests: Mutex<Vec<AiRequest>>,
}

impl MockProvider {
    /// A mock that returns `replies` in order, then empty text.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = AiResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// A text reply.
    #[must_use]
    pub fn text(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_owned(),
            tool_calls: Vec::new(),
            stop: StopReason::End,
        }
    }

    /// A `run_command` tool call.
    #[must_use]
    pub fn run(id: &str, command: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "run_command".to_owned(),
                arguments: json!({"command": command, "reason": "test"}),
            }],
            stop: StopReason::ToolUse,
        }
    }

    /// Requests received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<AiRequest> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl AiProvider for MockProvider {
    async fn complete(&self, req: AiRequest) -> Result<AiResponse> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(req);
        }
        let next = self.replies.lock().ok().and_then(|mut q| q.pop_front());
        Ok(next.unwrap_or_else(|| Self::text("")))
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

/// Builds the provider named by `[default].provider`.
///
/// # Errors
///
/// Fails for unknown providers, missing keys, or bad URLs.
pub fn from_config(cfg: &AiConfig) -> Result<Box<dyn AiProvider>> {
    let name = cfg.default.provider.as_str();
    let p = cfg.provider(name);
    let model = cfg.model(name);
    let key = |default_env: &str| {
        p.api_key.as_ref().map_or_else(
            || {
                std::env::var(default_env)
                    .ok()
                    .filter(|v| !v.is_empty())
                    .map(Secret::new)
                    .ok_or_else(|| Error::NoApiKey(name.to_owned()))
            },
            |spec| spec.resolve(name),
        )
    };
    Ok(match name {
        "openai" => Box::new(OpenAiProvider::new(
            key("OPENAI_API_KEY")?,
            model,
            p.base_url.as_deref(),
        )?),
        "anthropic" => Box::new(AnthropicProvider::new(
            key("ANTHROPIC_API_KEY")?,
            model,
            p.base_url.as_deref(),
        )?),
        "ollama" => Box::new(OllamaProvider::new(model, p.base_url.as_deref())?),
        "custom" => {
            if model.is_empty() {
                return Err(Error::CustomProvider("no model is selected"));
            }
            let base = p
                .chat_base()
                .ok_or(Error::CustomProvider("no valid endpoint is set"))?;
            let key = p.api_key.as_ref().map(|s| s.resolve(name)).transpose()?;
            Box::new(OpenAiProvider::custom(key, model, &base)?)
        }
        other => return Err(Error::UnknownProvider(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_require_tls_except_localhost() {
        assert!(validate_url("https://api.openai.com/v1/").is_ok());
        assert!(validate_url("http://localhost:11434/").is_ok());
        assert!(validate_url("http://example.com/").is_err());
        assert!(validate_url("ftp://localhost/").is_err());
    }

    #[test]
    fn debug_redacts_keys() -> Result<()> {
        let p = OpenAiProvider::new(Secret::new("sk-very-secret"), "m", None)?;
        let a = AnthropicProvider::new(Secret::new("sk-ant-secret"), "m", None)?;
        let dbg = format!("{p:?} {a:?}");
        assert!(!dbg.contains("sk-very") && !dbg.contains("sk-ant"));
        Ok(())
    }
}
