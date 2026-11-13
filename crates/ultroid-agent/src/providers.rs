//! AI provider layer — port of `pyUltroid/agent/_providers.py`.
//!
//! One normalized interface across every AI provider: Ollama (local),
//! OpenAI (Chat Completions + GPT-5/Codex Responses routing), Anthropic,
//! Gemini, Groq, the OpenAI-compatible family (compat, xAI, apimix, Meta
//! Llama, OpenRouter, template) and Cloudflare Workers AI.
//!
//! All wire calls go through `reqwest` directly (no vendor SDKs). A
//! missing API key is `Error::Config` naming the exact database key;
//! transport / provider-side failures are reported on the returned
//! `AIResponse.error`, exactly like the Python `complete()` catch-all.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use regex::Regex;
use serde_json::{Map, Value, json};
use ultroid_core::error::{Error, Result};

use crate::config::{AgentConfig, provider_spec};

// ── Cost tiers ─────────────────────────────────────────────────────────────

/// Routing cost tier, cheapest first. Mirrors `CostTier` in `_providers.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostTier {
    Free = 0,
    Cheap = 1,
    Medium = 2,
    Premium = 3,
}

// ── Tool calls ─────────────────────────────────────────────────────────────

/// One normalized function/tool call, shared by messages and responses.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// Parse a tool-call `arguments` value the way `_tool_arguments` does: a
/// JSON string is decoded (falling back to the raw string), anything falsy
/// becomes an empty object.
pub fn tool_arguments(value: &Value) -> Value {
    match value {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
        Value::Null | Value::Bool(false) => json!({}),
        other => other.clone(),
    }
}

/// Render tool-call arguments as JSON text, as OpenAI Chat Completions
/// requires (`_tool_arguments_json`).
pub fn tool_arguments_json(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "{}".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// Normalise raw OpenAI-style tool calls into [`ToolCall`]s
/// (`_normalise_openai_tool_calls`).
pub fn normalise_openai_tool_calls(tool_calls: &Value) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let items = match tool_calls {
        Value::Array(items) => items.clone(),
        _ => return calls,
    };
    for call in &items {
        let function = call.get("function").cloned().unwrap_or(Value::Null);
        let id = call
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = function
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| call.get("name").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let arguments = function
            .get("arguments")
            .or_else(|| call.get("arguments"))
            .cloned()
            .unwrap_or(Value::Null);
        calls.push(ToolCall {
            id,
            name,
            arguments: tool_arguments(&arguments),
        });
    }
    calls
}

/// Join text parts the way `_join_text_parts` does: plain strings plus the
/// `text` field of part objects, concatenated.
pub fn join_text_parts(parts: &Value) -> String {
    let mut text = String::new();
    if let Value::Array(items) = parts {
        for part in items {
            match part {
                Value::String(s) => text.push_str(s),
                obj => {
                    if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                        text.push_str(t);
                    }
                }
            }
        }
    }
    text
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn metadata_origin(metadata: &HashMap<String, Value>) -> Option<String> {
    metadata
        .get("parts_provider")
        .or_else(|| metadata.get("native_provider"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Native provider parts replay only to the provider they came from
/// (`_native_parts`): the message's `parts` are returned only when its
/// `parts_provider` / `native_provider` metadata tag equals `provider`.
pub fn native_parts(message: &AIMessage, provider: &str) -> Vec<Value> {
    match metadata_origin(&message.metadata) {
        Some(origin) if origin == provider => message.parts.clone(),
        _ => Vec::new(),
    }
}

// ── AIMessage ──────────────────────────────────────────────────────────────

/// One normalized chat message. `content` stays the familiar text field;
/// native provider parts are retained only with an explicit origin tag so
/// opaque blocks from one wire protocol never leak into another.
#[derive(Debug, Clone)]
pub struct AIMessage {
    pub role: String,
    pub content: Value,
    pub name: Option<String>,
    pub parts: Vec<Value>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub tool_result: Option<Value>,
    pub reasoning: Option<String>,
    pub thinking: Option<String>,
    pub modalities: Vec<String>,
    pub metadata: HashMap<String, Value>,
}

impl AIMessage {
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        AIMessage {
            role: role.into(),
            content: Value::String(text.into()),
            name: None,
            parts: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_result: None,
            reasoning: None,
            thinking: None,
            modalities: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Build a message whose content is a list of native parts (mirrors the
    /// `__post_init__` rule: a list `content` becomes `parts`, `content`
    /// becomes the empty string).
    pub fn from_parts(role: impl Into<String>, parts: Vec<Value>) -> Self {
        let mut msg = AIMessage::new(role, "");
        msg.parts = parts;
        msg
    }

    /// The message text. Mirrors `AIMessage.text()`.
    pub fn text(&self) -> String {
        value_to_string(&self.content)
    }

    /// Chat Completions shape. Uses the exact `type=function` envelope for
    /// assistant tool calls; Responses output items are handled only by the
    /// Responses input builder.
    pub fn to_openai(&self) -> Value {
        let native = native_parts(self, "openai_chat");
        let content = if native.is_empty() {
            Value::String(self.text())
        } else {
            Value::Array(native)
        };
        let mut d = Map::new();
        d.insert("role".to_string(), Value::String(self.role.clone()));
        d.insert("content".to_string(), content);
        if let Some(name) = &self.name {
            d.insert("name".to_string(), Value::String(name.clone()));
        }
        if let Some(id) = &self.tool_call_id {
            d.insert("tool_call_id".to_string(), Value::String(id.clone()));
        }
        if self.role == "assistant" && !self.tool_calls.is_empty() {
            let calls: Vec<Value> = self
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": tool_arguments_json(&call.arguments),
                        },
                    })
                })
                .collect();
            d.insert("tool_calls".to_string(), Value::Array(calls));
        }
        Value::Object(d)
    }

    /// Anthropic Messages shape: tool results become a `tool_result` block
    /// on a user turn, assistant turns replay native Anthropic blocks or a
    /// text block plus `tool_use` blocks.
    pub fn to_anthropic(&self) -> Value {
        if self.role == "tool" {
            let result = match &self.tool_result {
                Some(v) => v.clone(),
                None => self.content.clone(),
            };
            let content_val = match &result {
                Value::String(_) | Value::Array(_) => result,
                other => Value::String(value_to_string(other)),
            };
            return json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": self.tool_call_id.clone().or_else(|| self.name.clone()).unwrap_or_default(),
                    "content": content_val,
                }],
            });
        }
        if self.role == "assistant" {
            let blocks = native_parts(self, "anthropic");
            let blocks = if blocks.is_empty() {
                let mut b: Vec<Value> = Vec::new();
                if !self.text().is_empty() {
                    b.push(json!({"type": "text", "text": self.text()}));
                }
                for call in &self.tool_calls {
                    b.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": tool_arguments(&call.arguments),
                    }));
                }
                b
            } else {
                blocks
            };
            let content = if blocks.is_empty() {
                Value::String(String::new())
            } else {
                Value::Array(blocks)
            };
            return json!({"role": "assistant", "content": content});
        }
        let origin = metadata_origin(&self.metadata);
        let native = native_parts(self, "anthropic");
        let content = if !native.is_empty() {
            Value::Array(native)
        } else if !self.parts.is_empty() && origin.is_none() {
            Value::Array(self.parts.clone())
        } else {
            Value::String(self.text())
        };
        json!({"role": self.role, "content": content})
    }

    /// Gemini `generateContent` shape: assistant turns become `model`,
    /// tool turns become `function_response` parts, and native Gemini
    /// `function_call` parts are kept with tool calls appended (deduped by
    /// name + arguments, so a native call is never sent twice).
    pub fn to_gemini(&self) -> Value {
        let role = if self.role == "assistant" {
            "model"
        } else {
            "user"
        };
        if self.role == "tool" {
            let response = match &self.tool_result {
                Some(v) => v.clone(),
                None => self.content.clone(),
            };
            return json!({
                "role": "user",
                "parts": [{
                    "function_response": {
                        "name": self.name.clone().unwrap_or_else(|| "tool".to_string()),
                        "response": {"result": response},
                    },
                }],
            });
        }
        // Untagged user parts are caller-authored Gemini content. Provider
        // responses must be tagged and are replayed only to Gemini.
        let origin = metadata_origin(&self.metadata);
        let native = native_parts(self, "gemini");
        let mut parts = if !native.is_empty() {
            native
        } else if self.role != "assistant" && origin.is_none() {
            self.parts.clone()
        } else {
            Vec::new()
        };
        if parts.is_empty() && !self.text().is_empty() {
            parts.push(json!({"text": self.text()}));
        }
        let mut native_calls: Vec<(String, String)> = Vec::new();
        for part in &parts {
            if let Some(call) = part.get("function_call") {
                let name = call
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = tool_arguments(call.get("args").unwrap_or(&Value::Null));
                native_calls.push((name, serde_json::to_string(&args).unwrap_or_default()));
            }
        }
        for call in &self.tool_calls {
            let args = tool_arguments(&call.arguments);
            let identity = (
                call.name.clone(),
                serde_json::to_string(&args).unwrap_or_default(),
            );
            if !native_calls.contains(&identity) {
                parts.push(json!({
                    "function_call": {"name": call.name, "args": args},
                }));
            }
        }
        if parts.is_empty() {
            parts.push(json!({"text": ""}));
        }
        json!({"role": role, "parts": parts})
    }
}

// ── AIResponse ─────────────────────────────────────────────────────────────

/// One normalized provider answer.
#[derive(Debug, Clone)]
pub struct AIResponse {
    pub content: String,
    pub provider: String,
    pub model: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub duration_ms: f64,
    pub error: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub image: Option<String>,
    pub reasoning: Option<String>,
    pub thinking: Option<String>,
    pub parts: Vec<Value>,
    pub modalities: Vec<String>,
    pub metadata: HashMap<String, Value>,
}

