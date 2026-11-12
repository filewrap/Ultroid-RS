//! UltroidAgent — the full agentic loop — port of `pyUltroid/agent/_agent.py`.
//!
//! One instance per bot; histories are per-chat.
//!
//! Contract notes:
//!
//! * History persistence uses the `ultroid_core::db::Database` trait,
//!   which has no TTL write: `HISTORY_TTL` is kept as the documented
//!   retention, but entries are stored without an expiry (the Redis
//!   `SETEX` in Python has no counterpart in the trait).
//! * FNV-1a-64 replaces SHA-256 for the stable tool-call id hash (no
//!   SHA-256 crate in this crate's dependencies); the `call_` + 16
//!   hex-digit shape is unchanged.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, mpsc};
use ultroid_core::db::{Database, db_get_json, db_set_json};

use crate::chatkey::{ChatKeyValue, build_chat_key};
use crate::config::AgentConfig;
use crate::r#loop::{GuardVerdict, LoopGuard, is_failure_result};
use crate::mini::{MiniAI, MiniRespondArgs};
use crate::providers::{
    AIMessage, AIResponse, CompleteOptions, CostTier, ProviderRegistry, ToolCall,
};
use crate::skills::render_skills;
use crate::tools::ToolExecutor;

/// Max agentic loops per request.
pub const MAX_TOOL_ROUNDS: u32 = 5;
/// Messages kept in context window.
pub const MAX_HISTORY: usize = 20;
/// History retention, seconds (24 hours). See the module docs: the
/// database trait cannot enforce it.
pub const HISTORY_TTL: u64 = 86400;
pub const HISTORY_KEY_PFX: &str = "agent:history:";

pub const SYSTEM_PROMPT: &str = r#"You are Ultroid  -  an intelligent, self-aware Telegram userbot AI.
You have full access to the system you run on:

CAPABILITIES:
- shell_execute: Run any shell command on the SarahUB runtime environment
- code_search: Search and read your own live plugin source code
- get_traces: Read your own execution history and error logs
- update_check: Check for and apply updates to yourself
- install_package: Install Python packages on demand
- process_image: Transform images, generate error cards, code images, memes
- send_message: Send Telegram messages
- send_text_file: Send generated UTF-8 .txt/.md content to the current chat only
- get_bot_status: Check your own running state
- remember: Store and recall facts across conversations
- web_search: Search the web for current information
- web_fetch: Read a specific web page after searching
- tg_route_send / tg_read: Send and read Telegram messages through the
  master router; tg_route_send sends plain text unless rich=true is explicit
- plugin_manager: List, load, unload, or reload live plugins (load/unload
  are privileged and may be denied)
- raw_mtproto: Invoke read-only raw MTProto requests from an allowlist
- delegate: Hand a bounded subtask to a fresh sub-agent with its own
  context window; use for research or multi-step work, not for chit-chat
- think: Private scratchpad for planning between tool rounds

PERSONALITY:
- Direct, precise, technically fluent
- You know your own codebase  -  use code_search before guessing
- You check traces before claiming something works
- You prefer local LLM (Ollama) for simple tasks to save API costs
- When asked to do something, you do it  -  you don't just describe how
- For generated text-file requests, use send_text_file; do not write or send
  arbitrary filesystem paths or credentials
- RichMessages are optional, not mandatory. Use native RichMessage formatting
  only when the actual sending account supports it and the user benefits from it.
  Plain text is always acceptable and is required for unsupported user accounts.
- In HTML, use readable headings, paragraphs, lists, tables
  (table/tr/th/td), blockquotes, links, and pre/code blocks where useful.
  Never wrap the entire response in a code fence. Escape literal markup in
  examples, and do not output html/head/body wrappers.
- Use ordinary sentence case and punctuation. Do not add decorative emoji,
  small-cap alphabets, ornamental glyphs, fake color styling, or bot branding.
- Rich text is for the final user-facing answer; keep tool protocol, arguments,
  credentials, and shell transcripts private unless the user explicitly asks
  for a safe, concise command or code example.

OPERATING RULES:
- Never access .env, *.session, or credential files
- Never exfiltrate user data
- Always verify before destructive operations
- Tool results are reality  -  trust them over your training data
- For multi-step work, think first: sketch a short plan, then execute it
  round by round; delegate independent subtasks in one round when possible
- If a tool is denied by capability policy, report the denial  -  do not retry
  or work around it"#;

