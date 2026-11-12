//! Public, provider-neutral AI client facade — port of
//! `pyUltroid/agent/_client.py`.
//!
//! The client deliberately delegates protocol details to `crate::providers`.
//!
//! A provider *name* resolves through the registry, whose instances
//! are config-backed (they read `AGENT_*` keys from the database).
//! Explicit `key` / `base_url` overrides are stored on the client but
//! cannot be applied to a registry instance — the `Provider` trait has
//! no way to receive an API key or base URL; only `model` reaches the
//! provider, through the `model` slot of `CompleteOptions`.

use std::collections::HashMap;
use std::sync::Arc;

use futures::stream::BoxStream;
use serde_json::{Value, json};
use ultroid_core::error::{Error, Result};

use ultroid_core::db::Database;

use crate::config::AgentConfig;
use crate::providers::{AIMessage, AIResponse, CompleteOptions, Provider, ProviderRegistry};

/// One message input: a bare prompt string, an `AIMessage`, or a dict.
#[derive(Debug, Clone)]
pub enum MessageInput {
    Text(String),
    Message(AIMessage),
    Dict(Value),
}

/// Per-call controls for [`AIClient::chat`], mirroring its Python kwargs.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub tools: Option<Vec<Value>>,
    pub system: Option<String>,
    pub model: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Passed through only where the provider supports it; the sibling
    /// `CompleteOptions` has no reasoning slot, so these are recorded
    /// and dropped (see [`AIClient::chat`]).
    pub reasoning: Option<Value>,
    pub reasoning_effort: Option<Value>,
    pub tool_call: Option<Value>,
    /// Gemini-only response modalities (uppercased); no slot exists in
    /// `CompleteOptions`, so these are recorded and dropped.
    pub response_modalities: Option<Vec<String>>,
    /// Any further provider controls.
    pub extra: HashMap<String, Value>,
}

impl ChatOptions {
    pub fn new() -> Self {
        ChatOptions {
            max_tokens: 1024,
            temperature: 0.7,
            ..Default::default()
        }
    }
}

/// Small uniform client for built-in and custom AI providers.
///
/// `provider` accepts a registered provider name or a provider
/// instance. Explicit `key`, `base_url` and `model` values always win
/// over saved configuration where the provider surface allows it, which
/// makes it suitable for one-off clients.
pub struct AIClient {
    pub provider_name: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub use_tools: bool,
    pub key: Option<String>,
    pub options: HashMap<String, Value>,
    provider: Arc<dyn Provider>,
}

const KNOWN_PROVIDERS: &[&str] = &[
    "openai",
    "gemini",
    "google",
    "groq",
    "anthropic",
    "claude",
    "ollama",
    "xai",
    "grok",
    "apimix",
    "apmix",
    "meta",
    "meta_llama",
    "llama",
    "openrouter",
    "openai_compat",
    "compat",
    "custom",
];

/// Canonical registry name for a user-supplied provider name.
fn canonical_provider_name(name: &str) -> String {
    match name.to_lowercase().replace('-', "_").as_str() {
        "google" => "gemini".to_string(),
        "claude" => "anthropic".to_string(),
        "grok" => "xai".to_string(),
        "apmix" => "apimix".to_string(),
        "meta" | "llama" => "meta_llama".to_string(),
        "compat" | "custom" => "openai_compat".to_string(),
        other => other.to_string(),
    }
}

