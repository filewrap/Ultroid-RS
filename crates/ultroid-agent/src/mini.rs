//! MiniAI — the lightweight layer that sits in front of the full LLM —
//! port of `pyUltroid/agent/_mini.py`.
//!
//! Sits in front of the full agent: handles cheap/simple requests
//! without touching premium APIs.
//!
//! Two deliberate divergences from the Python source:
//!
//! * `CompleteOptions` (sibling) has no `tool_call` slot, so the
//!   `tool_call` continuation object a caller passes to
//!   `mini_respond` is honoured for cache/dedup bypass exactly as in
//!   Python, but is not forwarded to the provider — the continuation
//!   must ride in `messages`, which is also what the provider
//!   serializers replay.
//! * Cache keys use FNV-1a-64 hex where Python uses MD5 (no MD5 crate
//!   in this crate's dependencies). Keys are Rust-side only.
//!
//! The registry (`crate::providers::ProviderRegistry`) needs an
//! `AgentConfig`, which needs a database. When this `MiniAI` was built
//! without one, routing reports "not configured" instead of inventing
//! providers — the same fail-closed shape as the Python code running
//! before startup has built the database.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use ultroid_core::db::{Database, db_get_json, db_set_json};

use crate::config::AgentConfig;
use crate::providers::{
    AIMessage, AIResponse, CompleteOptions, CostTier, Provider, ProviderRegistry,
};

// ── Intent ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    Greeting,
    SimpleFact,
    CodeHelp,
    ImageRequest,
    ShellRequest,
    UpdateRequest,
    Search,
    Creative,
    Analysis,
    System,
    Unknown,
}

/// Ordered exactly as `_INTENT_PATTERNS` in `_mini.py`: first match wins.
const INTENT_PATTERNS: &[(Intent, &[&str])] = &[
    (
        Intent::Greeting,
        &[r"^(hi|hello|hey|sup|yo|howdy|greetings)\b"],
    ),
    (
        Intent::CodeHelp,
        [
            r"\b(code|debug|fix|error|function|script|python|bash|class|import)\b",
            r"```",
            r"def |class |import ",
        ]
        .as_slice(),
    ),
    (
        Intent::ImageRequest,
        &[r"\b(image|picture|photo|draw|paint|generate.*image|art|meme|sticker)\b"],
    ),
    (
        Intent::ShellRequest,
        &[r"\b(run|execute|terminal|shell|command|bash|sh |sudo)\b"],
    ),
    (
        Intent::UpdateRequest,
        &[r"\b(update|upgrade|pull|latest|new version)\b"],
    ),
    (
        Intent::Search,
        &[r"\b(search|find|look up|google|what is|who is|when did|where is)\b"],
    ),
    (
        Intent::Creative,
        &[r"\b(write|poem|story|caption|lyrics|compose|create a)\b"],
    ),
    (
        Intent::Analysis,
        &[r"\b(analyse|analyze|explain|summarise|summarize|describe|review)\b"],
    ),
    (
        Intent::System,
        &[r"\b(status|stats|trace|uptime|plugins|version|bot info)\b"],
    ),
    (
        Intent::SimpleFact,
        &[r"\b(what|who|when|where|how many|define|meaning of)\b"],
    ),
];

fn intent_patterns() -> &'static Vec<(Intent, Vec<Regex>)> {
    static PATTERNS: std::sync::OnceLock<Vec<(Intent, Vec<Regex>)>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        INTENT_PATTERNS
            .iter()
            .map(|(intent, pats)| {
                (
                    *intent,
                    pats.iter()
                        .map(|p| Regex::new(p).expect("static intent regex"))
                        .collect(),
                )
            })
            .collect()
    })
}

pub fn classify_intent(text: &str) -> Intent {
    let lower = text.to_lowercase();
    let text_lower = lower.trim();
    for (intent, patterns) in intent_patterns() {
        for p in patterns {
            if p.is_match(text_lower) {
                return *intent;
            }
        }
    }
    Intent::Unknown
}