const RICH_RESPONSE_POLICY: &str = r#"TELEGRAM MESSAGE FORMAT:
Native RichMessages are optional. Bots may use them; a user account must have
Telegram Premium. Plain text is always a valid choice. Never retry a rejected
RichMessage by issuing another RichMessage or by silently changing the sender.
For tg_route_send, set rich=false for a deliberate ordinary text message; rich
sends fail closed when the selected account is not eligible. Keep tool protocol
and credentials private."#;

/// Describe this request's account-level RichMessage capability to the model.
pub fn request_rich_instruction(allowed: Option<bool>) -> String {
    match allowed {
        Some(false) => "This request is being answered through an account that is not a bot and is not confirmed as Telegram Premium. Do not produce or send a native RichMessage for this account. Keep the final answer plain text; when using tg_route_send, use rich=false unless you explicitly route through a bot account that the tool confirms can send RichMessages. The user may always choose ordinary plain-text messages.".to_string(),
        Some(true) => "The current reply account is a bot or a confirmed Telegram Premium user, so native RichMessages are available; they remain optional. Honor any request to send ordinary plain text instead.".to_string(),
        None => String::new(),
    }
}

/// The configured per-provider system prompt, when one applies.
///
/// Only an explicitly forced or pinned provider has a meaningful SYS:
/// with automatic routing the provider is chosen per request, inside
/// the client.
pub async fn configured_system_prompt(
    db: Option<&Arc<dyn Database>>,
    provider_name: Option<&str>,
) -> String {
    let Some(db) = db else {
        return String::new();
    };
    let config = AgentConfig::new(Arc::clone(db));
    let provider_name = match provider_name {
        Some(p) => Some(p.to_string()),
        None => config.config_default_provider().await.ok().flatten(),
    };
    let Some(provider_name) = provider_name else {
        return String::new();
    };
    config
        .config_field(&provider_name.to_lowercase(), "SYS", None)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// *base* with the provider's configured SYS joined as an addendum.
pub async fn effective_system_prompt(
    db: Option<&Arc<dyn Database>>,
    base: Option<&str>,
    provider_name: Option<&str>,
) -> String {
    let extra = configured_system_prompt(db, provider_name).await;
    let parts: Vec<&str> = [base.unwrap_or(""), &extra, RICH_RESPONSE_POLICY]
        .into_iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    parts.join("\n\n")
}

// ── Conversation history ──────────────────────────────────────────────────

/// Per-chat message history, database-backed.
pub struct ConversationHistory {
    pub chat_id: String,
    key: String,
    db: Option<Arc<dyn Database>>,
    local: Vec<AIMessage>,
    loaded: bool,
}

fn message_to_history_json(m: &AIMessage) -> Value {
    json!({
        "role": m.role,
        "content": m.content,
        "name": m.name,
        "tool_calls": m.tool_calls.iter().map(|c| json!({
            "id": c.id, "name": c.name, "arguments": c.arguments,
        })).collect::<Vec<_>>(),
        "tool_call_id": m.tool_call_id,
        "tool_result": m.tool_result,
        "metadata": m.metadata,
    })
}

fn message_from_history_json(d: &Value) -> AIMessage {
    let obj = match d.as_object() {
        Some(o) => o,
        None => return AIMessage::new("user", ""),
    };
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
        _ => {
            let mut m = AIMessage::new(role, "");
            m.content = content.clone();
            m
        }
    };
    msg.content = content;
    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        msg.name = Some(name.to_string());
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
    if let Some(meta) = obj.get("metadata").and_then(|v| v.as_object()) {
        msg.metadata = meta.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    msg
}

impl ConversationHistory {
    pub fn new(chat_id: impl Into<String>, db: Option<Arc<dyn Database>>) -> Self {
        let chat_id = chat_id.into();
        let key = format!("{HISTORY_KEY_PFX}{chat_id}");
        ConversationHistory {
            chat_id,
            key,
            db,
            local: Vec::new(),
            loaded: false,
        }
    }

    pub async fn add(&mut self, role: &str, content: &str) {
        self.add_message(AIMessage::new(role, content)).await;
    }

    /// Append and persist a full message, including tool-call metadata.
    pub async fn add_message(&mut self, message: AIMessage) {
        self.local.push(message);
        self.persist().await;
    }

    pub async fn get(&mut self, max_messages: usize) -> Vec<AIMessage> {
        self.load().await;
        let start = self.local.len().saturating_sub(max_messages);
        self.local[start..].to_vec()
    }

    pub async fn clear(&mut self) {
        self.local.clear();
        self.loaded = true;
        if let Some(db) = &self.db {
            let _ = db.del(&self.key).await;
        }
    }

    async fn persist(&self) {
        let Some(db) = &self.db else {
            return;
        };
        let start = self.local.len().saturating_sub(MAX_HISTORY);
        let data: Vec<Value> = self.local[start..]
            .iter()
            .map(message_to_history_json)
            .collect();
        let _ = db_set_json(db.as_ref(), &self.key, &data).await;
    }

    async fn load(&mut self) {
        if self.loaded || !self.local.is_empty() {
            return;
        }
        self.loaded = true;
        let Some(db) = &self.db else {
            return;
        };
        let raw: Option<Vec<Value>> = db_get_json(db.as_ref(), &self.key).await.ok().flatten();
        if let Some(raw) = raw {
            self.local = raw.iter().map(message_from_history_json).collect();
        }
    }
}

// ── AgentResponse ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AgentResponse {
    pub content: String,
    pub provider: String,
    pub model: String,
    pub tool_rounds: u32,
    pub tools_used: Vec<String>,
    pub duration_ms: f64,
    pub error: Option<String>,
    /// Set if `process_image` was called.
    pub image_result: Option<Value>,
    pub reasoning: Option<String>,
    pub thinking: Option<String>,
    pub modalities: Vec<String>,
}