impl AIResponse {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        AIResponse {
            content: String::new(),
            provider: provider.into(),
            model: model.into(),
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0.0,
            error: None,
            tool_calls: Vec::new(),
            image: None,
            reasoning: None,
            thinking: None,
            parts: Vec::new(),
            modalities: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_error(
        provider: impl Into<String>,
        model: impl Into<String>,
        error: impl Into<String>,
        started: Instant,
    ) -> Self {
        let mut r = AIResponse::new(provider, model);
        r.error = Some(error.into());
        r.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        r
    }

    /// True when the response carries an answer: no error, and content, a
    /// tool call or an image is present. Opaque parts alone are not an
    /// answer.
    pub fn ok(&self) -> bool {
        self.error.is_none()
            && (!self.content.is_empty() || !self.tool_calls.is_empty() || self.image.is_some())
    }
}

impl std::fmt::Display for AIResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.content)
    }
}

// ── Options / capabilities / trait ─────────────────────────────────────────

/// Per-call generation controls, mirroring `BaseProvider.complete()`'s
/// keyword arguments.
#[derive(Debug, Clone)]
pub struct CompleteOptions {
    pub model: Option<String>,
    pub tools: Option<Vec<Value>>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub system: Option<String>,
}

impl Default for CompleteOptions {
    fn default() -> Self {
        CompleteOptions {
            model: None,
            tools: None,
            max_tokens: 1024,
            temperature: 0.7,
            system: None,
        }
    }
}

/// Supported generation parameters for a model, mirroring
/// `BaseProvider.model_capabilities()`. `max_tokens` is the native
/// parameter name for the output-token cap, or `None` when the model
/// accepts no such cap (e.g. image-generation models).
#[derive(Debug, Clone, Copy)]
pub struct ModelCapabilities {
    pub max_tokens: Option<&'static str>,
    pub temperature: bool,
    pub tools: bool,
    pub system: bool,
    pub stream: bool,
}

impl ModelCapabilities {
    /// The common OpenAI-style defaults the base provider returns.
    pub fn base() -> Self {
        ModelCapabilities {
            max_tokens: Some("max_tokens"),
            temperature: true,
            tools: true,
            system: true,
            stream: true,
        }
    }
}

/// The provider contract. `complete` returns an `AIResponse` carrying the
/// error (like the Python catch-all) for transport / provider failures;
/// only a missing credential / endpoint is `Err(Error::Config)`.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn cost_tier(&self) -> CostTier;
    fn default_model(&self) -> String;
    fn is_configured(&self) -> bool;
    fn model_capabilities(&self, model: Option<&str>) -> ModelCapabilities;
    async fn verify(&self) -> bool;
    async fn list_models(&self) -> Result<Vec<String>>;
    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse>;
    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>>;
}

// ── Shared helpers ─────────────────────────────────────────────────────────

/// First database key holding *field* for *provider*, for error messages
/// that name the exact key (`config.rs` `PROVIDERS` is the source).
fn first_db_key(provider: &str, field: &str) -> Option<&'static str> {
    provider_spec(provider)
        .ok()?
        .keys
        .iter()
        .find(|(f, _)| *f == field)
        .and_then(|(_, keys)| keys.first().copied())
}

/// Read one config field synchronously. `AgentConfig::config_field` is
/// async, but `is_configured` / `default_model` on the trait are sync, so
/// the read runs on a fresh OS thread with its own current-thread runtime.
/// Falls back to `default` when there is no database value (or the read
/// itself fails); an explicit in-memory override is checked by callers
/// before this is reached.
fn read_field_sync(
    config: &AgentConfig,
    provider: &str,
    field: &str,
    default: Option<String>,
) -> Option<String> {
    let cfg = config.clone();
    let prov = provider.to_string();
    let fld = field.to_string();
    let joined = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async move { cfg.config_field(&prov, &fld, None).await.ok().flatten() })
    })
    .join();
    match joined {
        Ok(Some(v)) if !v.is_empty() => Some(v),
        _ => default,
    }
}

/// Map the logical `max_tokens` / `temperature` controls onto a provider's
/// native completion parameters (`BaseProvider._completion_params`).
/// Temperature is omitted entirely for models that reject it, and the token
/// cap is sent under whatever native name the model expects.
fn completion_params(
    caps: &ModelCapabilities,
    max_tokens: u32,
    temperature: f32,
) -> Map<String, Value> {
    let mut params = Map::new();
    if let Some(name) = caps.max_tokens {
        params.insert(name.to_string(), json!(max_tokens));
    }
    if caps.temperature {
        params.insert("temperature".to_string(), json!(temperature));
    }
    params
}

/// Human-readable message for a non-2xx HTTP response, preferring the
/// provider's own `error.message` / `message` fields.
fn http_error_message(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(err) = v.get("error") {
            if let Some(m) = err.get("message").and_then(|m| m.as_str()) {
                return format!("{status}: {m}");
            }
            if let Some(s) = err.as_str() {
                return format!("{status}: {s}");
            }
        }
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            return format!("{status}: {m}");
        }
    }
    let snippet: String = body.chars().take(300).collect();
    format!("{status}: {snippet}")
}

/// Shared OpenAI-compatible chat-completions call (used by the OpenAI
/// family and Groq). Returns the parsed `AIResponse`; HTTP errors and
/// transport failures become an error response, never `Err`.
#[allow(clippy::too_many_arguments)]
async fn openai_chat_complete(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    provider_name: &'static str,
    model: &str,
    messages: &[AIMessage],
    opts: &CompleteOptions,
    caps: &ModelCapabilities,
    started: Instant,
) -> AIResponse {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(system) = &opts.system {
        msgs.push(json!({"role": "system", "content": system}));
    }
    msgs.extend(messages.iter().map(|m| m.to_openai()));
    let mut body = completion_params(caps, opts.max_tokens, opts.temperature);
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert("messages".to_string(), Value::Array(msgs));
    if let Some(tools) = &opts.tools {
        if !tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(tools.clone()));
            body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }
    }
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let resp = match client
        .post(&url)
        .bearer_auth(api_key)
        .json(&Value::Object(body))
        .timeout(Duration::from_secs(120))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return AIResponse::with_error(provider_name, model, e.to_string(), started),
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return AIResponse::with_error(provider_name, model, e.to_string(), started),
    };
    if !status.is_success() {
        return AIResponse::with_error(
            provider_name,
            model,
            http_error_message(status, &text),
            started,
        );
    }
    let data: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return AIResponse::with_error(provider_name, model, e.to_string(), started),
    };
    parse_chat_response(&data, provider_name, model, started)
}

/// Parse an OpenAI-style chat-completions response body into an
/// `AIResponse`, including tool calls, usage and reasoning passthrough.
fn parse_chat_response(
    data: &Value,
    provider_name: &str,
    model: &str,
    started: Instant,
) -> AIResponse {
    let mut resp = AIResponse::new(provider_name, model);
    resp.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let choice = data.get("choices").and_then(|c| c.get(0));
    let message = choice.and_then(|c| c.get("message"));
    let usage = data.get("usage");
    resp.tokens_in = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    resp.tokens_out = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    if let Some(msg) = message {
        match msg.get("content") {
            Some(Value::String(s)) => resp.content = s.clone(),
            Some(Value::Array(_)) => {
                resp.content = join_text_parts(msg.get("content").unwrap());
                if let Value::Array(parts) = msg.get("content").unwrap().clone() {
                    resp.parts = parts;
                }
            }
            _ => {}
        }
        if let Some(tcs) = msg.get("tool_calls") {
            resp.tool_calls = normalise_openai_tool_calls(tcs);
        }
        let reasoning = msg
            .get("reasoning_content")
            .or_else(|| msg.get("reasoning"))
            .or_else(|| msg.get("thinking"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        resp.reasoning = reasoning;
        if !resp.content.is_empty() {
            resp.modalities = vec!["text".to_string()];
        }
    }
    resp.metadata.insert(
        "parts_provider".to_string(),
        Value::String("openai_chat".to_string()),
    );
    resp
}

/// A one-chunk stream carrying a single message (not-configured notices,
/// request errors that the Python generators yield as text).
fn single_chunk_stream(message: String) -> BoxStream<'static, Result<String>> {
    futures::stream::iter(vec![Ok(message)]).boxed()
}

/// Server-sent-events stream: buffers `data:` events split across chunks,
/// stops on `data: [DONE]`, and yields the text `extract` pulls from each
/// event's JSON payload.
fn sse_stream(
    resp: reqwest::Response,
    extract: Box<dyn Fn(&Value) -> Option<String> + Send + Sync>,
    error_label: &'static str,
) -> BoxStream<'static, Result<String>> {
    futures::stream::unfold(
        (resp, String::new(), false, extract),
        move |(mut resp, mut buf, mut done, extract)| async move {
            loop {
                if done {
                    if buf.trim().is_empty() {
                        return None;
                    }
                    let event = std::mem::take(&mut buf);
                    for line in event.lines() {
                        let line = line.trim_end_matches('\r');
                        if let Some(data) = line.strip_prefix("data:") {
                            let data = data.trim();
                            if data.is_empty() || data == "[DONE]" {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                if let Some(text) = extract(&v) {
                                    if !text.is_empty() {
                                        return Some((Ok(text), (resp, buf, true, extract)));
                                    }
                                }
                            }
                        }
                    }
                    return None;
                }
                if let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos + 2).collect();
                    let mut yielded: Option<String> = None;
                    let mut terminated = false;
                    for line in event.lines() {
                        let line = line.trim_end_matches('\r');
                        if let Some(data) = line.strip_prefix("data:") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                terminated = true;
                                break;
                            }
                            if data.is_empty() {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                if let Some(text) = extract(&v) {
                                    if !text.is_empty() {
                                        yielded = Some(text);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if terminated {
                        return None;
                    }
                    if let Some(text) = yielded {
                        return Some((Ok(text), (resp, buf, done, extract)));
                    }
                    continue;
                }
                match resp.chunk().await {
                    Ok(Some(bytes)) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    Ok(None) => {
                        done = true;
                    }
                    Err(e) => {
                        return Some((
                            Ok(format!("[{error_label} stream error: {e}]")),
                            (resp, buf, true, extract),
                        ));
                    }
                }
            }
        },
    )
    .boxed()
}

/// Newline-delimited JSON stream (Ollama `/api/chat`, `/api/pull`).
fn ndjson_stream(
    resp: reqwest::Response,
    extract: Box<dyn Fn(&Value) -> (Option<String>, bool) + Send + Sync>,
    error_label: &'static str,
) -> BoxStream<'static, Result<String>> {
    futures::stream::unfold(
        (resp, String::new(), false, extract),
        move |(mut resp, mut buf, mut done, extract)| async move {
            loop {
                if done {
                    if buf.trim().is_empty() {
                        return None;
                    }
                    let line = std::mem::take(&mut buf);
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                        let (text, _) = extract(&v);
                        if let Some(text) = text {
                            if !text.is_empty() {
                                return Some((Ok(text), (resp, buf, true, extract)));
                            }
                        }
                    }
                    return None;
                }
                if let Some(pos) = buf.find('\n') {
                    let line: String = buf.drain(..=pos).collect();
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        let (text, finished) = extract(&v);
                        match text {
                            Some(t) if !t.is_empty() => {
                                let state = (resp, buf, finished, extract);
                                return Some((Ok(t), state));
                            }
                            _ if finished => return None,
                            // No text in this chunk; keep draining.
                            _ => continue,
                        }
                    }
                    continue;
                }
                match resp.chunk().await {
                    Ok(Some(bytes)) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    Ok(None) => {
                        done = true;
                    }
                    Err(e) => {
                        return Some((
                            Ok(format!("[{error_label} stream error: {e}]")),
                            (resp, buf, true, extract),
                        ));
                    }
                }
            }
        },
    )
    .boxed()
}