fn builtin_response(intent: Intent) -> Option<&'static str> {
    match intent {
        Intent::Greeting => Some("Hey! I'm Ultroid  -  fully operational. What do you need?"),
        _ => None,
    }
}

fn fnv_hex(data: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn cache_key(messages: &[AIMessage], system: &str, scope: &str) -> String {
    let tail: Vec<String> = messages
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|m| format!("{}:{}", m.role, m.text()))
        .collect();
    let content = format!("{scope}|{system}|{}", tail.join("|"));
    format!("BRAIN_CACHE_{}", fnv_hex(&content))
}

fn cost_tiers_in_order() -> [CostTier; 4] {
    [
        CostTier::Free,
        CostTier::Cheap,
        CostTier::Medium,
        CostTier::Premium,
    ]
}

fn error_response(provider: &str, model: &str, error: String) -> AIResponse {
    let mut r = AIResponse::new(provider, model);
    r.error = Some(error);
    r
}

// ── MiniAI ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct CacheRecord {
    exp: f64,
    v: String,
}

/// Arguments to [`MiniAI::mini_respond`], mirroring its Python kwargs.
#[derive(Debug, Clone, Default)]
pub struct MiniRespondArgs {
    pub messages: Vec<AIMessage>,
    pub system: String,
    pub force_provider: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub use_cache: bool,
    pub tools: Option<Vec<Value>>,
    pub tool_call: Option<Value>,
    pub model: Option<String>,
    pub scope: String,
}

impl MiniRespondArgs {
    pub fn new(messages: Vec<AIMessage>) -> Self {
        MiniRespondArgs {
            messages,
            max_tokens: 1024,
            temperature: 0.7,
            use_cache: true,
            ..Default::default()
        }
    }
}

/// One in-flight request, mirroring `asyncio.Event`: `wait()` returns
/// immediately once `finish()` has run, even if the waiter arrived after
/// the notification. (`tokio::sync::Notify::notify_waiters` alone does
/// not persist the signal, so a bare `Notify` would strand a waiter
/// that arrived between the finish and the wait.)
struct FlightState {
    done: Mutex<bool>,
    notify: tokio::sync::Notify,
}

