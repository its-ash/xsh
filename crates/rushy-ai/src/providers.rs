//! OpenAI, Anthropic, Ollama, and custom OpenAI-compatible providers, plus a
//! scripted mock for tests.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
const MAX_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

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

/// Validates `raw` and makes sure its path ends in `/`, so that
/// `https://host/v1` joins `chat/completions` to `https://host/v1/chat/completions`
/// instead of replacing the last path segment.
fn normalized_base(raw: &str) -> Result<Url> {
    let mut url = validate_url(raw.trim())?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Tool-call arguments, sent as a JSON string by OpenAI or as an object by
/// Ollama and some compatible servers. Anything else becomes `null`.
fn tool_arguments(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        Value::Object(_) => v.clone(),
        _ => Value::Null,
    }
}

fn client(https_only: bool) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .user_agent(concat!("xsh/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529)
}

fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

async fn send_with_retry<F>(provider: &str, mut build: F) -> Result<Value>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    for attempt in 0..MAX_ATTEMPTS {
        let last = attempt + 1 >= MAX_ATTEMPTS;
        match build().send().await {
            Ok(res) => {
                if is_retryable_status(res.status()) && !last {
                    let delay = retry_after(res.headers())
                        .unwrap_or_else(|| INITIAL_BACKOFF * (1u32 << attempt));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return read_json(provider, res).await;
            }
            Err(err) => {
                if is_retryable_error(&err) && !last {
                    tokio::time::sleep(INITIAL_BACKOFF * (1u32 << attempt)).await;
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    Err(Error::BadResponse("request exhausted retries".into()))
}

async fn read_json(provider: &str, mut res: reqwest::Response) -> Result<Value> {
    let status = res.status();
    if let Some(len) = res.content_length()
        && usize::try_from(len).is_ok_and(|n| n > MAX_RESPONSE_BYTES)
    {
        return Err(Error::BadResponse(format!(
            "response exceeded {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if buf.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(Error::BadResponse(format!(
                "response exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&buf);
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
    let http = client(url.scheme() == "https")?;
    let v = send_with_retry("custom", || {
        let mut req = http.get(url.clone()).timeout(LIST_TIMEOUT);
        if let Some(k) = key {
            req = req.bearer_auth(k.expose());
        }
        req
    })
    .await?;
    let names = model_names(&v);
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
        let base = normalized_base(base_url.unwrap_or("https://api.openai.com/v1/"))?;
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
        let base = normalized_base(base_url)?;
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
        let mut body = if is_reasoning_model(&self.model) {
            json!({
                "model": self.model,
                "messages": messages,
                "max_completion_tokens": req.max_tokens,
            })
        } else {
            json!({
                "model": self.model,
                "messages": messages,
                "temperature": req.temperature,
                "max_tokens": req.max_tokens,
            })
        };
        if !req.tools.is_empty() {
            body["tools"] = openai_tools(req);
        }
        body
    }
}

fn is_reasoning_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    ["o1", "o3", "o4", "gpt-5"].iter().any(|p| {
        lower == *p
            || lower
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
    })
}

#[async_trait]
impl AiProvider for OpenAiProvider {
    async fn complete(&self, req: AiRequest) -> Result<AiResponse> {
        let url = self
            .base
            .join("chat/completions")
            .map_err(|_| Error::Url(self.base.to_string()))?;
        let body = self.body(&req);
        let v = send_with_retry(self.name, || {
            let mut http = self.http.post(url.clone()).json(&body);
            if let Some(k) = &self.key {
                http = http.bearer_auth(k.expose());
            }
            http
        })
        .await?;
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
                        arguments: tool_arguments(&c["function"]["arguments"]),
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
        let base = normalized_base(base_url.unwrap_or("https://api.anthropic.com/v1/"))?;
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
                    if !blocks.is_empty() {
                        messages.push(json!({"role": "assistant", "content": blocks}));
                    }
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
        let body = self.body(&req);
        let v = send_with_retry("anthropic", || {
            self.http
                .post(url.clone())
                .header("x-api-key", self.key.expose())
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
        })
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
    next_call_id: AtomicU64,
}

impl OllamaProvider {
    /// Provider for `model` at `base_url` (default `http://localhost:11434/`).
    ///
    /// # Errors
    ///
    /// Fails on a disallowed URL or client setup error.
    pub fn new(model: impl Into<String>, base_url: Option<&str>) -> Result<Self> {
        let base = normalized_base(base_url.unwrap_or("http://localhost:11434/"))?;
        Ok(Self {
            http: client(base.scheme() == "https")?,
            base,
            model: model.into(),
            next_call_id: AtomicU64::new(0),
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
        let body = self.body(&req);
        let v = send_with_retry("ollama", || self.http.post(url.clone()).json(&body)).await?;
        let msg = &v["message"];
        let tool_calls: Vec<ToolCall> = msg["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .map(|c| {
                        let id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
                        ToolCall {
                            id: format!("call_{id}"),
                            name: c["function"]["name"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            arguments: tool_arguments(&c["function"]["arguments"]),
                        }
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
    fn base_urls_keep_their_path() -> Result<()> {
        for raw in ["https://proxy.example/v1", "https://proxy.example/v1/"] {
            let p = OpenAiProvider::new(Secret::new("k"), "m", Some(raw))?;
            let url = p
                .base
                .join("chat/completions")
                .map_err(|_| Error::Url(raw.into()))?;
            assert_eq!(url.as_str(), "https://proxy.example/v1/chat/completions");
        }
        let a = AnthropicProvider::new(
            Secret::new("k"),
            "m",
            Some("https://gw.example/anthropic/v1"),
        )?;
        assert_eq!(a.base.as_str(), "https://gw.example/anthropic/v1/");
        let o = OllamaProvider::new("m", Some("http://localhost:11434"))?;
        assert_eq!(o.base.as_str(), "http://localhost:11434/");
        Ok(())
    }

    #[test]
    fn tool_arguments_accept_strings_and_objects() {
        assert_eq!(
            tool_arguments(&json!("{\"command\":\"ls\"}")),
            json!({"command": "ls"})
        );
        assert_eq!(
            tool_arguments(&json!({"command": "ls"})),
            json!({"command": "ls"})
        );
        assert_eq!(tool_arguments(&json!("{not json")), Value::Null);
        assert_eq!(tool_arguments(&Value::Null), Value::Null);
        assert_eq!(tool_arguments(&json!("[1]")), json!([1]));
    }

    #[test]
    fn model_names_accept_every_shape() {
        assert_eq!(
            model_names(&json!({"data": [{"id": "b"}, {"id": "a"}, {"id": "a"}]})),
            ["a", "b"]
        );
        assert_eq!(
            model_names(&json!({"models": [{"name": "x"}, {"model": ""}]})),
            ["x"]
        );
        assert_eq!(model_names(&json!(["m", 3])), ["m"]);
        assert!(model_names(&json!({"error": "no"})).is_empty());
    }

    #[test]
    fn debug_redacts_keys() -> Result<()> {
        let p = OpenAiProvider::new(Secret::new("sk-very-secret"), "m", None)?;
        let a = AnthropicProvider::new(Secret::new("sk-ant-secret"), "m", None)?;
        let dbg = format!("{p:?} {a:?}");
        assert!(!dbg.contains("sk-very") && !dbg.contains("sk-ant"));
        Ok(())
    }

    #[test]
    fn openai_reasoning_models_use_max_completion_tokens() -> Result<()> {
        let req = AiRequest {
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            temperature: 0.2,
            max_tokens: 512,
        };
        for model in ["o1", "o3-mini", "o4-mini", "gpt-5", "gpt-5-mini"] {
            let p = OpenAiProvider::new(Secret::new("k"), model, None)?;
            let body = p.body(&req);
            assert_eq!(body["max_completion_tokens"], 512, "{model}");
            assert!(body.get("max_tokens").is_none(), "{model}");
            assert!(body.get("temperature").is_none(), "{model}");
        }
        let p = OpenAiProvider::new(Secret::new("k"), "gpt-4o-mini", None)?;
        let body = p.body(&req);
        assert_eq!(body["max_tokens"], 512);
        assert!(body.get("temperature").is_some());
        Ok(())
    }

    #[test]
    fn anthropic_omits_empty_assistant_messages() -> Result<()> {
        let req = AiRequest {
            messages: vec![
                ChatMessage::user("hi"),
                ChatMessage::assistant("", Vec::new()),
                ChatMessage::user("next"),
            ],
            tools: Vec::new(),
            temperature: 0.2,
            max_tokens: 256,
        };
        let a = AnthropicProvider::new(Secret::new("k"), crate::config::ANTHROPIC_MODEL, None)?;
        let body = a.body(&req);
        let msgs = body["messages"]
            .as_array()
            .ok_or_else(|| Error::BadResponse("missing messages".into()))?;
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| m["role"] == "user"));
        Ok(())
    }
}