// ── Ollama (local LLM) ─────────────────────────────────────────────────────

/// Local Ollama. Always "configured" (the Python provider returns `True`
/// before any model is set up); reachability is checked per call via the
/// tags list.
pub struct OllamaProvider {
    config: AgentConfig,
    client: reqwest::Client,
    explicit_base: Option<String>,
    explicit_model: Option<String>,
}

impl OllamaProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        OllamaProvider {
            config,
            client,
            explicit_base: None,
            explicit_model: None,
        }
    }

    /// An explicit override from a one-off caller stays in memory; the
    /// persistent configuration only changes through the menu.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.explicit_model = Some(model.into());
        self
    }

    fn default_model_sync(&self) -> String {
        if let Some(m) = &self.explicit_model {
            return m.clone();
        }
        // Database first: the in-process override applies only while the
        // database holds no model.
        read_field_sync(&self.config, "ollama", "MODEL", None)
            .unwrap_or_else(|| "llama3".to_string())
    }

    async fn base_url_async(&self) -> String {
        if let Some(b) = &self.explicit_base {
            return b.clone();
        }
        self.config
            .config_field("ollama", "BASE", None)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "http://localhost:11434".to_string())
    }


    /// Listing local tags is a read-only reachability check.
    async fn is_running(&self) -> bool {
        match self
            .client
            .get(format!(
                "{}/api/tags",
                self.base_url_async().await.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) => r.status() == reqwest::StatusCode::OK,
            Err(_) => false,
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Free
    }

    fn default_model(&self) -> String {
        self.default_model_sync()
    }

    fn is_configured(&self) -> bool {
        true
    }

    fn model_capabilities(&self, _model: Option<&str>) -> ModelCapabilities {
        // Ollama exposes the token cap as `num_predict` inside the
        // `options` object, and drops the tool schema, so tool-calling
        // requests skip this tier entirely.
        ModelCapabilities {
            max_tokens: Some("num_predict"),
            temperature: true,
            tools: false,
            system: true,
            stream: true,
        }
    }

    async fn verify(&self) -> bool {
        self.is_running().await
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let resp = match self
            .client
            .get(format!(
                "{}/api/tags",
                self.base_url_async().await.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(data
            .get("models")
            .and_then(|m| m.as_array())
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| {
                        m.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let base = self.base_url_async().await;
        if !self.is_running().await {
            return Ok(AIResponse::with_error(
                "ollama",
                &model,
                "Ollama not running",
                started,
            ));
        }
        let mut payload = json!({
            "model": model,
            "messages": messages.iter().map(|m| m.to_openai()).collect::<Vec<_>>(),
            "stream": false,
            "options": {
                "temperature": opts.temperature,
                "num_predict": opts.max_tokens,
            },
        });
        if let Some(system) = &opts.system {
            payload["system"] = Value::String(system.clone());
        }
        let resp = match self
            .client
            .post(format!("{}/api/chat", base.trim_end_matches('/')))
            .json(&payload)
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "ollama",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "ollama",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        if !status.is_success() {
            return Ok(AIResponse::with_error(
                "ollama",
                &model,
                http_error_message(status, &text),
                started,
            ));
        }
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "ollama",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        let mut out = AIResponse::new("ollama", &model);
        out.content = data
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        out.tokens_in = data
            .get("prompt_eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.tokens_out = data.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(out)
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let base = self.base_url_async().await;
        if !self.is_running().await {
            return Ok(single_chunk_stream("[Ollama not running]".to_string()));
        }
        let mut payload = json!({
            "model": model,
            "messages": messages.iter().map(|m| m.to_openai()).collect::<Vec<_>>(),
            "stream": true,
            "options": {
                "temperature": opts.temperature,
                "num_predict": opts.max_tokens,
            },
        });
        if let Some(system) = &opts.system {
            payload["system"] = Value::String(system.clone());
        }
        let resp = match self
            .client
            .post(format!("{}/api/chat", base.trim_end_matches('/')))
            .json(&payload)
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(single_chunk_stream(format!("[Ollama stream error: {e}]"))),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[Ollama stream error: {}]",
                http_error_message(status, &body)
            )));
        }
        Ok(ndjson_stream(
            resp,
            Box::new(|v: &Value| {
                let token = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
                let done = v.get("done").and_then(|d| d.as_bool()).unwrap_or(false);
                (token, done)
            }),
            "Ollama",
        ))
    }
}

impl OllamaProvider {
    /// Pull a model from the Ollama registry — streams progress statuses
    /// (`pull_model` in the Python provider).
    pub async fn pull_model(&self, model: &str) -> BoxStream<'static, Result<String>> {
        let base = self.base_url_async().await;
        let resp = match self
            .client
            .post(format!("{}/api/pull", base.trim_end_matches('/')))
            .json(&json!({"name": model, "stream": true}))
            .timeout(Duration::from_secs(600))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return single_chunk_stream(format!("Pull error: {e}")),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return single_chunk_stream(format!(
                "Pull error: {}",
                http_error_message(status, &body)
            ));
        }
        ndjson_stream(
            resp,
            Box::new(|v: &Value| {
                let status = v
                    .get("status")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                (status, false)
            }),
            "Ollama",
        )
    }
}

// ── Anthropic ──────────────────────────────────────────────────────────────

/// Anthropic Messages API (direct REST; the Python provider uses the
/// `anthropic` SDK against the same endpoints).
pub struct AnthropicProvider {
    config: AgentConfig,
    client: reqwest::Client,
    explicit_key: Option<String>,
    explicit_model: Option<String>,
}

impl AnthropicProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        AnthropicProvider {
            config,
            client,
            explicit_key: None,
            explicit_model: None,
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.explicit_key = Some(key.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.explicit_model = Some(model.into());
        self
    }

    fn api_key_sync(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        read_field_sync(&self.config, "anthropic", "KEY", None)
    }

    fn default_model_sync(&self) -> String {
        if let Some(m) = &self.explicit_model {
            return m.clone();
        }
        read_field_sync(&self.config, "anthropic", "MODEL", None)
            .unwrap_or_else(|| "claude-haiku-4-5-20251001".to_string())
    }

    async fn api_key_async(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        self.config
            .config_field("anthropic", "KEY", None)
            .await
            .ok()
            .flatten()
    }


    fn missing_key_error(&self) -> Error {
        let key = first_db_key("anthropic", "KEY").unwrap_or("AGENT_CLAUDE_KEY");
        Error::Config(format!(
            "ANTHROPIC_API_KEY not set; set {key} (legacy env ANTHROPIC_API_KEY)"
        ))
    }