impl FlightState {
    fn new() -> Self {
        FlightState {
            done: Mutex::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    async fn wait(&self) {
        if *self.done.lock().await {
            return;
        }
        let notified = self.notify.notified();
        tokio::pin!(notified);
        // Register first, then re-check: this closes the race where the
        // owner finishes between the first check and the wait.
        if notified.as_mut().enable() {
            return;
        }
        if *self.done.lock().await {
            return;
        }
        notified.await;
    }

    async fn finish(&self) {
        *self.done.lock().await = true;
        self.notify.notify_waiters();
    }
}

pub struct MiniAI {
    db: Option<Arc<dyn Database>>,
    provider: std::sync::RwLock<Option<Arc<dyn Provider>>>,
    in_flight: Arc<Mutex<HashMap<String, Arc<FlightState>>>>,
    in_flight_results: Arc<Mutex<HashMap<String, AIResponse>>>,
}

impl MiniAI {
    pub const CACHE_TTL: f64 = 3600.0;
    pub const CACHE_MAX_LEN: usize = 500;
    pub const CACHE_KEY_PFX: &'static str = "BRAIN_CACHE_";

    pub fn new(db: Option<Arc<dyn Database>>, provider: Option<Arc<dyn Provider>>) -> Self {
        MiniAI {
            db,
            provider: std::sync::RwLock::new(provider),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            in_flight_results: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Use *provider* for requests, or restore registry routing.
    pub fn mini_set_provider(&self, provider: Option<Arc<dyn Provider>>) {
        if let Ok(mut slot) = self.provider.write() {
            *slot = provider;
        }
    }

    fn injected_provider(&self) -> Option<Arc<dyn Provider>> {
        self.provider.read().ok().and_then(|g| g.clone())
    }

    /// The provider registry, or `None` when there is no database for
    /// its `AgentConfig` (see the module docs).
    fn registry(&self) -> Option<ProviderRegistry> {
        self.db
            .as_ref()
            .map(|db| ProviderRegistry::new(AgentConfig::new(Arc::clone(db))))
    }

    /// Configured providers of one tier, mirroring Python
    /// `providers_by_tier` (configured only).
    fn providers_by_tier(registry: &ProviderRegistry, tier: CostTier) -> Vec<Arc<dyn Provider>> {
        registry
            .all()
            .into_iter()
            .filter(|p| p.is_configured() && p.cost_tier() == tier)
            .collect()
    }

    // ── Cache ──

    async fn cache_get(&self, key: &str) -> Option<String> {
        let db = self.db.as_ref()?;
        let record: CacheRecord = db_get_json(db.as_ref(), key).await.ok().flatten()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        if record.exp < now {
            let _ = db.del(key).await;
            return None;
        }
        Some(record.v)
    }

    async fn cache_set(&self, key: &str, value: &str) {
        if value.len() > Self::CACHE_MAX_LEN {
            return; // don't cache long creative responses
        }
        let Some(db) = self.db.as_ref() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let record = CacheRecord {
            exp: now + Self::CACHE_TTL,
            v: value.to_string(),
        };
        let _ = db_set_json(db.as_ref(), key, &record).await;
    }

    // ── Core respond ──

    /// Route a conversation to the best available provider.
    /// Returns an `AIResponse` — always populated, never raises.
    pub async fn mini_respond(&self, args: MiniRespondArgs) -> AIResponse {
        let mut use_cache = args.use_cache;
        let last_user = args
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text())
            .unwrap_or_default();

        // A tool-result continuation must always reach the selected provider.
        if args.tool_call.is_some() {
            use_cache = false;
        }

        // A pinned/forced provider must not be bypassed by a zero-cost
        // classifier shortcut; selection is part of the requested contract.
        if let Some(force) = &args.force_provider {
            let Some(registry) = self.registry() else {
                return error_response(
                    force,
                    "none",
                    format!("Provider `{force}` is not configured."),
                );
            };
            let p = match registry.provider(force) {
                Ok(p) => p,
                Err(e) => return error_response(force, "none", e.to_string()),
            };
            if !p.is_configured() {
                return error_response(
                    force,
                    "none",
                    format!("Provider `{force}` is not configured."),
                );
            }
            return self
                .complete_with(
                    &p,
                    &args.messages,
                    args.tools.as_ref(),
                    args.model.as_deref(),
                    args.max_tokens,
                    args.temperature,
                    &args.system,
                )
                .await;
        }

        // 1. Built-in pattern response (zero cost)
        let intent = classify_intent(&last_user);
        if let Some(text) = builtin_response(intent) {
            if args.tools.is_none() && args.tool_call.is_none() {
                let mut r = AIResponse::new("builtin", "pattern");
                r.content = text.to_string();
                return r;
            }
        }

        // 2. Cache check
        let key = cache_key(&args.messages, &args.system, &args.scope);
        if use_cache && args.tools.is_none() && args.tool_call.is_none() {
            if let Some(cached) = self.cache_get(&key).await {
                tracing::debug!(
                    "[BRAIN:MINI] Cache hit for {}",
                    &key[key.len().saturating_sub(8)..]
                );
                let mut r = AIResponse::new("cache", "udb");
                r.content = cached;
                return r;
            }
        }

        // 3. Dedup concurrent identical requests
        let existing = {
            let mut flight = self.in_flight.lock().await;
            match flight.get(&key) {
                Some(state) => Some(Arc::clone(state)),
                None => {
                    let state = Arc::new(FlightState::new());
                    flight.insert(key.clone(), Arc::clone(&state));
                    None
                }
            }
        };
        if let Some(state) = existing {
            tracing::debug!(
                "[BRAIN:MINI] Waiting for in-flight request {}",
                &key[key.len().saturating_sub(8)..]
            );
            state.wait().await;
            if let Some(result) = self.in_flight_results.lock().await.get(&key) {
                return result.clone();
            }
            // The in-flight request left no result (e.g. it was shut
            // down); fall through and run the request ourselves.
            let mut flight = self.in_flight.lock().await;
            flight.insert(key.clone(), Arc::new(FlightState::new()));
        }

        let response = self
            .route(
                &args.messages,
                &args.system,
                intent,
                args.max_tokens,
                args.temperature,
                args.tools.as_ref(),
                args.model.as_deref(),
            )
            .await;

        // Cache only ordinary text exchanges; tool state is provider-native.
        if response.error.is_none() && use_cache && args.tools.is_none() && args.tool_call.is_none()
        {
            self.cache_set(&key, &response.content).await;
        }

        {
            let mut results = self.in_flight_results.lock().await;
            results.insert(key.clone(), response.clone());
        }
        let state = {
            let mut flight = self.in_flight.lock().await;
            flight.remove(&key)
        };
        if let Some(state) = state {
            state.finish().await;
        }
        // Clean result after a small delay so waiters can read it.
        let results_map = self.in_flight_results.clone();
        let key_clone = key.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            results_map.lock().await.remove(&key_clone);
        });

        response
    }

    async fn route(
        &self,
        messages: &[AIMessage],
        system: &str,
        intent: Intent,
        max_tokens: u32,
        temperature: f32,
        tools: Option<&Vec<Value>>,
        model: Option<&str>,
    ) -> AIResponse {
        // An injected provider/client takes precedence over registry routing.
        if let Some(provider) = self.injected_provider() {
            return self
                .complete_with(
                    &provider,
                    messages,
                    tools,
                    model,
                    max_tokens,
                    temperature,
                    system,
                )
                .await;
        }

        // Determine target tier from intent.
        let target_tier = self.intent_to_tier(intent, tools);
        let allow_local = tools.map_or(true, |t| t.is_empty());
        let Some(registry) = self.registry() else {
            return error_response(
                "none",
                "none",
                "All providers failed or unconfigured.".to_string(),
            );
        };
        let attempt_order = Self::build_order(&registry, target_tier, allow_local);

        for provider in &attempt_order {
            tracing::debug!("[BRAIN:MINI] Trying {}…", provider.name());
            let response = self
                .complete_with(
                    provider,
                    messages,
                    tools,
                    model,
                    max_tokens,
                    temperature,
                    system,
                )
                .await;
            if response.error.is_none() {
                tracing::debug!(
                    "[BRAIN:MINI] {} {}/{} ({:.0}ms)",
                    "ok",
                    provider.name(),
                    response.model,
                    response.duration_ms
                );
                return response;
            }
            tracing::warn!(
                "[BRAIN:MINI] {}: {}",
                provider.name(),
                response.error.clone().unwrap_or_default()
            );
        }

        // All failed
        error_response(
            "none",
            "none",
            "All providers failed or unconfigured.".to_string(),
        )
    }

    /// Call a normalized provider exactly once.
    async fn complete_with(
        &self,
        provider: &Arc<dyn Provider>,
        messages: &[AIMessage],
        tools: Option<&Vec<Value>>,
        model: Option<&str>,
        max_tokens: u32,
        temperature: f32,
        system: &str,
    ) -> AIResponse {
        let opts = CompleteOptions {
            model: model.map(|m| m.to_string()),
            tools: tools.cloned(),
            max_tokens,
            temperature,
            system: Some(system.to_string()),
        };
        match provider.complete(messages, &opts).await {
            Ok(r) => r,
            Err(e) => error_response(provider.name(), model.unwrap_or("none"), e.to_string()),
        }
    }

    fn intent_to_tier(&self, intent: Intent, tools: Option<&Vec<Value>>) -> CostTier {
        // Tool calls need capable models.
        if tools.map_or(false, |t| !t.is_empty()) {
            return CostTier::Medium;
        }
        match intent {
            Intent::Greeting | Intent::SimpleFact => CostTier::Free,
            Intent::CodeHelp | Intent::Analysis | Intent::Creative => CostTier::Medium,
            _ => CostTier::Cheap,
        }
    }

    fn build_order(
        registry: &ProviderRegistry,
        min_tier: CostTier,
        allow_local: bool,
    ) -> Vec<Arc<dyn Provider>> {
        let mut ordered: Vec<Arc<dyn Provider>> = Vec::new();
        if allow_local {
            ordered.extend(Self::providers_by_tier(registry, CostTier::Free));
        }
        for tier in cost_tiers_in_order() {
            if tier >= min_tier {
                ordered.extend(Self::providers_by_tier(registry, tier));
            }
        }
        // Dedup by provider name, preserving order (FREE providers reappear
        // in the tier sweep when min_tier == FREE).
        let mut seen = std::collections::HashSet::new();
        let mut unique = Vec::new();
        for provider in ordered {
            if seen.insert(provider.name().to_string()) {
                unique.push(provider);
            }
        }
        unique
    }

    // ── Convenience: single prompt ────────────────────────────────────────

    /// Single-turn prompt returning text while retaining tool-call support.
    pub async fn mini_ask(
        &self,
        prompt: &str,
        system: &str,
        provider: Option<&str>,
        tools: Option<Vec<Value>>,
        tool_call: Option<Value>,
        model: Option<&str>,
    ) -> String {
        let messages = vec![AIMessage::new("user", prompt)];
        let mut args = MiniRespondArgs::new(messages);
        args.system = system.to_string();
        args.force_provider = provider.map(|p| p.to_string());
        args.tools = tools;
        args.tool_call = tool_call;
        args.model = model.map(|m| m.to_string());
        let resp = self.mini_respond(args).await;
        if !resp.content.is_empty() {
            resp.content
        } else {
            format!("[Error: {}]", resp.error.unwrap_or_default())
        }
    }

    // ── Stats ──

    pub fn mini_provider_status(&self) -> HashMap<String, bool> {
        match self.registry() {
            Some(registry) => registry
                .all()
                .iter()
                .map(|p| (p.name().to_string(), p.is_configured()))
                .collect(),
            None => HashMap::new(),
        }
    }

    pub async fn mini_cache_size(&self) -> usize {
        let Some(db) = self.db.as_ref() else {
            return 0;
        };
        db.keys(Self::CACHE_KEY_PFX)
            .await
            .map(|k| k.len())
            .unwrap_or(0)
    }

    pub async fn mini_flush_cache(&self) -> usize {
        let Some(db) = self.db.as_ref() else {
            return 0;
        };
        let Ok(keys) = db.keys(Self::CACHE_KEY_PFX).await else {
            return 0;
        };
        let mut count = 0;
        for key in keys {
            if db.del(&key).await.is_ok() {
                count += 1;
            }
        }
        count
    }

    // ── Lifecycle support ─────────────────────────────────────────────────

    /// Stamp every in-flight request with the shutdown response and wake
    /// its waiters. Returns how many requests were stamped. Used by
    /// `crate::lifecycle`.
    ///
    /// Mirrors `_lifecycle.lifecycle_stop`: the stamped results are swept
    /// after 5s, but only entries that still hold the sentinel are
    /// removed — a real response that raced in for the same key is never
    /// deleted.
    pub async fn shutdown_inflight(&self) -> usize {
        let shutdown = error_response(
            "lifecycle",
            "shutdown",
            "Agent engine stopped before this request completed.".to_string(),
        );
        let states: Vec<(String, Arc<FlightState>)> = {
            let mut flight = self.in_flight.lock().await;
            let states: Vec<(String, Arc<FlightState>)> = flight
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect();
            flight.clear();
            states
        };
        let keys: Vec<String> = states.iter().map(|(k, _)| k.clone()).collect();
        {
            let mut results = self.in_flight_results.lock().await;
            for key in &keys {
                results.insert(key.clone(), shutdown.clone());
            }
        }
        for (_, state) in &states {
            state.finish().await;
        }
        let stamped = keys.len();
        if stamped > 0 {
            let results_map = self.in_flight_results.clone();
            let sentinel = shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let mut results = results_map.lock().await;
                for key in keys {
                    let still_sentinel = results.get(&key).map(|r| {
                        r.provider == sentinel.provider
                            && r.model == sentinel.model
                            && r.error == sentinel.error
                    });
                    if still_sentinel.unwrap_or(false) {
                        results.remove(&key);
                    }
                }
            });
        }
        stamped
    }
}