impl AgentResponse {
    pub fn new(
        content: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        AgentResponse {
            content: content.into(),
            provider: provider.into(),
            model: model.into(),
            tool_rounds: 0,
            tools_used: Vec::new(),
            duration_ms: 0.0,
            error: None,
            image_result: None,
            reasoning: None,
            thinking: None,
            modalities: Vec::new(),
        }
    }

    pub fn ok(&self) -> bool {
        self.error.is_none()
    }
}

impl std::fmt::Display for AgentResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.content)
    }
}

// ── arguments ─────────────────────────────────────────────────────────────

/// Arguments to [`UltroidAgent::respond`], mirroring its Python kwargs.
#[derive(Debug, Clone)]
pub struct RespondArgs {
    pub user_message: String,
    /// Canonical chat key string (numeric id or `dm:<user>:<anchor>`).
    pub chat_id: String,
    pub provider_name: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub use_tools: bool,
    pub use_history: bool,
    pub use_cache: bool,
    pub model: Option<String>,
    pub tool_call: Option<Value>,
    pub max_rounds: Option<u32>,
    pub scope: String,
    pub caps: Option<HashSet<String>>,
    pub rich_messages_allowed: Option<bool>,
    pub depth: u32,
}

impl Default for RespondArgs {
    fn default() -> Self {
        RespondArgs {
            user_message: String::new(),
            chat_id: "0".to_string(),
            provider_name: None,
            max_tokens: 2048,
            temperature: 0.7,
            use_tools: true,
            use_history: true,
            use_cache: true,
            model: None,
            tool_call: None,
            max_rounds: None,
            scope: String::new(),
            caps: None,
            rich_messages_allowed: None,
            depth: 0,
        }
    }
}

/// Arguments to [`UltroidAgent::stream`].
#[derive(Debug, Clone)]
pub struct StreamArgs {
    pub user_message: String,
    pub chat_id: String,
    pub provider_name: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub model: Option<String>,
    pub rich_messages_allowed: Option<bool>,
}

impl Default for StreamArgs {
    fn default() -> Self {
        StreamArgs {
            user_message: String::new(),
            chat_id: "0".to_string(),
            provider_name: None,
            max_tokens: 2048,
            temperature: 0.7,
            model: None,
            rich_messages_allowed: None,
        }
    }
}

// ── UltroidAgent ──────────────────────────────────────────────────────────

/// The full agentic brain of Ultroid. One instance per bot — histories
/// are per-chat.
pub struct UltroidAgent {
    db: Option<Arc<dyn Database>>,
    data_dir: PathBuf,
    mini: MiniAI,
    histories: Mutex<HashMap<String, Arc<Mutex<ConversationHistory>>>>,
    history_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    chat_systems: Mutex<Option<HashMap<String, String>>>,
    system: RwLock<String>,
}