    fn system_prompt<'a>(
        &self,
        messages: &'a [AIMessage],
        opts: &'a CompleteOptions,
    ) -> Option<String> {
        if let Some(s) = &opts.system {
            return Some(s.clone());
        }
        messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.text())
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Premium
    }

    fn default_model(&self) -> String {
        self.default_model_sync()
    }

    fn is_configured(&self) -> bool {
        self.api_key_sync().is_some_and(|k| !k.is_empty())
    }

    fn model_capabilities(&self, _model: Option<&str>) -> ModelCapabilities {
        // Anthropic uses `max_tokens` (required) and supports temperature,
        // tool_use and system prompts across the Claude family.
        ModelCapabilities {
            max_tokens: Some("max_tokens"),
            temperature: true,
            tools: true,
            system: true,
            stream: true,
        }
    }

    async fn verify(&self) -> bool {
        let Some(key) = self.api_key_async().await else {
            return false;
        };
        // The models listing endpoint validates credentials without usage.
        match self
            .client
            .get("https://api.anthropic.com/v1/models?limit=1")
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        // The Python provider defines no listing; the base returns [].
        Ok(Vec::new())
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Err(self.missing_key_error());
        };
        let caps = self.model_capabilities(Some(&model));
        let mut body = completion_params(&caps, opts.max_tokens, opts.temperature);
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert(
            "messages".to_string(),
            Value::Array(
                messages
                    .iter()
                    .filter(|m| m.role != "system")
                    .map(|m| m.to_anthropic())
                    .collect(),
            ),
        );
        if let Some(system) = self.system_prompt(messages, opts) {
            body.insert("system".to_string(), Value::String(system));
        }
        if let Some(tools) = &opts.tools {
            if !tools.is_empty() {
                body.insert("tools".to_string(), Value::Array(tools.clone()));
            }
        }
        let resp = match self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .json(&Value::Object(body))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "anthropic",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "anthropic",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        if !status.is_success() {
            return Ok(AIResponse::with_error(
                "anthropic",
                &model,
                http_error_message(status, &text),
                started,
            ));
        }
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "anthropic",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        let mut out = AIResponse::new("anthropic", &model);
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        let blocks: Vec<Value> = data
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let mut thinking = String::new();
        for block in &blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        out.content.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                        thinking.push_str(t);
                    }
                }
                Some("tool_use") => {
                    out.tool_calls.push(ToolCall {
                        id: block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: block.get("input").cloned().unwrap_or(json!({})),
                    });
                }
                _ => {}
            }
        }
        if !thinking.is_empty() {
            out.thinking = Some(thinking.clone());
            out.reasoning = Some(thinking);
        }
        out.tokens_in = data
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.tokens_out = data
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.parts = blocks;
        if !out.content.is_empty() {
            out.modalities = vec!["text".to_string()];
        }
        out.metadata.insert(
            "parts_provider".to_string(),
            Value::String("anthropic".to_string()),
        );
        Ok(out)
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Ok(single_chunk_stream(
                "[Anthropic not configured]".to_string(),
            ));
        };
        let caps = self.model_capabilities(Some(&model));
        let mut body = completion_params(&caps, opts.max_tokens, opts.temperature);
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert(
            "messages".to_string(),
            Value::Array(
                messages
                    .iter()
                    .filter(|m| m.role != "system")
                    .map(|m| m.to_anthropic())
                    .collect(),
            ),
        );
        body.insert("stream".to_string(), Value::Bool(true));
        if let Some(system) = self.system_prompt(messages, opts) {
            body.insert("system".to_string(), Value::String(system));
        }
        let resp = match self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .json(&Value::Object(body))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(single_chunk_stream(format!(
                    "[Anthropic stream error: {e}]"
                )));
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[Anthropic stream error: {}]",
                http_error_message(status, &text)
            )));
        }
        Ok(sse_stream(
            resp,
            Box::new(|v: &Value| {
                if v.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                    return v
                        .get("delta")
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                }
                None
            }),
            "Anthropic",
        ))
    }
}

// ── Gemini ─────────────────────────────────────────────────────────────────

/// Google Gemini `generateContent` REST API (the Python provider uses the
/// `google-genai` SDK against the same endpoints).
pub struct GeminiProvider {
    config: AgentConfig,
    client: reqwest::Client,
    explicit_key: Option<String>,
    explicit_model: Option<String>,
}

impl GeminiProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        GeminiProvider {
            config,
            client,
            explicit_key: None,
            explicit_model: None,
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.explicit_key = Some(key.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.explicit_model = Some(model.into());
        self
    }

    fn api_key_sync(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        read_field_sync(&self.config, "gemini", "KEY", None)
    }

    fn default_model_sync(&self) -> String {
        if let Some(m) = &self.explicit_model {
            return m.clone();
        }
        read_field_sync(&self.config, "gemini", "MODEL", None)
            .unwrap_or_else(|| "gemini-2.5-flash".to_string())
    }

    async fn api_key_async(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        self.config
            .config_field("gemini", "KEY", None)
            .await
            .ok()
            .flatten()
    }


    fn missing_key_error(&self) -> Error {
        let key = first_db_key("gemini", "KEY").unwrap_or("AGENT_CLOUD_GEMINI_KEY");
        Error::Config(format!(
            "GEMINI_API_KEY not set; set {key} (legacy env GEMINI_API_KEY or GOOGLE_API_KEY)"
        ))
    }

    /// Request contents: system turns are excluded (they become the
    /// `system_instruction`); an empty history still needs one user part.
    fn contents(messages: &[AIMessage]) -> Vec<Value> {
        let contents: Vec<Value> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| m.to_gemini())
            .collect();
        if contents.is_empty() {
            vec![json!({"role": "user", "parts": [{"text": ""}]})]
        } else {
            contents
        }
    }

    /// Accept native Gemini declarations and OpenAI function schemas
    /// (`GeminiProvider._tools`).
    fn tools(tools: &Option<Vec<Value>>) -> Vec<Value> {
        let Some(tools) = tools else {
            return Vec::new();
        };
        if tools
            .iter()
            .any(|t| t.get("function_declarations").is_some())
        {
            return tools.clone();
        }
        let mut declarations: Vec<Value> = Vec::new();
        for tool in tools {
            let function = tool.get("function").unwrap_or(tool);
            if let Some(name) = function.get("name").and_then(|n| n.as_str()) {
                let mut decl = Map::new();
                decl.insert("name".to_string(), Value::String(name.to_string()));
                for key in ["description", "parameters"] {
                    if let Some(v) = function.get(key) {
                        decl.insert(key.to_string(), v.clone());
                    }
                }
                declarations.push(Value::Object(decl));
            }
        }
        if declarations.is_empty() {
            Vec::new()
        } else {
            vec![json!({"function_declarations": declarations})]
        }
    }

    fn request_body(&self, messages: &[AIMessage], opts: &CompleteOptions, model: &str) -> Value {
        let caps = self.model_capabilities(Some(model));
        let gen_config = completion_params(&caps, opts.max_tokens, opts.temperature);
        let system = opts.system.clone().or_else(|| {
            messages
                .iter()
                .find(|m| m.role == "system")
                .map(|m| m.text())
        });
        let mut body = json!({
            "contents": Self::contents(messages),
            "generationConfig": gen_config,
        });
        if let Some(system) = system {
            body["systemInstruction"] = json!({"parts": [{"text": system}]});
        }
        let tools = Self::tools(&opts.tools);
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        body
    }

    fn parse_generate_content(data: &Value, model: &str, started: Instant) -> AIResponse {
        let mut out = AIResponse::new("gemini", model);
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        let candidates = data.get("candidates").and_then(|c| c.as_array());
        let parts: Vec<Value> = candidates
            .and_then(|c| c.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        out.content = join_text_parts(&Value::Array(parts.clone()));
        let mut thinking = String::new();
        for part in &parts {
            if part
                .get("thought")
                .and_then(|t| t.as_bool())
                .unwrap_or(false)
            {
                // `thought` parts carry their text alongside the flag.
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    thinking.push_str(t);
                }
            }
            if let Some(call) = part.get("functionCall") {
                out.tool_calls.push(ToolCall {
                    id: call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .or_else(|| part.get("id").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string(),
                    name: call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    arguments: call.get("args").cloned().unwrap_or(json!({})),
                });
            }
        }
        if !thinking.is_empty() {
            out.thinking = Some(thinking.clone());
            out.reasoning = Some(thinking);
        }
        out.tokens_in = data
            .get("usageMetadata")
            .and_then(|u| u.get("promptTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.tokens_out = data
            .get("usageMetadata")
            .and_then(|u| u.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.parts = parts;
        if !out.content.is_empty() {
            out.modalities = vec!["text".to_string()];
        }
        out.metadata.insert(
            "parts_provider".to_string(),
            Value::String("gemini".to_string()),
        );
        out
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Medium
    }

    fn default_model(&self) -> String {
        self.default_model_sync()
    }

    fn is_configured(&self) -> bool {
        self.api_key_sync().is_some_and(|k| !k.is_empty())
    }

    fn model_capabilities(&self, _model: Option<&str>) -> ModelCapabilities {
        ModelCapabilities {
            max_tokens: Some("max_output_tokens"),
            temperature: true,
            tools: true,
            system: true,
            stream: true,
        }
    }

    async fn verify(&self) -> bool {
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return false;
        };
        match self
            .client
            .get("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1")
            .query(&[("key", &key)])
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Ok(Vec::new());
        };
        let resp = match self
            .client
            .get("https://generativelanguage.googleapis.com/v1beta/models?pageSize=100")
            .query(&[("key", &key)])
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(data
            .get("models")
            .and_then(|m| m.as_array())
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| {
                        m.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Err(self.missing_key_error());
        };
        let body = self.request_body(messages, opts, &model);
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        );
        let resp = match self
            .client
            .post(&url)
            .query(&[("key", &key)])
            .json(&body)
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "gemini",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "gemini",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        if !status.is_success() {
            return Ok(AIResponse::with_error(
                "gemini",
                &model,
                http_error_message(status, &text),
                started,
            ));
        }
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Ok(AIResponse::with_error(
                    "gemini",
                    &model,
                    e.to_string(),
                    started,
                ));
            }
        };
        Ok(Self::parse_generate_content(&data, &model, started))
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Ok(single_chunk_stream("[Gemini not configured]".to_string()));
        };
        let started = Instant::now();
        let _ = started;
        let body = self.request_body(messages, opts, &model);
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent"
        );
        let resp = match self
            .client
            .post(&url)
            .query(&[("key", &key), ("alt", &"sse".to_string())])
            .json(&body)
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(single_chunk_stream(format!("[Gemini stream error: {e}]"))),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[Gemini stream error: {}]",
                http_error_message(status, &text)
            )));
        }
        Ok(sse_stream(
            resp,
            Box::new(|v: &Value| {
                let parts = v
                    .get("candidates")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("content"))
                    .and_then(|c| c.get("parts"));
                parts.map(|p| join_text_parts(p)).filter(|t| !t.is_empty())
            }),
            "Gemini",
        ))
    }
}

// ── Groq ───────────────────────────────────────────────────────────────────

/// Groq, OpenAI-compatible chat completions at Groq's base URL.
pub struct GroqProvider {
    config: AgentConfig,
    client: reqwest::Client,
    explicit_key: Option<String>,
    explicit_model: Option<String>,
}

impl GroqProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        GroqProvider {
            config,
            client,
            explicit_key: None,
            explicit_model: None,
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.explicit_key = Some(key.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.explicit_model = Some(model.into());
        self
    }