impl AIClient {
    /// Build a client around an already-constructed provider instance.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        AIClient {
            provider_name: provider.name().to_string(),
            base_url: None,
            model: None,
            use_tools: true,
            key: None,
            options: HashMap::new(),
            provider,
        }
    }

    /// Resolve *provider* (a registered name) into a client.
    ///
    /// Explicit `key` / `base_url` are stored on the client but cannot
    /// be applied to the registry instance — see the module docs.
    /// The registry needs a database for its `AgentConfig`.
    pub fn from_name(
        provider: &str,
        base_url: Option<String>,
        model: Option<String>,
        key: Option<String>,
        use_tools: bool,
        db: Option<Arc<dyn Database>>,
    ) -> Result<Self> {
        let name = provider.to_lowercase().replace('-', "_");
        if !KNOWN_PROVIDERS.contains(&name.as_str()) {
            let known: Vec<&str> = {
                let mut k: Vec<&str> = KNOWN_PROVIDERS.to_vec();
                k.sort_unstable();
                k
            };
            return Err(Error::Config(format!(
                "Unknown AI provider {provider:?}; expected one of: {}",
                known.join(", ")
            )));
        }
        let canonical = canonical_provider_name(&name);
        let Some(db) = db else {
            return Err(Error::Config(
                "AIClient::from_name needs a database: ProviderRegistry reads provider configuration from AGENT_* keys".to_string(),
            ));
        };
        let registry = ProviderRegistry::new(AgentConfig::new(db));
        let instance = registry.provider(&canonical)?;
        Ok(AIClient {
            provider_name: canonical,
            base_url,
            model,
            use_tools,
            key,
            options: HashMap::new(),
            provider: instance,
        })
    }

    /// Normalise message inputs into `AIMessage`s.
    ///
    /// Never attaches request-only state (such as modalities) to the
    /// caller's history object: messages are cloned, structured blocks
    /// copied.
    pub fn normalize_messages(messages: &[MessageInput]) -> Result<Vec<AIMessage>> {
        let mut normalized = Vec::new();
        for message in messages {
            match message {
                MessageInput::Message(m) => normalized.push(m.clone()),
                MessageInput::Text(s) => normalized.push(AIMessage::new("user", s)),
                MessageInput::Dict(d) => normalized.push(message_from_dict(d)?),
            }
        }
        if normalized.is_empty() {
            return Err(Error::Other(
                "messages must contain at least one message".to_string(),
            ));
        }
        Ok(normalized)
    }

    async fn complete_inner(
        &self,
        messages: &[AIMessage],
        opts: &CompleteOptions,
    ) -> Result<AIResponse> {
        self.provider.complete(messages, opts).await
    }

    /// Perform the adapter's read-only authentication/model-list check.
    pub async fn verify(&self) -> bool {
        self.provider.verify().await
    }

    /// Generate one normalized response from messages or a prompt string.
    pub async fn chat(&self, messages: &[MessageInput], opts: &ChatOptions) -> Result<AIResponse> {
        let selected_tools = if self.use_tools {
            opts.tools.clone()
        } else {
            None
        };
        let normalized = Self::normalize_messages(messages)?;
        // `reasoning`, `reasoning_effort`, `tool_call` and
        // `response_modalities` have no slot in the sibling
        // `CompleteOptions`; they are accepted for call-site parity and
        // dropped here rather than silently retried against a provider
        // that cannot take them.
        let _ = (
            &opts.reasoning,
            &opts.reasoning_effort,
            &opts.tool_call,
            &opts.response_modalities,
        );
        let complete_opts = CompleteOptions {
            model: opts.model.clone().or_else(|| self.model.clone()),
            tools: selected_tools,
            max_tokens: opts.max_tokens,
            temperature: opts.temperature,
            system: opts.system.clone(),
        };
        self.complete_inner(&normalized, &complete_opts).await
    }

    /// Compatibility alias for [`AIClient::chat`].
    pub async fn complete(
        &self,
        messages: &[MessageInput],
        opts: &ChatOptions,
    ) -> Result<AIResponse> {
        self.chat(messages, opts).await
    }

    /// Yield text chunks from the selected provider.
    pub async fn stream(
        &self,
        messages: &[MessageInput],
        system: Option<&str>,
        model: Option<&str>,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let normalized = Self::normalize_messages(messages)?;
        let opts = CompleteOptions {
            model: model.map(|m| m.to_string()).or_else(|| self.model.clone()),
            tools: None,
            max_tokens,
            temperature,
            system: system.map(|s| s.to_string()),
        };
        self.provider.stream(&normalized, &opts).await
    }

    /// Return built-in provider instances by their public names.
    /// `None` database → empty map (the registry needs one).
    pub fn providers(db: Option<Arc<dyn Database>>) -> HashMap<String, Arc<dyn Provider>> {
        let Some(db) = db else {
            return HashMap::new();
        };
        let registry = ProviderRegistry::new(AgentConfig::new(db));
        let mut known: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        for p in registry.all() {
            known.insert(p.name().to_string(), Arc::clone(&p));
        }
        // Aliases, mirroring the Python `setdefault` block. The
        // registry constructs `xai` / `meta_llama` on demand.
        for (alias, canonical) in [
            ("xai", "xai"),
            ("meta", "meta_llama"),
            ("meta_llama", "meta_llama"),
            ("apimix", "apimix"),
        ] {
            if let Ok(p) = registry.provider(canonical) {
                known.entry(alias.to_string()).or_insert(p);
            }
        }
        known
    }

    /// Ask a search-capable model to research a query.
    ///
    /// Providers without a native search tool still receive an explicit
    /// research request rather than silently claiming that a web lookup ran.
    pub async fn search(&self, query: &str, opts: &ChatOptions) -> Result<AIResponse> {
        let mut search_opts = opts.clone();
        if search_opts.system.is_none() {
            search_opts.system = Some(
                "Use available search tools when present. Cite uncertainty clearly.".to_string(),
            );
        }
        self.chat(
            &[MessageInput::Text(format!("Research this query: {query}"))],
            &search_opts,
        )
        .await
    }

    /// Run an agent-ready completion with tool calling enabled.
    ///
    /// Tool execution remains application-owned; returned calls include
    /// their provider IDs so callers can append matching
    /// `AIMessage(role='tool', tool_call_id=...)` results on the
    /// following turn.
    pub async fn agent(
        &mut self,
        messages: &[MessageInput],
        opts: &ChatOptions,
    ) -> Result<AIResponse> {
        let previous = self.use_tools;
        self.use_tools = true;
        let result = self.chat(messages, opts).await;
        self.use_tools = previous;
        result
    }
}