fn fnv_hex(data: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Parse a tool-call `arguments` value into an object, mirroring the
/// Python `json.loads` + `isinstance(arguments, dict)` guard.
fn parsed_args(arguments: &Value) -> Value {
    let value = crate::providers::tool_arguments(arguments);
    match value {
        Value::Object(_) => value,
        _ => json!({}),
    }
}

impl UltroidAgent {
    /// *mini* is an injected `MiniAI`; *provider* is an injected
    /// provider/client for it. Never mutate a shared instance when an
    /// explicit provider is given: separate agents may run concurrently.
    ///
    /// When both *mini* and *provider* are given, the provider wins
    /// (a fresh `MiniAI` is built around it), exactly as in Python.
    pub fn new(
        db: Option<Arc<dyn Database>>,
        data_dir: impl Into<PathBuf>,
        mini: Option<MiniAI>,
        provider: Option<Arc<dyn crate::providers::Provider>>,
    ) -> Self {
        let mini = match (mini, provider) {
            (Some(_), Some(p)) => MiniAI::new(db.clone(), Some(p)),
            (Some(m), None) => m,
            (None, p) => MiniAI::new(db.clone(), p),
        };
        UltroidAgent {
            db,
            data_dir: data_dir.into(),
            mini,
            histories: Mutex::new(HashMap::new()),
            history_locks: Mutex::new(HashMap::new()),
            chat_systems: Mutex::new(None),
            system: RwLock::new(SYSTEM_PROMPT.to_string()),
        }
    }

    /// Set an instance-local MiniAI provider/client.
    pub fn set_provider(&self, provider: Option<Arc<dyn crate::providers::Provider>>) {
        self.mini.mini_set_provider(provider);
    }

    /// The injected or default MiniAI routing facade.
    pub fn mini(&self) -> &MiniAI {
        &self.mini
    }

    // ── History management ──

    pub async fn history(&self, chat_id: &str) -> Arc<Mutex<ConversationHistory>> {
        let mut histories = self.histories.lock().await;
        histories
            .entry(chat_id.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(ConversationHistory::new(
                    chat_id,
                    self.db.clone(),
                )))
            })
            .clone()
    }

    pub async fn clear_history(&self, chat_id: &str) {
        // Always construct the history so a persisted-only chat is cleared too.
        let hist = self.history(chat_id).await;
        hist.lock().await.clear().await;
    }

    /// Preserve native IDs or make a stable ID for providers that omit one.
    pub fn tool_call_id(call: &ToolCall, round_num: u32, position: usize) -> String {
        if !call.id.is_empty() {
            return call.id.clone();
        }
        let payload = json!({
            "name": call.name,
            "arguments": call.arguments,
            "round": round_num,
            "position": position,
        });
        let mut text = serde_json::to_string(&payload).unwrap_or_default();
        // Sort keys, mirroring Python `sort_keys=True` (the json! macro
        // above preserves insertion order, so re-serialize sorted).
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", serde_json::to_string(*k).unwrap(), map[*k]))
                .collect();
            text = format!("{{{}}}", parts.join(","));
        }
        format!("call_{}", fnv_hex(&text))
    }

    // ── Core respond ──

    /// Full agentic loop. Returns `AgentResponse` when done.
    ///
    /// *max_rounds* caps the tool loop for this call only; `None` keeps
    /// the module default. It exists so a chat can hold the agent on a
    /// shorter leash (`.airounds`) without changing the global default.
    pub async fn respond(&self, args: RespondArgs) -> AgentResponse {
        let lock = {
            let mut locks = self.history_locks.lock().await;
            locks
                .entry(args.chat_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        self.respond_locked(args).await
    }

    async fn respond_locked(&self, args: RespondArgs) -> AgentResponse {
        let t0 = Instant::now();
        let RespondArgs {
            user_message,
            chat_id,
            provider_name,
            max_tokens,
            temperature,
            use_tools,
            use_history,
            use_cache,
            model,
            tool_call,
            max_rounds,
            scope,
            caps,
            rich_messages_allowed,
            depth,
        } = args;

        let hist = self.history(&chat_id).await;
        let mut executor = ToolExecutor::new(self.data_dir.clone())
            .with_chat_id(chat_id.parse::<i64>().unwrap_or(0))
            .with_depth(depth);
        if let Some(caps) = &caps {
            executor = executor.with_caps(caps.clone());
        }
        let executor = Arc::new(executor);
        let mut guard = LoopGuard::new();
        let mut tools_used: Vec<String> = Vec::new();
        let mut tool_rounds: u32 = 0;
        let mut image_result: Option<Value> = None;
        let mut pending_tool_call = tool_call;
        let mut response_reasoning: Option<String> = None;
        let mut response_thinking: Option<String> = None;
        let mut response_modalities: Vec<String> = Vec::new();

        // Add user message to history.
        if use_history {
            hist.lock().await.add("user", &user_message).await;
        }

        // Build message list.
        let mut messages: Vec<AIMessage> = if use_history {
            hist.lock().await.get(MAX_HISTORY).await
        } else {
            vec![AIMessage::new("user", &user_message)]
        };

        let tools: Option<Vec<Value>> = if use_tools {
            Some(executor.tool_definitions())
        } else {
            None
        };

        // The menu stores a per-provider system prompt (AGENT_*_SYS); it
        // joins the agent prompt as an addendum rather than replacing it.
        let base_system = self.system.read().await.clone();
        let mut system = effective_system_prompt(
            self.db.as_ref(),
            Some(&base_system),
            provider_name.as_deref(),
        )
        .await;
        let rich_instruction = request_rich_instruction(rich_messages_allowed);
        if !rich_instruction.is_empty() {
            system = format!("{system}\n\nTelegram delivery policy:\n{rich_instruction}");
        }
        let chat_prompt = self.chat_system(&chat_id).await.unwrap_or_default();
        if !chat_prompt.is_empty() {
            system = format!("{system}\n\nChat instructions:\n{chat_prompt}");
        }
        let skill_block = render_skills(scope.split(':').next());
        if !skill_block.is_empty() {
            system = format!("{system}\n\n{skill_block}");
        }

        // ── Agentic loop ──
        let rounds_cap = max_rounds.unwrap_or(MAX_TOOL_ROUNDS).min(MAX_TOOL_ROUNDS);
        let scope_prefix = format!(
            "{chat_id}:{}:{}:{scope}",
            provider_name.as_deref().unwrap_or("auto"),
            model.as_deref().unwrap_or("default")
        );
        let mut last_response: Option<AIResponse> = None;

        for round_num in 0..=rounds_cap {
            let mut mini_args = MiniRespondArgs::new(messages.clone());
            mini_args.system = system.clone();
            mini_args.force_provider = provider_name.clone();
            mini_args.max_tokens = max_tokens;
            mini_args.temperature = temperature;
            mini_args.use_cache = use_cache && !use_tools;
            mini_args.tools = tools.clone();
            mini_args.tool_call = pending_tool_call.take();
            mini_args.model = model.clone();
            mini_args.scope = scope_prefix.clone();
            let response = self.mini.mini_respond(mini_args).await;

            if response.reasoning.is_some() {
                response_reasoning = response.reasoning.clone();
            }
            if response.thinking.is_some() {
                response_thinking = response.thinking.clone();
            }
            if !response.modalities.is_empty() {
                response_modalities = response.modalities.clone();
            }

            if let Some(error) = &response.error {
                return AgentResponse {
                    content: format!("⚠️ {error}"),
                    provider: response.provider.clone(),
                    model: response.model.clone(),
                    tool_rounds,
                    tools_used,
                    duration_ms: t0.elapsed().as_secs_f64() * 1000.0,
                    error: Some(error.clone()),
                    image_result: None,
                    reasoning: response_reasoning,
                    thinking: response_thinking,
                    modalities: response_modalities,
                };
            }

            // No tool calls — we have the final answer.
            if response.tool_calls.is_empty() {
                if use_history {
                    hist.lock().await.add("assistant", &response.content).await;
                }
                return AgentResponse {
                    content: response.content,
                    provider: response.provider,
                    model: response.model,
                    tool_rounds,
                    tools_used,
                    duration_ms: t0.elapsed().as_secs_f64() * 1000.0,
                    error: None,
                    image_result,
                    reasoning: response_reasoning,
                    thinking: response_thinking,
                    modalities: response_modalities,
                };
            }

            // ── Execute tool calls ──
            tool_rounds += 1;

            // Normalize omitted IDs before serializing the assistant call
            // message; both sides of the exchange must use the same value.
            let mut normalized_calls: Vec<ToolCall> = Vec::new();
            for (position, call) in response.tool_calls.iter().enumerate() {
                let mut tc = call.clone();
                tc.id = Self::tool_call_id(call, round_num, position);
                normalized_calls.push(tc);
            }
            let assistant_msg = AIMessage {
                role: "assistant".to_string(),
                content: Value::String(response.content.clone()),
                name: None,
                parts: response.parts.clone(),
                tool_calls: normalized_calls.clone(),
                tool_call_id: None,
                tool_result: None,
                reasoning: response.reasoning.clone(),
                thinking: response.thinking.clone(),
                modalities: Vec::new(),
                metadata: response.metadata.clone(),
            };
            if use_history {
                hist.lock().await.add_message(assistant_msg.clone()).await;
            }
            // Preserve the normalized tool-call payload. Provider
            // serializers use it to recreate native assistant
            // tool-use/function-call blocks.
            messages.push(assistant_msg);

            // Parse every argument payload first, then execute the calls
            // of this round concurrently: tool calls in one round are
            // independent by construction (the model chose them together),
            // and parallel execution matches how production agent runtimes
            // (OpenAI Agents SDK, Pydantic AI) run local tools.
            let parsed: Vec<Value> = normalized_calls
                .iter()
                .map(|tc| parsed_args(&tc.arguments))
                .collect();

            // One guard verdict per call, decided before dispatch. A
            // blocked call never runs: it becomes a synthetic result the
            // model reads, so a model stuck in a retry loop is told why
            // instead of silently losing the result it asked for.
            let verdicts: Vec<GuardVerdict> = normalized_calls
                .iter()
                .zip(parsed.iter())
                .map(|(tc, args)| guard.before_call(&tc.name, args))
                .collect();

            let futures: Vec<_> = normalized_calls
                .iter()
                .zip(parsed.iter())
                .zip(verdicts.iter())
                .map(|((tc, _args), verdict)| {
                    let executor = Arc::clone(&executor);
                    let tc = tc.clone();
                    let verdict = verdict.clone();
                    async move {
                        if verdict.blocked() {
                            tracing::warn!(
                                "[AGENT:GUARD] blocked {} ({}) after {} occurrence(s)",
                                tc.name,
                                verdict.code,
                                verdict.count
                            );
                            let result = verdict.as_result();
                            return (tc, verdict, None, result);
                        }
                        match executor.execute(&tc).await {
                            Ok(result) => {
                                let value: Value = serde_json::from_str(&result.output)
                                    .unwrap_or_else(|_| Value::String(result.output.clone()));
                                (tc, verdict, Some(result), value)
                            }
                            Err(e) => (tc, verdict, None, json!({"error": e.to_string()})),
                        }
                    }
                })
                .collect();
            let results = futures::future::join_all(futures).await;

            let mut round_results: Vec<Value> = Vec::new();
            for (tc, _verdict, tool_result, value) in &results {
                if tool_result.is_some() {
                    guard.observe(&tc.name, &parsed_args(&tc.arguments), value);
                }
                round_results.push(value.clone());
            }

            let round_progressed =
                results
                    .iter()
                    .any(|(_, _, tool_result, value)| match tool_result {
                        Some(r) => !is_failure_result(r.is_error, value),
                        None => !is_failure_result(false, value),
                    });
            guard.begin_round(round_progressed);

            // A halt has to actually end the turn, and it must still
            // produce an answer: the model is told why the tools stopped
            // working rather than being cut off mid-sentence.
            if verdicts.iter().any(|v| v.halt()) {
                let code = verdicts
                    .iter()
                    .find(|v| v.halt())
                    .map(|v| v.code.clone())
                    .unwrap_or_default();
                guard.halt(&code);
            }
            if guard.halted() || guard.stalled() {
                tracing::warn!(
                    "[AGENT:GUARD] halting turn after round {}: {}",
                    round_num + 1,
                    guard.halted_reason
                );
                let halt_nudge = AIMessage::new(
                    "user",
                    &format!(
                        "The tools are no longer running ({}). Do not call any tool again. Summarise what you found, or say plainly what you could not do and why.",
                        if guard.halted_reason.is_empty() {
                            "the loop stopped making progress"
                        } else {
                            &guard.halted_reason
                        }
                    ),
                );
                messages.push(halt_nudge.clone());
                if use_history {
                    hist.lock().await.add_message(halt_nudge).await;
                }
                last_response = Some(response);
                break;
            }

            // Append each result with the provider-issued id retained on
            // the matching call. OpenAI requires tool_call_id; Anthropic
            // requires tool_use_id; Gemini uses the function name in a
            // response part.
            for (tc, mut result_value) in normalized_calls.iter().zip(round_results.into_iter()) {
                let tc_id = tc.id.clone();
                let tool_name = tc.name.clone();
                tools_used.push(tool_name.clone());
                tracing::info!("[AGENT:AGENT] Round {}: {}", round_num + 1, tool_name);

                // Extract image result for sending after response.
                if tool_name == "process_image" {
                    if let Some(obj) = result_value.as_object_mut() {
                        if let Some(image) = obj.remove("_image_result") {
                            image_result = Some(image);
                        }
                    }
                }

                let result_str_full = serde_json::to_string(&result_value).unwrap_or_default();
                let result_str: String = result_str_full.chars().take(3000).collect();
                let tool_msg = AIMessage {
                    role: "tool".to_string(),
                    content: Value::String(result_str.clone()),
                    name: Some(tool_name.clone()),
                    parts: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tc_id),
                    tool_result: Some(Value::String(result_str)),
                    modalities: Vec::new(),
                    metadata: {
                        let mut meta = HashMap::new();
                        meta.insert(
                            "tool_call".to_string(),
                            json!({"id": tc.id, "name": tc.name, "arguments": tc.arguments}),
                        );
                        meta
                    },
                    reasoning: None,
                    thinking: None,
                };
                messages.push(tool_msg.clone());
                if use_history {
                    hist.lock().await.add_message(tool_msg).await;
                }
            }

            if tool_rounds >= rounds_cap {
                // Force final answer. This has to use the effective cap:
                // with a lower per-call cap the loop ends before the
                // global constant is ever reached, and the run would
                // return the model's empty tool-call turn instead of an
                // answer.
                let final_nudge = AIMessage::new("user", "Please provide your final response now.");
                messages.push(final_nudge.clone());
                if use_history {
                    hist.lock().await.add_message(final_nudge).await;
                }
            }

            last_response = Some(response);
        }

        // Shouldn't reach here — but return what we have.
        match last_response {
            Some(response) => AgentResponse {
                content: response.content,
                provider: response.provider,
                model: response.model,
                tool_rounds,
                tools_used,
                duration_ms: t0.elapsed().as_secs_f64() * 1000.0,
                error: None,
                image_result,
                reasoning: response_reasoning,
                thinking: response_thinking,
                modalities: response_modalities,
            },
            None => AgentResponse {
                content: "(no response)".to_string(),
                provider: "unknown".to_string(),
                model: "unknown".to_string(),
                tool_rounds,
                tools_used,
                duration_ms: t0.elapsed().as_secs_f64() * 1000.0,
                error: None,
                image_result,
                reasoning: response_reasoning,
                thinking: response_thinking,
                modalities: response_modalities,
            },
        }
    }

    // ── Streaming respond ──

    /// Stream the agent's response. Yields text chunks over the
    /// returned channel. No tool calls in stream mode — use
    /// `respond()` for tools.
    ///
    /// Takes `&Arc<Self>` because the stream is produced by a spawned
    /// task that must own the agent.
    pub fn stream(self: &Arc<Self>, args: StreamArgs) -> mpsc::Receiver<String> {
        let this = Arc::clone(self);
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            this.stream_inner(args, tx).await;
        });
        rx
    }

    async fn stream_inner(&self, args: StreamArgs, tx: mpsc::Sender<String>) {
        let StreamArgs {
            user_message,
            chat_id,
            provider_name,
            max_tokens,
            temperature,
            model,
            rich_messages_allowed,
        } = args;
        let hist = self.history(&chat_id).await;
        hist.lock().await.add("user", &user_message).await;
        let messages = hist.lock().await.get(MAX_HISTORY).await;
        let base_system = self.system.read().await.clone();
        let mut system = effective_system_prompt(
            self.db.as_ref(),
            Some(&base_system),
            provider_name.as_deref(),
        )
        .await;
        let rich_instruction = request_rich_instruction(rich_messages_allowed);
        if !rich_instruction.is_empty() {
            system = format!("{system}\n\nTelegram delivery policy:\n{rich_instruction}");
        }
        let skill_block = render_skills(provider_name.as_deref());
        if !skill_block.is_empty() {
            system = format!("{system}\n\n{skill_block}");
        }

        // Pick best streaming provider. Without a database there is no
        // registry, hence no provider at all.
        let Some(db) = self.db.clone() else {
            let _ = tx.send("(no provider configured)".to_string()).await;
            return;
        };
        let registry = ProviderRegistry::new(AgentConfig::new(db));
        let provider = match &provider_name {
            Some(name) => match registry.provider(name) {
                Err(_) => {
                    let _ = tx.send(format!("⚠️ Unknown provider `{name}`.")).await;
                    return;
                }
                Ok(p) if !p.is_configured() => {
                    let _ = tx
                        .send(format!("⚠️ Provider `{name}` is not configured."))
                        .await;
                    return;
                }
                Ok(p) => Some(p),
            },
            None => {
                // Prefer local first.
                let mut found = None;
                for tier in [
                    CostTier::Free,
                    CostTier::Cheap,
                    CostTier::Medium,
                    CostTier::Premium,
                ] {
                    let providers: Vec<_> = registry
                        .all()
                        .into_iter()
                        .filter(|p| p.is_configured() && p.cost_tier() == tier)
                        .collect();
                    if !providers.is_empty() {
                        found = Some(providers[0].clone());
                        break;
                    }
                }
                found
            }
        };
        let Some(provider) = provider else {
            let _ = tx.send("(no provider configured)".to_string()).await;
            return;
        };

        let opts = CompleteOptions {
            model,
            tools: None,
            max_tokens,
            temperature,
            system: Some(system),
        };
        let mut full = String::new();
        match provider.stream(&messages, &opts).await {
            Ok(mut stream) => {
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => {
                            full.push_str(&chunk);
                            if tx.send(chunk).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(format!("\n[stream error: {e}]")).await;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(format!("\n[stream error: {e}]")).await;
            }
        }

        if !full.is_empty() {
            hist.lock().await.add("assistant", &full).await;
        }
    }

    // ── System prompt customization ──

    /// Set a persisted system addendum for one chat.
    ///
    /// `chat_id` may be a numeric Telegram id (groups, private chats)
    /// or a DM session key like `dm:<user>:<anchor>`; both are
    /// normalized through [`ChatKey`] so the same map can hold both
    /// without colliding. The previous implementation coerced to `int`
    /// and crashed on the string form.
    pub async fn set_chat_system(
        &self,
        chat_id: &str,
        prompt: &str,
    ) -> Result<(), ultroid_core::error::Error> {
        let key = build_chat_key(ChatKeyValue::Str(chat_id.to_string()))?.value();
        self.ensure_chat_systems().await;
        let mut guard = self.chat_systems.lock().await;
        if let Some(map) = guard.as_mut() {
            map.insert(key, prompt.to_string());
        }
        drop(guard);
        self.save_chat_systems().await;
        Ok(())
    }

    /// Remove the persisted system addendum for one chat, if any.
    pub async fn reset_chat_system(&self, chat_id: &str) -> Result<(), ultroid_core::error::Error> {
        let key = build_chat_key(ChatKeyValue::Str(chat_id.to_string()))?.value();
        self.ensure_chat_systems().await;
        let mut guard = self.chat_systems.lock().await;
        if let Some(map) = guard.as_mut() {
            map.remove(&key);
        }
        drop(guard);
        self.save_chat_systems().await;
        Ok(())
    }

    async fn ensure_chat_systems(&self) {
        let mut guard = self.chat_systems.lock().await;
        if guard.is_none() {
            *guard = Some(self.load_chat_systems().await);
        }
    }

    async fn load_chat_systems(&self) -> HashMap<String, String> {
        let Some(db) = &self.db else {
            return HashMap::new();
        };
        let raw: Option<HashMap<String, String>> = db_get_json(db.as_ref(), "AGENT_CHAT_SYSTEMS")
            .await
            .ok()
            .flatten();
        raw.map(|m| m.into_iter().filter(|(_, v)| !v.is_empty()).collect())
            .unwrap_or_default()
    }

    async fn save_chat_systems(&self) {
        let Some(db) = &self.db else {
            return;
        };
        let guard = self.chat_systems.lock().await;
        let map = guard.clone().unwrap_or_default();
        let _ = db_set_json(db.as_ref(), "AGENT_CHAT_SYSTEMS", &map).await;
    }

    /// Return the system addendum for *chat_id*, or `""` if none.
    ///
    /// Accepts any `normalize_chat_id` input; the key is resolved once
    /// and all lookup paths converge on the canonical string form, so a
    /// DM session key and a numeric group id never share a slot.
    pub async fn chat_system(&self, chat_id: &str) -> Result<String, ultroid_core::error::Error> {
        let key = build_chat_key(ChatKeyValue::Str(chat_id.to_string()))?.value();
        self.ensure_chat_systems().await;
        let guard = self.chat_systems.lock().await;
        Ok(guard
            .as_ref()
            .and_then(|m| m.get(&key).cloned())
            .unwrap_or_default())
    }

    pub async fn set_system(&self, prompt: &str) {
        *self.system.write().await = prompt.to_string();
    }

    pub async fn append_system(&self, extra: &str) {
        let mut system = self.system.write().await;
        system.push_str(&format!("\n\n{extra}"));
    }

    pub async fn reset_system(&self) {
        *self.system.write().await = SYSTEM_PROMPT.to_string();
    }

    // ── Status ──

    pub async fn status(&self) -> Value {
        let providers: HashMap<String, bool> = match self.db.clone() {
            Some(db) => {
                let registry = ProviderRegistry::new(AgentConfig::new(db));
                registry
                    .all()
                    .iter()
                    .map(|p| (p.name().to_string(), p.is_configured()))
                    .collect()
            }
            None => HashMap::new(),
        };
        let cache_size = self.mini.mini_cache_size().await;
        let configured: Vec<String> = providers
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.clone())
            .collect();
        let active_histories = self.histories.lock().await.len();
        json!({
            "providers": providers,
            "configured_count": configured.len(),
            "configured": configured,
            "cache_entries": cache_size,
            "active_histories": active_histories,
            "max_tool_rounds": MAX_TOOL_ROUNDS,
            "max_history": MAX_HISTORY,
        })
    }
}