    fn api_key_sync(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        read_field_sync(&self.config, "groq", "KEY", None)
    }

    fn default_model_sync(&self) -> String {
        if let Some(m) = &self.explicit_model {
            return m.clone();
        }
        read_field_sync(&self.config, "groq", "MODEL", None)
            .unwrap_or_else(|| "llama-3.3-70b-versatile".to_string())
    }

    async fn api_key_async(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        self.config
            .config_field("groq", "KEY", None)
            .await
            .ok()
            .flatten()
    }


    fn missing_key_error(&self) -> Error {
        let key = first_db_key("groq", "KEY").unwrap_or("AGENT_CLOUD_GROQ_KEY");
        Error::Config(format!(
            "GROQ_API_KEY not set; set {key} (legacy env GROQ_API_KEY)"
        ))
    }
}

const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

#[async_trait]
impl Provider for GroqProvider {
    fn name(&self) -> &'static str {
        "groq"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Cheap
    }

    fn default_model(&self) -> String {
        self.default_model_sync()
    }

    fn is_configured(&self) -> bool {
        self.api_key_sync().is_some_and(|k| !k.is_empty())
    }

    fn model_capabilities(&self, _model: Option<&str>) -> ModelCapabilities {
        ModelCapabilities::base()
    }

    async fn verify(&self) -> bool {
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return false;
        };
        match self
            .client
            .get(format!("{GROQ_BASE_URL}/models"))
            .bearer_auth(&key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        // The Python provider defines no listing; the base returns [].
        Ok(Vec::new())
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Err(self.missing_key_error());
        };
        let caps = self.model_capabilities(Some(&model));
        Ok(openai_chat_complete(
            &self.client,
            GROQ_BASE_URL,
            &key,
            "groq",
            &model,
            messages,
            opts,
            &caps,
            started,
        )
        .await)
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let Some(key) = self.api_key_async().await.filter(|k| !k.is_empty()) else {
            return Ok(single_chunk_stream("[Groq not configured]".to_string()));
        };
        let caps = self.model_capabilities(Some(&model));
        let mut msgs: Vec<Value> = Vec::new();
        if let Some(system) = &opts.system {
            msgs.push(json!({"role": "system", "content": system}));
        }
        msgs.extend(messages.iter().map(|m| m.to_openai()));
        let mut body = completion_params(&caps, opts.max_tokens, opts.temperature);
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("messages".to_string(), Value::Array(msgs));
        body.insert("stream".to_string(), Value::Bool(true));
        if let Some(tools) = &opts.tools {
            if !tools.is_empty() {
                body.insert("tools".to_string(), Value::Array(tools.clone()));
                body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
            }
        }
        let resp = match self
            .client
            .post(format!("{GROQ_BASE_URL}/chat/completions"))
            .bearer_auth(&key)
            .json(&Value::Object(body))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(single_chunk_stream(format!("[Groq stream error: {e}]"))),
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[Groq stream error: {}]",
                http_error_message(status, &text)
            )));
        }
        Ok(sse_stream(
            resp,
            Box::new(|v: &Value| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            }),
            "Groq",
        ))
    }
}

// ── OpenAI-compatible family ───────────────────────────────────────────────

fn is_o_series(model: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^o\d").expect("o-series regex"));
    re.is_match(model)
}

/// GPT-5 and Codex are Responses-first (`_uses_openai_responses`).
fn uses_openai_responses(model: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(?:gpt-5|codex)").expect("responses model regex"));
    re.is_match(model)
}

/// Shared engine for every OpenAI-wire-protocol provider: plain OpenAI,
/// the configurable compat family, xAI, apimix, Meta Llama, OpenRouter and
/// the template. Each public struct below is one set of identity fields
/// over this engine.
struct OpenAiCompatInner {
    name: &'static str,
    label: &'static str,
    tier: CostTier,
    config_provider: &'static str,
    env_prefix: &'static str,
    default_base_url: Option<&'static str>,
    fallback_model: &'static str,
    /// True only for the real OpenAI provider: Responses routing applies
    /// only to OpenAI's own reasoning models on OpenAI's own endpoint.
    responses_eligible: bool,
    /// Compat deployments need a base URL as well as a key; plain OpenAI
    /// needs a key only.
    requires_base: bool,
    /// Local gateways frequently do not authenticate but require a value:
    /// the key defaults to `"none"` and only the base URL gates setup.
    no_key_required: bool,
    /// OpenRouter only: retry a model-unavailable error once with the safe
    /// default model.
    openrouter_retry: bool,
    config: AgentConfig,
    client: reqwest::Client,
    explicit_key: Option<String>,
    explicit_base: Option<String>,
    explicit_model: Option<String>,
}

impl OpenAiCompatInner {
    async fn api_key_async(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        let v = self
            .config
            .config_field(self.config_provider, "KEY", None)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        if v.is_none() && self.no_key_required {
            return Some("none".to_string());
        }
        v
    }

    async fn base_url_async(&self) -> Option<String> {
        if let Some(b) = &self.explicit_base {
            return Some(b.clone());
        }
        self.config
            .config_field(self.config_provider, "BASE", None)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .or_else(|| self.default_base_url.map(|s| s.to_string()))
    }


    fn api_key_sync(&self) -> Option<String> {
        if let Some(k) = &self.explicit_key {
            return Some(k.clone());
        }
        let v = read_field_sync(&self.config, self.config_provider, "KEY", None);
        if v.is_none() && self.no_key_required {
            return Some("none".to_string());
        }
        v
    }

    fn base_url_sync(&self) -> Option<String> {
        if let Some(b) = &self.explicit_base {
            return Some(b.clone());
        }
        read_field_sync(&self.config, self.config_provider, "BASE", None)
            .or_else(|| self.default_base_url.map(|s| s.to_string()))
    }

    fn default_model_sync(&self) -> String {
        if let Some(m) = &self.explicit_model {
            return m.clone();
        }
        read_field_sync(&self.config, self.config_provider, "MODEL", None)
            .unwrap_or_else(|| self.fallback_model.to_string())
    }

    fn is_configured_sync(&self) -> bool {
        let key_ok = self.api_key_sync().is_some_and(|k| !k.is_empty());
        if self.requires_base {
            key_ok && self.base_url_sync().is_some_and(|b| !b.is_empty())
        } else {
            key_ok
        }
    }

    fn model_capabilities(&self, model: Option<&str>) -> ModelCapabilities {
        let mut caps = ModelCapabilities::base();
        let model = model.unwrap_or(&self.default_model_sync()).to_string();
        if self.use_responses_api_sync(&model) {
            // Native Responses reasoning models use this output-token cap
            // and reject sampling controls such as temperature.
            caps.max_tokens = Some("max_output_tokens");
            caps.temperature = false;
        } else if is_o_series(&model) {
            // Compatibility endpoints retain their existing Chat Completions
            // contract even when their model name resembles an o-series one.
            caps.max_tokens = Some("max_completion_tokens");
            caps.temperature = false;
        }
        caps
    }

    /// Use native Responses only for OpenAI's own reasoning models.
    /// OpenAI-compatible endpoints frequently expose only Chat Completions,
    /// even when they use an o-series-like model name, so a configured base
    /// URL keeps the established compatibility path.
    fn use_responses_api_sync(&self, model: &str) -> bool {
        self.responses_eligible && self.base_url_sync().is_none() && uses_openai_responses(model)
    }

    async fn use_responses_api(&self, model: &str) -> bool {
        self.responses_eligible
            && self.base_url_async().await.is_none()
            && uses_openai_responses(model)
    }

    fn missing_credential_error(&self) -> Error {
        let prefix = self.env_prefix;
        if self.requires_base && self.base_url_sync().is_none() {
            let key =
                first_db_key(self.config_provider, "BASE").unwrap_or("AGENT_CLOUD_COMPAT_BASE");
            return Error::Config(format!(
                "{prefix}_BASE_URL not set; set {key} (legacy env {prefix}_BASE_URL)"
            ));
        }
        let key = first_db_key(self.config_provider, "KEY").unwrap_or("AGENT_CLOUD_OPENAI_KEY");
        Error::Config(format!(
            "{prefix}_API_KEY not set; set {key} (legacy env {prefix}_API_KEY)"
        ))
    }

    async fn verify(&self) -> bool {
        if !self.is_configured_sync() {
            return false;
        }
        let Some(key) = self.api_key_async().await else {
            return false;
        };
        let Some(base) = self.base_url_async().await else {
            return false;
        };
        match self
            .client
            .get(format!("{}/models", base.trim_end_matches('/')))
            .bearer_auth(&key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let Some(key) = self.api_key_async().await else {
            return Ok(Vec::new());
        };
        let Some(base) = self.base_url_async().await else {
            return Ok(Vec::new());
        };
        let resp = match self
            .client
            .get(format!("{}/models", base.trim_end_matches('/')))
            .bearer_auth(&key)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(Vec::new()),
        };
        let data: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(data
            .get("data")
            .and_then(|d| d.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| {
                        i.get("id")
                            .and_then(|id| id.as_str())
                            .map(|s| s.to_string())
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Translate Chat Completions function schemas to Responses schemas.
    fn responses_tools(tools: &[Value]) -> Vec<Value> {
        let mut result: Vec<Value> = Vec::new();
        for tool in tools {
            let Some(obj) = tool.as_object() else {
                result.push(tool.clone());
                continue;
            };
            if obj.get("type").and_then(|t| t.as_str()) != Some("function")
                || !obj.contains_key("function")
            {
                result.push(tool.clone());
                continue;
            }
            let function = &obj["function"];
            let mut out = Map::new();
            for key in ["name", "description", "parameters", "strict"] {
                if let Some(v) = function.get(key) {
                    out.insert(key.to_string(), v.clone());
                }
            }
            out.insert("type".to_string(), Value::String("function".to_string()));
            result.push(Value::Object(out));
        }
        result
    }

    /// Build a safe Responses continuation from normalized messages. Only
    /// plain, OpenAI-Responses-origin items are replayed; text represented
    /// by a previous message item is not appended again.
    fn responses_input(messages: &[AIMessage], system: Option<&str>) -> Vec<Value> {
        let mut items: Vec<Value> = Vec::new();
        if let Some(system) = system {
            items.push(json!({"role": "system", "content": system}));
        }
        for message in messages {
            if message.role == "tool" {
                let output = match &message.tool_result {
                    Some(v) => value_to_string(v),
                    None => message.text(),
                };
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "output": output,
                }));
                continue;
            }
            let native = native_parts(message, "openai_responses");
            let mut known_call_ids: Vec<String> = Vec::new();
            if message.role == "assistant" && !native.is_empty() {
                let plain: Vec<Value> = native;
                known_call_ids = plain
                    .iter()
                    .filter_map(|i| {
                        i.get("call_id")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                items.extend(plain);
                // Native message output already contains its text, so
                // adding `message.content` here would duplicate the turn.
            } else {
                let origin = metadata_origin(&message.metadata);
                let content =
                    if message.role == "user" && !message.parts.is_empty() && origin.is_none() {
                        Value::Array(message.parts.clone())
                    } else {
                        Value::String(message.text())
                    };
                items.push(json!({"role": message.role, "content": content}));
            }
            if message.role == "assistant" {
                for call in &message.tool_calls {
                    if !call.id.is_empty() && known_call_ids.iter().any(|id| id == &call.id) {
                        continue;
                    }
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": tool_arguments_json(&call.arguments),
                    }));
                }
            }
        }
        items
    }