/// Build an `AIMessage` from a dict, mirroring `AIMessage(**dict)`.
fn message_from_dict(d: &Value) -> Result<AIMessage> {
    let obj = d.as_object().ok_or_else(|| {
        Error::Other("messages must be strings, AIMessage values, or dictionaries".to_string())
    })?;
    let role = obj
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("user")
        .to_string();
    let content = obj
        .get("content")
        .cloned()
        .unwrap_or(Value::String(String::new()));
    let mut msg = match &content {
        Value::String(s) => AIMessage::new(role, s),
        Value::Array(parts) => {
            let mut m = AIMessage::new(role, "");
            m.parts = parts.clone();
            m
        }
        other => AIMessage::new(role, serde_json::to_string(other).unwrap_or_default()),
    };
    msg.content = content;
    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        msg.name = Some(name.to_string());
    }
    if let Some(parts) = obj.get("parts").and_then(|v| v.as_array()) {
        msg.parts = parts.clone();
    }
    if let Some(calls) = obj.get("tool_calls").and_then(|v| v.as_array()) {
        msg.tool_calls =
            crate::providers::normalise_openai_tool_calls(&Value::Array(calls.clone()));
    }
    if let Some(id) = obj.get("tool_call_id").and_then(|v| v.as_str()) {
        msg.tool_call_id = Some(id.to_string());
    }
    if let Some(tr) = obj.get("tool_result") {
        msg.tool_result = Some(tr.clone());
    }
    if let Some(r) = obj.get("reasoning").and_then(|v| v.as_str()) {
        msg.reasoning = Some(r.to_string());
    }
    if let Some(t) = obj.get("thinking").and_then(|v| v.as_str()) {
        msg.thinking = Some(t.to_string());
    }
    if let Some(mods) = obj.get("modalities").and_then(|v| v.as_array()) {
        msg.modalities = mods
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(meta) = obj.get("metadata").and_then(|v| v.as_object()) {
        msg.metadata = meta.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    Ok(msg)
}

/// Convenience: a JSON view of a message dict, for tests/diagnostics.
pub fn message_to_json(msg: &AIMessage) -> Value {
    json!({
        "role": msg.role,
        "content": msg.content,
        "name": msg.name,
        "parts": msg.parts,
        "tool_calls": msg.tool_calls.iter().map(|c| json!({
            "id": c.id, "name": c.name, "arguments": c.arguments,
        })).collect::<Vec<_>>(),
        "tool_call_id": msg.tool_call_id,
        "tool_result": msg.tool_result,
        "reasoning": msg.reasoning,
        "thinking": msg.thinking,
        "modalities": msg.modalities,
        "metadata": msg.metadata,
    })
}