    async fn complete_responses(
        &self,
        messages: &[AIMessage],
        model: &str,
        opts: &CompleteOptions,
        caps: &ModelCapabilities,
        started: Instant,
    ) -> AIResponse {
        let Some(key) = self.api_key_async().await else {
            return AIResponse::with_error(self.name, model, "API key not set", started);
        };
        let base = self
            .base_url_async()
            .await
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let mut body = completion_params(caps, opts.max_tokens, opts.temperature);
        body.insert("model".to_string(), Value::String(model.to_string()));
        body.insert(
            "input".to_string(),
            Value::Array(Self::responses_input(messages, opts.system.as_deref())),
        );
        if let Some(tools) = &opts.tools {
            if !tools.is_empty() {
                body.insert(
                    "tools".to_string(),
                    Value::Array(Self::responses_tools(tools)),
                );
                body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
            }
        }
        // `CompleteOptions` carries no reasoning control; the Responses
        // call runs without a `reasoning` object (the Python `reasoning`
        // kwarg has no counterpart here).
        let url = format!("{}/responses", base.trim_end_matches('/'));
        let resp = match self
            .client
            .post(&url)
            .bearer_auth(&key)
            .json(&Value::Object(body))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return AIResponse::with_error(self.name, model, e.to_string(), started),
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return AIResponse::with_error(self.name, model, e.to_string(), started),
        };
        if !status.is_success() {
            return AIResponse::with_error(
                self.name,
                model,
                http_error_message(status, &text),
                started,
            );
        }
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return AIResponse::with_error(self.name, model, e.to_string(), started),
        };
        let output: Vec<Value> = data
            .get("output")
            .and_then(|o| o.as_array())
            .cloned()
            .unwrap_or_default();
        let mut content = data
            .get("output_text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let mut reasoning_summaries: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for item in &output {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("reasoning") => {
                    if let Some(summary) = item.get("summary") {
                        let parts: Vec<&Value> = match summary {
                            Value::Array(a) => a.iter().collect(),
                            other => vec![other],
                        };
                        for part in parts {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                reasoning_summaries.push(t.to_string());
                            } else if let Value::String(s) = part {
                                reasoning_summaries.push(s.clone());
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(ToolCall {
                        id: item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: tool_arguments(
                            item.get("arguments")
                                .unwrap_or(&Value::String("{}".to_string())),
                        ),
                    });
                }
                Some("message") if content.is_empty() => {
                    content = join_text_parts(item.get("content").unwrap_or(&Value::Null));
                }
                _ => {}
            }
        }
        let usage = data.get("usage");
        let mut out = AIResponse::new(self.name, model);
        out.content = content.clone();
        out.tokens_in = usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.tokens_out = usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        out.tool_calls = tool_calls;
        if !reasoning_summaries.is_empty() {
            out.reasoning = Some(reasoning_summaries.join("\n"));
        }
        out.parts = output;
        if !content.is_empty() {
            out.modalities = vec!["text".to_string()];
        }
        out.metadata.insert(
            "parts_provider".to_string(),
            Value::String("openai_responses".to_string()),
        );
        out
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        let key = match self.api_key_async().await.filter(|k| !k.is_empty()) {
            Some(k) => k,
            None => return Err(self.missing_credential_error()),
        };
        let base = match self.base_url_async().await.filter(|b| !b.is_empty()) {
            Some(b) => b,
            None => {
                if self.requires_base {
                    return Err(self.missing_credential_error());
                }
                "https://api.openai.com/v1".to_string()
            }
        };
        let caps = self.model_capabilities(Some(&model));
        let response = if self.use_responses_api(&model).await {
            self.complete_responses(messages, &model, opts, &caps, started)
                .await
        } else {
            openai_chat_complete(
                &self.client,
                &base,
                &key,
                self.name,
                &model,
                messages,
                opts,
                &caps,
                started,
            )
            .await
        };
        // OpenRouter only: retry an unavailable configured model once with
        // the safe default.
        if self.openrouter_retry
            && !response.ok()
            && model != self.fallback_model
            && Self::model_unavailable(response.error.as_deref())
        {
            let fallback = openai_chat_complete(
                &self.client,
                &base,
                &key,
                self.name,
                self.fallback_model,
                messages,
                opts,
                &caps,
                started,
            )
            .await;
            if fallback.ok() {
                return Ok(fallback);
            }
            let mut fallback = fallback;
            let prev_error = fallback.error.take().unwrap_or_default();
            fallback.error = Some(format!(
                "Configured OpenRouter model {model:?} has no available endpoint; fallback {:?} also failed: {prev_error}",
                self.fallback_model
            ));
            return Ok(fallback);
        }
        Ok(response)
    }

    /// Recognize OpenRouter's model-specific 404 responses.
    fn model_unavailable(error: Option<&str>) -> bool {
        let message = error.unwrap_or("").to_lowercase();
        ["no endpoints found", "model not found", "unknown model"]
            .iter()
            .any(|marker| message.contains(marker))
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        if !self.is_configured_sync() {
            return Ok(single_chunk_stream(format!(
                "[{} not configured]",
                self.label
            )));
        }
        let key = match self.api_key_async().await.filter(|k| !k.is_empty()) {
            Some(k) => k,
            None => {
                return Ok(single_chunk_stream(format!(
                    "[{} not configured]",
                    self.label
                )));
            }
        };
        let base = match self.base_url_async().await.filter(|b| !b.is_empty()) {
            Some(b) => b,
            None => "https://api.openai.com/v1".to_string(),
        };
        let caps = self.model_capabilities(Some(&model));
        let mut msgs: Vec<Value> = Vec::new();
        if let Some(system) = &opts.system {
            msgs.push(json!({"role": "system", "content": system}));
        }
        msgs.extend(messages.iter().map(|m| m.to_openai()));
        let mut body = completion_params(&caps, opts.max_tokens, opts.temperature);
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("messages".to_string(), Value::Array(msgs));
        body.insert("stream".to_string(), Value::Bool(true));
        if let Some(tools) = &opts.tools {
            if !tools.is_empty() {
                body.insert("tools".to_string(), Value::Array(tools.clone()));
                body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
            }
        }
        let resp = match self
            .client
            .post(format!("{}/chat/completions", base.trim_end_matches('/')))
            .bearer_auth(&key)
            .json(&Value::Object(body))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(single_chunk_stream(format!(
                    "[{} stream error: {e}]",
                    self.label
                )));
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[{} stream error: {}]",
                self.label,
                http_error_message(status, &text)
            )));
        }
        Ok(sse_stream(
            resp,
            Box::new(|v: &Value| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            }),
            self.label,
        ))
    }
}

macro_rules! impl_compat_provider {
    ($ty:ty) => {
        #[async_trait]
        impl Provider for $ty {
            fn name(&self) -> &'static str {
                self.inner.name
            }

            fn cost_tier(&self) -> CostTier {
                self.inner.tier
            }

            fn default_model(&self) -> String {
                self.inner.default_model_sync()
            }

            fn is_configured(&self) -> bool {
                self.inner.is_configured_sync()
            }

            fn model_capabilities(&self, model: Option<&str>) -> ModelCapabilities {
                self.inner.model_capabilities(model)
            }

            async fn verify(&self) -> bool {
                self.inner.verify().await
            }

            async fn list_models(&self) -> Result<Vec<String>> {
                self.inner.list_models().await
            }

            async fn complete(
                &self,
                messages: &[AIMessage],
                opts: &CompleteOptions,
            ) -> Result<AIResponse> {
                self.inner.complete(messages, opts).await
            }

            async fn stream(
                &self,
                messages: &[AIMessage],
                opts: &CompleteOptions,
            ) -> Result<BoxStream<'static, Result<String>>> {
                self.inner.stream(messages, opts).await
            }
        }
    };
}

/// OpenAI: Chat Completions + GPT-5/Codex Responses routing per
/// `_is_o_series` / `_uses_openai_responses`. The o-series keeps the
/// Chat Completions path with `max_completion_tokens` and no temperature.
pub struct OpenAIProvider {
    inner: OpenAiCompatInner,
}

impl OpenAIProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        OpenAIProvider {
            inner: OpenAiCompatInner {
                name: "openai",
                label: "OpenAI",
                tier: CostTier::Premium,
                config_provider: "openai",
                env_prefix: "OPENAI",
                default_base_url: None,
                fallback_model: "gpt-4o-mini",
                responses_eligible: true,
                requires_base: false,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(OpenAIProvider);

/// OpenAI wire-protocol provider with explicit per-client configuration.
/// Custom gateways, xAI and the official Meta Llama API's OpenAI-compatible
/// deployment all share the same normalized tool/message contract.
pub struct ConfigurableOpenAIProvider {
    inner: OpenAiCompatInner,
}

impl ConfigurableOpenAIProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        ConfigurableOpenAIProvider {
            inner: OpenAiCompatInner {
                name: "openai_compat",
                label: "OpenAI-compatible",
                tier: CostTier::Medium,
                config_provider: "compat",
                env_prefix: "COMPAT",
                default_base_url: None,
                fallback_model: "gpt-4o-mini",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    /// Build a compat provider under a different registry name, for the
    /// template use-case.
    pub fn named(
        config: AgentConfig,
        client: reqwest::Client,
        name: &'static str,
        label: &'static str,
        tier: CostTier,
        config_provider: &'static str,
        env_prefix: &'static str,
        default_base_url: Option<&'static str>,
        fallback_model: &'static str,
    ) -> Self {
        ConfigurableOpenAIProvider {
            inner: OpenAiCompatInner {
                name,
                label,
                tier,
                config_provider,
                env_prefix,
                default_base_url,
                fallback_model,
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(ConfigurableOpenAIProvider);

/// Any OpenAI-compatible endpoint (LM Studio, vLLM, etc.). Local gateways
/// frequently do not authenticate, but require a value, so the key defaults
/// to `"none"` and only the base URL gates setup.
pub struct OpenAICompatProvider {
    inner: OpenAiCompatInner,
}

impl OpenAICompatProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        OpenAICompatProvider {
            inner: OpenAiCompatInner {
                name: "openai_compat",
                label: "OpenAI-compatible",
                tier: CostTier::Free,
                config_provider: "openai_compat",
                env_prefix: "COMPAT",
                default_base_url: None,
                fallback_model: "local-model",
                responses_eligible: false,
                requires_base: true,
                no_key_required: true,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(OpenAICompatProvider);

/// xAI Grok API, which uses the OpenAI chat-completions wire contract.
pub struct XAIProvider {
    inner: OpenAiCompatInner,
}

impl XAIProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        XAIProvider {
            inner: OpenAiCompatInner {
                name: "xai",
                label: "xAI",
                tier: CostTier::Premium,
                config_provider: "xai",
                env_prefix: "XAI",
                default_base_url: Some("https://api.x.ai/v1"),
                fallback_model: "grok-3-mini",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(XAIProvider);

/// apmix.ai aggregator — one key across many upstream model vendors, on an
/// OpenAI-compatible Chat Completions surface.
pub struct ApimixProvider {
    inner: OpenAiCompatInner,
}

impl ApimixProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        ApimixProvider {
            inner: OpenAiCompatInner {
                name: "apimix",
                label: "apimix",
                tier: CostTier::Cheap,
                config_provider: "apimix",
                env_prefix: "APIMIX",
                default_base_url: Some("https://api.apmix.ai/v1"),
                fallback_model: "openai/gpt-5.6-luna",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(ApimixProvider);

/// Meta Llama API adapter (OpenAI-compatible deployment at
/// `https://api.llama.com/compat/v1`); a custom base URL stays supported
/// for hosted Llama / vLLM / Together.
pub struct MetaLlamaProvider {
    inner: OpenAiCompatInner,
}

impl MetaLlamaProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        MetaLlamaProvider {
            inner: OpenAiCompatInner {
                name: "meta_llama",
                label: "Meta Llama",
                tier: CostTier::Medium,
                config_provider: "meta_llama",
                env_prefix: "META_LLAMA",
                default_base_url: Some("https://api.llama.com/compat/v1"),
                fallback_model: "Llama-4-Maverick-17B-128E-Instruct-FP8",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(MetaLlamaProvider);

/// OpenRouter's OpenAI-compatible gateway. Retries an unavailable
/// configured model once with the safe default.
pub struct OpenRouterProvider {
    inner: OpenAiCompatInner,
}

impl OpenRouterProvider {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        OpenRouterProvider {
            inner: OpenAiCompatInner {
                name: "openrouter",
                label: "OpenRouter",
                tier: CostTier::Cheap,
                config_provider: "openrouter",
                env_prefix: "OPENROUTER",
                default_base_url: Some("https://openrouter.ai/api/v1"),
                fallback_model: "openai/gpt-4o-mini",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: true,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(OpenRouterProvider);

/// Template for a new OpenAI-compatible provider. Subclass it (or use
/// `ConfigurableOpenAIProvider::named`), set `name`, `env_prefix`,
/// `default_base_url` and `fallback_model`. The Python template reads the
/// `compat` configuration until the provider gets its own.
pub struct ProviderTemplate {
    inner: OpenAiCompatInner,
}

impl ProviderTemplate {
    pub fn new(config: AgentConfig, client: reqwest::Client) -> Self {
        ProviderTemplate {
            inner: OpenAiCompatInner {
                name: "template",
                label: "Template",
                tier: CostTier::Medium,
                config_provider: "compat",
                env_prefix: "TEMPLATE",
                default_base_url: None,
                fallback_model: "gpt-4o-mini",
                responses_eligible: false,
                requires_base: true,
                no_key_required: false,
                openrouter_retry: false,
                config,
                client,
                explicit_key: None,
                explicit_base: None,
                explicit_model: None,
            },
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.explicit_key = Some(key.into());
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner.explicit_base = Some(base_url.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.inner.explicit_model = Some(model.into());
        self
    }
}

impl_compat_provider!(ProviderTemplate);

// ── Cloudflare Workers AI ──────────────────────────────────────────────────

/// Cloudflare Workers AI REST API (no SDK): one endpoint per model,
/// `POST https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{model}`.
/// The same provider dispatches text-generation AND image-generation
/// models — the model name decides which response shape to parse.
///
/// NOTE: the Python provider reads `CF_ACCOUNT` / `CF_TOKEN` / `CF_MODEL`
/// from `udB` directly, and `config.rs` defines no Cloudflare provider
/// spec, so `AgentConfig::config_field("cloudflare", …)` cannot resolve
/// them. Credentials therefore come only from the explicit setters below
/// until a Cloudflare spec is added to `config.rs`; a missing credential
/// is `Error::Config` naming the exact `CF_*` key.
pub struct CloudflareProvider {
    client: reqwest::Client,
    account: Option<String>,
    token: Option<String>,
    explicit_model: Option<String>,
}

impl CloudflareProvider {
    /// Model-name hints for image-generation models.
    pub const IMAGE_MODEL_HINTS: &'static [&'static str] = &[
        "stable-diffusion",
        "sdxl",
        "flux-1",
        "dreamshaper",
        "sd-v1-5",
        "real-esrgan",
        "img2img",
        "inpainting",
    ];

    pub const TEXT_MODELS: &'static [&'static str] = &[
        "@cf/meta/llama-3.1-8b-instruct",
        "@cf/meta/llama-3.1-70b-instruct",
        "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
        "@cf/meta/llama-3-8b-instruct",
        "@hf/nous/hermes-2-pro-mistral-7b",
        "@cf/qwen/qwen1.5-14b-chat-awq",
    ];

    pub const IMAGE_MODELS: &'static [&'static str] = &[
        "@cf/black-forest-labs/flux-1-schnell",
        "@cf/stabilityai/stable-diffusion-xl-base-1.0",
        "@cf/stabilityai/stable-diffusion-xl-lightning",
        "@cf/bytedance/stable-diffusion-xl-lightning",
        "@cf/lykon/dreamshaper-8-lcm",
    ];

    pub const DEFAULT_MODEL: &'static str = "@cf/meta/llama-3.1-8b-instruct";

    pub fn new(_config: AgentConfig, client: reqwest::Client) -> Self {
        CloudflareProvider {
            client,
            account: None,
            token: None,
            explicit_model: None,
        }
    }

    /// Credentials as read from `udB` keys `CF_ACCOUNT` / `CF_TOKEN` /
    /// `CF_MODEL` in the Python provider.
    pub fn with_credentials(
        mut self,
        account: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        self.account = Some(account.into());
        self.token = Some(token.into());
        self
    }

    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.explicit_model = Some(model.into());
        self
    }

    fn default_model_sync(&self) -> String {
        self.explicit_model
            .clone()
            .unwrap_or_else(|| Self::DEFAULT_MODEL.to_string())
    }

    fn is_image_model(model: &str) -> bool {
        let model = model.to_lowercase();
        Self::IMAGE_MODEL_HINTS
            .iter()
            .any(|hint| model.contains(hint))
    }

    fn endpoint(&self, model: &str) -> Option<String> {
        let account = self.account.as_ref()?;
        Some(format!(
            "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{model}"
        ))
    }

    fn missing_credential_error() -> Error {
        Error::Config(
            "CF_ACCOUNT/CF_TOKEN not set; set the CF_ACCOUNT and CF_TOKEN database keys"
                .to_string(),
        )
    }

    fn cf_error(data: &Value) -> String {
        if let Some(errs) = data.get("errors") {
            let first = match errs {
                Value::Array(a) => a.first().cloned(),
                other => Some(other.clone()),
            };
            if let Some(first) = first {
                if let Value::Object(_) = first {
                    if let Some(m) = first.get("message").and_then(|m| m.as_str()) {
                        return m.to_string();
                    }
                    return first.to_string();
                }
                return value_to_string(&first);
            }
        }
        data.get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Cloudflare AI request failed")
            .to_string()
    }

    async fn complete_text(
        &self,
        messages: &[AIMessage],
        model: &str,
        opts: &CompleteOptions,
        started: Instant,
    ) -> AIResponse {
        let Some(endpoint) = self.endpoint(model) else {
            return AIResponse::with_error("cloudflare", model, "CF_ACCOUNT not set", started);
        };
        let token = match &self.token {
            Some(t) => t.clone(),
            None => {
                return AIResponse::with_error("cloudflare", model, "CF_TOKEN not set", started);
            }
        };
        let caps = self.model_capabilities(Some(model));
        let mut msgs: Vec<Value> = Vec::new();
        if let Some(system) = &opts.system {
            msgs.push(json!({"role": "system", "content": system}));
        }
        msgs.extend(messages.iter().map(|m| m.to_openai()));
        let mut payload = completion_params(&caps, opts.max_tokens, opts.temperature);
        payload.insert("messages".to_string(), Value::Array(msgs));
        let resp = match self
            .client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&Value::Object(payload))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        if !status.is_success() || data.get("success").and_then(|s| s.as_bool()) != Some(true) {
            let error = if status.is_success() {
                Self::cf_error(&data)
            } else {
                http_error_message(status, &text)
            };
            return AIResponse::with_error("cloudflare", model, error, started);
        }
        let result = data.get("result").cloned().unwrap_or(Value::Null);
        let usage = result.get("usage").cloned().unwrap_or(Value::Null);
        let mut out = AIResponse::new("cloudflare", model);
        out.content = result
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        out.tokens_in = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.tokens_out = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        out
    }

    async fn complete_image(
        &self,
        messages: &[AIMessage],
        model: &str,
        started: Instant,
    ) -> AIResponse {
        let Some(endpoint) = self.endpoint(model) else {
            return AIResponse::with_error("cloudflare", model, "CF_ACCOUNT not set", started);
        };
        let token = match &self.token {
            Some(t) => t.clone(),
            None => {
                return AIResponse::with_error("cloudflare", model, "CF_TOKEN not set", started);
            }
        };
        // The prompt is the most recent user message.
        let prompt = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text())
            .unwrap_or_default();
        let payload = json!({"prompt": prompt});
        let resp = match self
            .client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&payload)
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return AIResponse::with_error("cloudflare", model, e.to_string(), started),
        };
        if !status.is_success() || data.get("success").and_then(|s| s.as_bool()) != Some(true) {
            let error = if status.is_success() {
                Self::cf_error(&data)
            } else {
                http_error_message(status, &text)
            };
            return AIResponse::with_error("cloudflare", model, error, started);
        }
        let result = data.get("result").cloned().unwrap_or(Value::Null);
        let Some(image_b64) = result.get("image").and_then(|i| i.as_str()) else {
            return AIResponse::with_error(
                "cloudflare",
                model,
                "No image in Cloudflare response",
                started,
            );
        };
        let mut out = AIResponse::new("cloudflare", model);
        out.content = format!("[image generated via {model}]");
        out.image = Some(image_b64.to_string());
        out.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        out
    }
}

#[async_trait]
impl Provider for CloudflareProvider {
    fn name(&self) -> &'static str {
        "cloudflare"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Cheap
    }

    fn default_model(&self) -> String {
        self.default_model_sync()
    }

    fn is_configured(&self) -> bool {
        self.account.as_ref().is_some_and(|a| !a.is_empty())
            && self.token.as_ref().is_some_and(|t| !t.is_empty())
    }

    fn model_capabilities(&self, model: Option<&str>) -> ModelCapabilities {
        let model = model.unwrap_or(Self::DEFAULT_MODEL);
        if Self::is_image_model(model) {
            return ModelCapabilities {
                max_tokens: None,
                temperature: false,
                tools: false,
                system: false,
                stream: false,
            };
        }
        ModelCapabilities {
            max_tokens: Some("max_tokens"),
            temperature: true,
            tools: false,
            system: true,
            stream: true,
        }
    }

    async fn verify(&self) -> bool {
        if !self.is_configured() {
            return false;
        }
        let Some(account) = &self.account else {
            return false;
        };
        let Some(token) = &self.token else {
            return false;
        };
        let url = format!("https://api.cloudflare.com/client/v4/accounts/{account}");
        match self
            .client
            .get(&url)
            .bearer_auth(token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r.status() == reqwest::StatusCode::OK,
            Err(_) => false,
        }
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        // The Python provider defines no listing; the base returns [].
        // The known models are exposed as `TEXT_MODELS` / `IMAGE_MODELS`.
        Ok(Vec::new())
    }

    async fn complete(&self, messages: &[AIMessage], opts: &CompleteOptions) -> Result<AIResponse> {
        let started = Instant::now();
        if !self.is_configured() {
            return Err(Self::missing_credential_error());
        }
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        if Self::is_image_model(&model) {
            Ok(self.complete_image(messages, &model, started).await)
        } else {
            Ok(self.complete_text(messages, &model, opts, started).await)
        }
    }

    async fn stream(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<BoxStream<'static, Result<String>>> {
        if !self.is_configured() {
            return Ok(single_chunk_stream(
                "[Cloudflare not configured]".to_string(),
            ));
        }
        let model = opts
            .model
            .clone()
            .unwrap_or_else(|| self.default_model_sync());
        if Self::is_image_model(&model) {
            return Ok(single_chunk_stream(
                "[Cloudflare image models do not support streaming]".to_string(),
            ));
        }
        let Some(endpoint) = self.endpoint(&model) else {
            return Ok(single_chunk_stream(
                "[Cloudflare not configured]".to_string(),
            ));
        };
        let Some(token) = self.token.clone() else {
            return Ok(single_chunk_stream(
                "[Cloudflare not configured]".to_string(),
            ));
        };
        let caps = self.model_capabilities(Some(&model));
        let mut msgs: Vec<Value> = Vec::new();
        if let Some(system) = &opts.system {
            msgs.push(json!({"role": "system", "content": system}));
        }
        msgs.extend(messages.iter().map(|m| m.to_openai()));
        let mut payload = completion_params(&caps, opts.max_tokens, opts.temperature);
        payload.insert("messages".to_string(), Value::Array(msgs));
        payload.insert("stream".to_string(), Value::Bool(true));
        let resp = match self
            .client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&Value::Object(payload))
            .timeout(Duration::from_secs(120))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(single_chunk_stream(format!(
                    "[Cloudflare stream error: {e}]"
                )));
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Ok(single_chunk_stream(format!(
                "[Cloudflare stream error: {}]",
                http_error_message(status, &text)
            )));
        }
        Ok(sse_stream(
            resp,
            Box::new(|v: &Value| {
                v.get("response")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string())
            }),
            "Cloudflare",
        ))
    }
}

// ── Provider registry ──────────────────────────────────────────────────────

/// Holds all provider instances. Exposes configured providers in priority
/// order: FREE → CHEAP → MEDIUM → PREMIUM.
pub struct ProviderRegistry {
    config: AgentConfig,
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new(config: AgentConfig) -> Self {
        let client = reqwest::Client::new();
        // The Python registry's order: Ollama, compat, Groq, Gemini,
        // apimix, OpenRouter, OpenAI, Anthropic, plus Cloudflare when
        // configured (here always present; `is_configured` gates it).
        let providers: Vec<Arc<dyn Provider>> = vec![
            Arc::new(OllamaProvider::new(config.clone(), client.clone())),
            Arc::new(OpenAICompatProvider::new(config.clone(), client.clone())),
            Arc::new(GroqProvider::new(config.clone(), client.clone())),
            Arc::new(GeminiProvider::new(config.clone(), client.clone())),
            Arc::new(ApimixProvider::new(config.clone(), client.clone())),
            Arc::new(OpenRouterProvider::new(config.clone(), client.clone())),
            Arc::new(OpenAIProvider::new(config.clone(), client.clone())),
            Arc::new(AnthropicProvider::new(config.clone(), client.clone())),
            Arc::new(CloudflareProvider::new(config.clone(), client.clone())),
        ];
        ProviderRegistry { config, providers }
    }

    fn construct_by_name(&self, name: &str) -> Option<Arc<dyn Provider>> {
        let client = reqwest::Client::new();
        let config = self.config.clone();
        match name {
            "xai" => Some(Arc::new(XAIProvider::new(config, client))),
            "meta_llama" => Some(Arc::new(MetaLlamaProvider::new(config, client))),
            "template" => Some(Arc::new(ProviderTemplate::new(config, client))),
            "configurable" | "openai_compat_base" => {
                Some(Arc::new(ConfigurableOpenAIProvider::new(config, client)))
            }
            _ => None,
        }
    }

    /// Resolve a provider by name (`"compat"` maps to `openai_compat`,
    /// matching `config.rs`).
    pub fn provider(&self, name: &str) -> Result<Arc<dyn Provider>> {
        let key = name.to_lowercase();
        let key = if key == "compat" {
            "openai_compat".to_string()
        } else {
            key
        };
        if let Some(p) = self.providers.iter().find(|p| p.name() == key) {
            return Ok(p.clone());
        }
        if let Some(p) = self.construct_by_name(&key) {
            return Ok(p);
        }
        let names: Vec<&str> = self
            .providers
            .iter()
            .map(|p| p.name())
            .chain(["xai", "meta_llama", "template"])
            .collect();
        Err(Error::Config(format!(
            "Unknown AI provider {name:?}; expected one of: {}",
            names.join(", ")
        )))
    }

    /// The default provider for bare `.ai` commands: the configured pin
    /// from `AgentConfig::config_default_provider` (which itself prefers a
    /// configured apimix account), else the cheapest configured provider
    /// by cost tier.
    pub async fn default_provider(&self) -> Result<Arc<dyn Provider>> {
        if let Some(pinned) = self.config.config_default_provider().await? {
            return self.provider(&pinned);
        }
        // Cheapest configured provider, tier by tier — the Python
        // `provider_cheapest` rule (`providers_configured` reads
        // `is_configured`, so Ollama, always configured, wins the FREE
        // tier whenever it is listed).
        for tier in [
            CostTier::Free,
            CostTier::Cheap,
            CostTier::Medium,
            CostTier::Premium,
        ] {
            for candidate in self.providers.iter().filter(|p| p.cost_tier() == tier) {
                if candidate.is_configured() {
                    return Ok(candidate.clone());
                }
            }
        }
        Err(Error::Config(
            "No AI provider is configured; configure one with .aiconf first".to_string(),
        ))
    }

    pub fn all(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.clone()
    }
}
