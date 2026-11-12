//! `/agent` — full agent-configuration menu tree for the assistant bot.
//!
//! Port of `assistant/agent.py`.
//!
//! One `@asst_cmd(pattern="agent")` entry point opens the dashboard;
//! six sections branch off it, driven by three callback handlers
//! over the colon callback model:
//! * `menu:<key>` — screen navigation (home, llama, llama:<prov>,
//!   cloud, cloud:<prov>, claude, keys, modules).
//! * `conv:<field>[:<prov>]` — conversation-style value entry.
//! * `act:<name>[:<id>]` — side-effects / fetches: `syscheck`,
//!   `llama_fetch`, `cloud_validate`, `cloud_primary`,
//!   `claude_fetch`, `keys_clear`, `close`.
//!
//! Persisted udB keys (namespace `AGENT_*`): local LLM
//! `AGENT_LLAMA_<PROV>_BASE|MODEL|CTX|TEMP|SYS`; cloud
//! `AGENT_CLOUD_<PROV>_KEY|MODEL|SYS|BASE`; Claude
//! `AGENT_CLAUDE_KEY|MODEL|FALLBACK`; pin `AGENT_PROVIDER`.
//!
//! The provider-pin / readiness logic is re-implemented here
//! against the same DB keys `ultroid-agent`'s `AgentConfig` reads
//! (`config_default_provider` / `config_ready` /
//! `config_use_default` in `ultroid-agent/src/config.rs`), because
//! this crate does not depend on `ultroid-agent` (see below).
//!
//! Real network calls, as in Python: Llama "Fetch Models" (Ollama
//! `{base}/api/tags`, OpenAI-compatible `{base}/v1/models` — never
//! doubling a `/v1` the base already carries); cloud "Validate Key"
//! (per-provider authenticated GET); Claude "Fetch Models" (real
//! GET to `api.anthropic.com/v1/models`, result pasted to spaceb.in);
//! "Check System" reachability (short-timeout GET per configured
//! endpoint). All go through `ctx.http` with a timeout.
//!
//! Contract limits:
//! * This crate's `Cargo.toml` does not list `ultroid-agent`, so
//!   `agent()` (sessions, `config_default_provider`, the runtime)
//!   cannot be called here. The menu logic that only reads/writes
//!   the `AGENT_*` DB keys is ported in full against `ctx.db`;
//!   anything that needs a live agent (running a completion) is
//!   not in this file at all — it belongs to `ai_dm`.
//! * Conversations: Python blocks on `conv.get_response()`. Here
//!   the prompt is sent and the pending field is stored in the DB
//!   (`asst:agentconv:<user_id>`); the binary routes the owner's
//!   next private message to `handle_conversation_reply`.
//! * "Modules" probes Python importability (`openai`, `anthropic`,
//!   `httpx`, `aiohttp`, `requests`). In this Rust build those
//!   Python libraries do not exist; the screen reports each as
//!   missing with the same install hint, and notes that this build
//!   does HTTP through `reqwest` instead.
//! * Telegraph: Python posts the Claude model list via
//!   `fns.make_html_telegraph` (a stored Telegraph token). This
//!   port posts to `api.telegra.ph/createPage` only when a token is
//!   stored under udB `TELEGRAPH_TOKEN`; otherwise the spaceb.in
//!   paste alone is used. No Telegraph account is created implicitly.

use serde::{Deserialize, Serialize};
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};
use ultroid_core::{CommandDef, Ctx, Registry};

use crate::cbs::{cb, db_del, db_get, def, is_cancel, reg_add};
use crate::rich_pages::{body_page, edit_callback_page, rich_page};

pub const DEFAULT_PROVIDER_KEY: &str = "AGENT_PROVIDER";

/// Local LLM servers: `(id, label, default base, api shape)`.
pub const LLAMA_PROVIDERS: &[(&str, &str, &str, &str)] = &[
    ("ollama", "Ollama", "http://localhost:11434", "ollama"),
    ("llamacpp", "llama.cpp", "http://localhost:8080", "openai"),
    (
        "lmstudio",
        "LM Studio",
        "http://localhost:1234/v1",
        "openai",
    ),
    ("tgi", "TGI / WebUI", "http://localhost:5000/v1", "openai"),
    ("vllm", "vLLM", "http://localhost:8000/v1", "openai"),
];

/// Cloud LLM providers: `(id, label)`.
pub const CLOUD_PROVIDERS: &[(&str, &str)] = &[
    ("openai", "OpenAI-compatible"),
    ("gemini", "Google Gemini"),
    ("anthropic", "Anthropic"),
    ("apimix", "apmix.ai"),
    ("openrouter", "OpenRouter"),
    ("groq", "Groq"),
    ("xai", "xAI (Grok)"),
    ("meta_llama", "Meta Llama API"),
];

/// Host roots for the reachability probe.
pub fn cloud_host(prov: &str) -> &'static str {
    match prov {
        "openai" => "https://api.openai.com",
        "gemini" => "https://generativelanguage.googleapis.com",
        "anthropic" => "https://api.anthropic.com",
        "apimix" => "https://api.apmix.ai",
        "openrouter" => "https://openrouter.ai",
        "groq" => "https://api.groq.com",
        "xai" => "https://api.x.ai",
        "meta_llama" => "https://api.llama.com",
        _ => "",
    }
}

/// Optional AI client libraries probed by the "modules" screen:
/// `(module, label, install hint)`.
pub const MODULES: &[(&str, &str, &str)] = &[
    ("openai", "openai", "pip install \"openai>=1.40\""),
    ("anthropic", "anthropic", "pip install anthropic"),
    ("httpx", "httpx", "pip install httpx"),
    ("aiohttp", "aiohttp", "pip install aiohttp"),
    ("requests", "requests", "pip install requests"),
];

/// Conversation-field metadata: `(field, label, prompt, validator)`.
/// Validator is `None` | `"int"` | `"float"`.
pub fn conv_field(field: &str) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    Some(match field {
        "llama_base" => (
            "Base URL",
            "Send the base URL of the local server (e.g. http://localhost:11434). Use /cancel to abort.",
            None,
        ),
        "llama_model" => (
            "Model",
            "Send the model name to use (e.g. llama3, qwen2.5:7b). Use /cancel to abort.",
            None,
        ),
        "llama_ctx" => (
            "Context Window",
            "Send the context window size in tokens (e.g. 4096). Use /cancel to abort.",
            Some("int"),
        ),
        "llama_temp" => (
            "Temperature",
            "Send the sampling temperature, 0.0 2.0 (e.g. 0.7). Use /cancel to abort.",
            Some("float"),
        ),
        "llama_sys" => (
            "System Prompt",
            "Send the system prompt text. Use /cancel to abort.",
            None,
        ),
        "cloud_key" => (
            "API Key",
            "Send the API key for this cloud provider. Use /cancel to abort.",
            None,
        ),
        "cloud_model" => (
            "Model",
            "Send the model id (e.g. gpt-4o-mini, gemini-1.5-pro). Use /cancel to abort.",
            None,
        ),
        "cloud_sys" => (
            "System Prompt",
            "Send the system prompt text. Use /cancel to abort.",
            None,
        ),
        "cloud_base" => (
            "Base URL",
            "Send the OpenAI-compatible base URL (e.g. https://api.openai.com/v1). Use /cancel to abort.",
            None,
        ),
        "claude_key" => (
            "Anthropic API Key",
            "Send your Anthropic API key (sk-ant-...). Use /cancel to abort.",
            None,
        ),
        "claude_model" => (
            "Default Model",
            "Send the default Claude model id (e.g. claude-3-5-sonnet-20241022). Use /cancel to abort.",
            None,
        ),
        "claude_fallback" => (
            "Fallback Model",
            "Send the fallback Claude model id. Use /cancel to abort.",
            None,
        ),
        _ => return None,
    })
}

/// Fields whose value may be echoed back in the confirmation
/// (non-secret, short). Keys and system prompts are never echoed.
pub fn is_echo_field(field: &str) -> bool {
    matches!(
        field,
        "llama_base"
            | "llama_model"
            | "llama_ctx"
            | "llama_temp"
            | "cloud_model"
            | "cloud_base"
            | "claude_model"
            | "claude_fallback"
    )
}

/// Build a udB key from a convo field + optional provider scope
/// (Python `_agent_var`): `llama_base` + `ollama` →
/// `AGENT_LLAMA_OLLAMA_BASE`.
pub fn agent_var(field: &str, scope: &str) -> String {
    let (section, suffix) = match field.split_once('_') {
        Some((s, rest)) => (s, rest),
        None => (field, ""),
    };
    let mut base = format!("AGENT_{}", section.to_ascii_uppercase());
    if !scope.is_empty() {
        base.push('_');
        base.push_str(&scope.to_ascii_uppercase());
    }
    if !suffix.is_empty() {
        base.push('_');
        base.push_str(&suffix.to_ascii_uppercase());
    }
    base
}

pub fn has_value(value: Option<&str>) -> bool {
    matches!(value, Some(v) if !v.trim().is_empty())
}

fn state_style(value: Option<&str>) -> Option<ButtonStyle> {
    if has_value(value) {
        Some(ButtonStyle::Success)
    } else {
        None
    }
}

fn menu_data(parts: &[&str]) -> String {
    let mut all = vec!["assistant", "agent", "menu"];
    all.extend(parts);
    cb(&all).unwrap_or_default()
}

fn conv_data(parts: &[&str]) -> String {
    let mut all = vec!["assistant", "agent", "conv"];
    all.extend(parts);
    cb(&all).unwrap_or_default()
}

fn act_data(parts: &[&str]) -> String {
    let mut all = vec!["assistant", "agent", "act"];
    all.extend(parts);
    cb(&all).unwrap_or_default()
}

pub const MENU_PREFIX: [&str; 3] = ["assistant", "agent", "menu"];
pub const CONV_PREFIX: [&str; 3] = ["assistant", "agent", "conv"];
pub const ACT_PREFIX: [&str; 3] = ["assistant", "agent", "act"];

/// Back button for a menu path: destination is the parent path
/// (Python `_back_btn`).
fn back_btn(current: &[&str]) -> InlineButton {
    let parent: Vec<&str> = if current.len() <= 1 {
        vec!["home"]
    } else {
        current[..current.len() - 1].to_vec()
    };
    let data = menu_data(&parent);
    InlineButton::data("Back", data.into_bytes())
}

fn back_btn_owned(current: &[&str]) -> Vec<Vec<InlineButton>> {
    vec![vec![back_btn(current)]]
}

fn btn(label: &str, data: String, style: Option<ButtonStyle>) -> InlineButton {
    let mut b = InlineButton::data(label, data.into_bytes());
    if let Some(s) = style {
        b = b.styled(s);
    }
    b
}

/// A settings table with an optional expandable details block
/// (Python `_settings_page`).
fn settings_page(
    title: &str,
    description: &str,
    rows: &[Vec<String>],
    details: Option<(&str, &str)>,
) -> Result<RichDoc> {
    let total = rows.len();
    let page = rich_page(
        title,
        rows,
        Some(description),
        &["Setting", "Current value"],
        1,
        1,
        Some(total),
        None,
    )?;
    let Some((detail_title, detail_text)) = details else {
        return Ok(page);
    };
    let html = page.html.clone().unwrap_or_default();
    let expandable = format!(
        "</table><blockquote expandable><b>{}</b>\n{}</blockquote><hr>",
        escape_html(detail_title),
        escape_html(detail_text)
    );
    Ok(RichDoc {
        html: Some(html.replacen("</table><hr>", &expandable, 1)),
        ..page
    })
}

// ── Provider pin / readiness (ultroid-agent AgentConfig logic) ───────────

/// Whether a provider has what it needs to run (Python
/// `AgentConfig.config_ready`): cloud providers need their KEY,
/// local providers their BASE.
pub async fn config_ready(ctx: &Ctx, provider: &str) -> bool {
    if LLAMA_PROVIDERS.iter().any(|(id, _, _, _)| *id == provider) {
        return has_value(
            db_get(ctx, &agent_var("llama_base", provider))
                .await
                .ok()
                .flatten()
                .as_deref(),
        );
    }
    if provider == "claude" {
        return has_value(
            db_get(ctx, "AGENT_CLAUDE_KEY")
                .await
                .ok()
                .flatten()
                .as_deref(),
        );
    }
    has_value(
        db_get(ctx, &agent_var("cloud_key", provider))
            .await
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// Resolve the pinned default provider, or configured apimix
/// (Python `AgentConfig.config_default_provider`).
pub async fn config_default_provider(ctx: &Ctx) -> Option<String> {
    if let Ok(Some(v)) = db_get(ctx, DEFAULT_PROVIDER_KEY).await {
        let mut v = v.to_ascii_lowercase();
        if v == "compat" {
            v = "openai_compat".to_string();
        }
        let known = LLAMA_PROVIDERS.iter().any(|(id, _, _, _)| *id == v)
            || CLOUD_PROVIDERS.iter().any(|(id, _)| *id == v)
            || v == "claude";
        if known {
            return Some(v);
        }
    }
    if config_ready(ctx, "apimix").await {
        return Some("apimix".to_string());
    }
    None
}

/// Pin / clear the default provider (Python
/// `AgentConfig.config_use_default`). With `require_configured`, a
/// pin for an unconfigured provider is refused.
pub async fn config_use_default(
    ctx: &Ctx,
    provider: Option<&str>,
    require_configured: bool,
) -> Result<bool> {
    match provider {
        None => Ok(db_del(ctx, DEFAULT_PROVIDER_KEY).await.is_ok()),
        Some(p) => {
            let known = LLAMA_PROVIDERS.iter().any(|(id, _, _, _)| *id == p)
                || CLOUD_PROVIDERS.iter().any(|(id, _)| *id == p)
                || p == "claude";
            if !known {
                return Err(Error::Config(format!("Unknown provider {p:?}")));
            }
            if require_configured && !config_ready(ctx, p).await {
                return Err(Error::Config(format!(
                    "Provider {p:?} is not configured; configure it first or keep automatic routing."
                )));
            }
            db_set_key(ctx, DEFAULT_PROVIDER_KEY, p).await?;
            Ok(true)
        }
    }
}

async fn db_set_key(ctx: &Ctx, key: &str, value: &str) -> Result<()> {
    ctx.db.set(key, value).await
}

fn provider_label(provider: &str) -> String {
    if let Some((_, label)) = CLOUD_PROVIDERS.iter().find(|(id, _)| *id == provider) {
        return label.to_string();
    }
    if let Some((_, label, _, _)) = LLAMA_PROVIDERS.iter().find(|(id, _, _, _)| *id == provider) {
        return label.to_string();
    }
    provider.to_string()
}

// ── HTTP helper ───────────────────────────────────────────────────────────

/// `(ok, status, body)` (Python `_http_get`): `ok` is false only on
/// timeout / connection error; any HTTP status still returns `ok`.
async fn http_get(
    ctx: &Ctx,
    url: &str,
    headers: &[(&str, String)],
    timeout_s: u64,
) -> (bool, Option<u16>, serde_json::Value) {
    let mut req = ctx.http.get(url);
    for (k, v) in headers {
        req = req.header(*k, v.clone());
    }
    let fut = async move {
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        Ok::<(u16, serde_json::Value), Error>((status, body))
    };
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_s), fut).await {
        Ok(Ok((status, body))) => (true, Some(status), body),
        Ok(Err(e)) => (false, None, serde_json::Value::String(e.to_string())),
        Err(_) => (
            false,
            None,
            serde_json::Value::String("timed out".to_string()),
        ),
    }
}

/// `(url, headers)` for a lightweight authenticated "whoami" request
/// (Python `_validate_spec`), or `None` when no safe validation
/// endpoint is known.
fn validate_spec(
    prov: &str,
    key: &str,
    base: Option<&str>,
) -> Option<(String, Vec<(String, String)>)> {
    match prov {
        "openai" => {
            let b = base
                .unwrap_or("https://api.openai.com/v1")
                .trim_end_matches('/')
                .to_string();
            Some((
                format!("{b}/models"),
                vec![("Authorization".into(), format!("Bearer {key}"))],
            ))
        }
        "gemini" => Some((
            format!("https://generativelanguage.googleapis.com/v1beta/models?key={key}"),
            vec![],
        )),
        "anthropic" => Some((
            "https://api.anthropic.com/v1/models".into(),
            vec![
                ("x-api-key".into(), key.to_string()),
                ("anthropic-version".into(), "2023-06-01".into()),
            ],
        )),
        "openrouter" => Some((
            "https://openrouter.ai/api/v1/key".into(),
            vec![("Authorization".into(), format!("Bearer {key}"))],
        )),
        "groq" => Some((
            "https://api.groq.com/openai/v1/models".into(),
            vec![("Authorization".into(), format!("Bearer {key}"))],
        )),
        "xai" => Some((
            "https://api.x.ai/v1/models".into(),
            vec![("Authorization".into(), format!("Bearer {key}"))],
        )),
        "meta_llama" => Some((
            "https://api.llama.com/v1/models".into(),
            vec![("Authorization".into(), format!("Bearer {key}"))],
        )),
        _ => None,
    }
}

/// Non-sensitive hint from a successful validation (Python
/// `_validate_hint`): model count or key label, never the key.
fn validate_hint(prov: &str, body: &serde_json::Value) -> String {
    if prov == "openrouter" {
        if let Some(d) = body.get("data").and_then(|v| v.as_object()) {
            let mut parts = Vec::new();
            if let Some(l) = d.get("label").and_then(|v| v.as_str()) {
                parts.push(format!("label: {l}"));
            }
            if let Some(u) = d.get("usage") {
                parts.push(format!("usage: {u}"));
            }
            if !parts.is_empty() {
                return format!(" ({})", parts.join(", "));
            }
        }
        return String::new();
    }
    if let Some(data) = body
        .get("data")
        .or_else(|| body.get("models"))
        .and_then(|v| v.as_array())
    {
        if !data.is_empty() {
            return format!(" ({} models listed)", data.len());
        }
    }
    String::new()
}

// ── Menu renderers ────────────────────────────────────────────────────────

pub async fn home_menu(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let primary = config_default_provider(ctx).await;
    let primary_label = match &primary {
        Some(p) => provider_label(p),
        None => "Automatic routing".to_string(),
    };
    let text = settings_page(
        "Agent Config",
        "Configure local and cloud models, manage keys, inspect installed modules, and run a system health check.",
        &[
            vec![
                "Version".into(),
                format!("v{}", ultroid_core::ULTROID_VERSION),
            ],
            vec!["Primary provider".into(), primary_label],
            vec![
                "Native RichMessage engine".into(),
                "Available for this assistant bot".into(),
            ],
        ],
        None,
    )?;
    let kb = vec![
        vec![
            btn(
                "Llama / Local",
                menu_data(&["llama"]),
                Some(ButtonStyle::Primary),
            ),
            btn(
                "Cloud AI",
                menu_data(&["cloud"]),
                Some(ButtonStyle::Primary),
            ),
        ],
        vec![
            btn(
                "Claude (Anthropic)",
                menu_data(&["claude"]),
                Some(ButtonStyle::Primary),
            ),
            btn("Keys", menu_data(&["keys"]), Some(ButtonStyle::Primary)),
        ],
        vec![
            btn(
                "Modules",
                menu_data(&["modules"]),
                Some(ButtonStyle::Primary),
            ),
            btn(
                "Check System",
                act_data(&["syscheck"]),
                Some(ButtonStyle::Primary),
            ),
        ],
        vec![btn("Close", act_data(&["close"]), None)],
    ];
    Ok((text, kb))
}

pub async fn llama_menu(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (pid, label, default_base, _) in LLAMA_PROVIDERS {
        let base = db_get(ctx, &agent_var("llama_base", pid))
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| default_base.to_string());
        let model = db_get(ctx, &agent_var("llama_model", pid))
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "model not set".to_string());
        rows.push(vec![label.to_string(), format!("{base} · {model}")]);
    }
    let text = settings_page(
        "Llama / Local",
        "Choose a local inference server. Each server keeps its own URL, model, context, temperature, and prompt.",
        &rows,
        None,
    )?;
    let mut kb: Vec<Vec<InlineButton>> = Vec::new();
    let mut items = LLAMA_PROVIDERS.iter();
    loop {
        let Some(a) = items.next() else { break };
        let mut row = vec![btn(
            a.1,
            menu_data(&["llama", a.0]),
            Some(ButtonStyle::Primary),
        )];
        if let Some(b) = items.next() {
            row.push(btn(
                b.1,
                menu_data(&["llama", b.0]),
                Some(ButtonStyle::Primary),
            ));
        }
        kb.push(row);
    }
    kb.push(back_btn_owned(&["llama"])[0].clone());
    Ok((text, kb))
}

pub async fn llama_prov_menu(ctx: &Ctx, prov: &str) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let Some((_, label, default_base, api)) =
        LLAMA_PROVIDERS.iter().find(|(id, _, _, _)| *id == prov)
    else {
        let page = body_page("Llama", "<p>Unknown provider.</p>", 1, 1, 1, None)?;
        return Ok((page, back_btn_owned(&["llama", prov])));
    };
    let stored_base = db_get(ctx, &agent_var("llama_base", prov))
        .await
        .ok()
        .flatten();
    let base = stored_base
        .clone()
        .unwrap_or_else(|| default_base.to_string());
    let model = db_get(ctx, &agent_var("llama_model", prov))
        .await
        .ok()
        .flatten();
    let context = db_get(ctx, &agent_var("llama_ctx", prov))
        .await
        .ok()
        .flatten();
    let temperature = db_get(ctx, &agent_var("llama_temp", prov))
        .await
        .ok()
        .flatten();
    let system_prompt = db_get(ctx, &agent_var("llama_sys", prov))
        .await
        .ok()
        .flatten();
    let text = settings_page(
        &format!("Llama · {label}"),
        "Settings for this local inference endpoint. Fetch Models checks the live server without changing configuration.",
        &[
            vec!["Base URL".into(), base],
            vec![
                "Model".into(),
                model.clone().unwrap_or_else(|| "Not set".into()),
            ],
            vec![
                "Context window".into(),
                context.clone().unwrap_or_else(|| "Not set".into()),
            ],
            vec![
                "Temperature".into(),
                temperature.clone().unwrap_or_else(|| "Not set".into()),
            ],
            vec!["API protocol".into(), api.to_string()],
        ],
        Some((
            "System prompt",
            system_prompt.as_deref().unwrap_or("Not set"),
        )),
    )?;
    let kb = vec![
        vec![
            btn(
                "Base URL",
                conv_data(&["llama_base", prov]),
                state_style(stored_base.as_deref()),
            ),
            btn(
                "Model",
                conv_data(&["llama_model", prov]),
                state_style(model.as_deref()),
            ),
        ],
        vec![
            btn(
                "Context",
                conv_data(&["llama_ctx", prov]),
                state_style(context.as_deref()),
            ),
            btn(
                "Temperature",
                conv_data(&["llama_temp", prov]),
                state_style(temperature.as_deref()),
            ),
        ],
        vec![btn(
            "System Prompt",
            conv_data(&["llama_sys", prov]),
            state_style(system_prompt.as_deref()),
        )],
        vec![btn("Fetch Models", act_data(&["llama_fetch", prov]), None)],
        back_btn_owned(&["llama", prov])[0].clone(),
    ];
    Ok((text, kb))
}

pub async fn cloud_menu(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (pid, label) in CLOUD_PROVIDERS {
        let key = db_get(ctx, &agent_var("cloud_key", pid))
            .await
            .ok()
            .flatten();
        let model = db_get(ctx, &agent_var("cloud_model", pid))
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "model not set".to_string());
        let cell = if has_value(key.as_deref()) {
            format!("configured · {model}")
        } else {
            "not configured".to_string()
        };
        rows.push(vec![label.to_string(), cell]);
    }
    let text = settings_page(
        "Cloud AI",
        "Store keys securely, choose a model and prompt, validate credentials, and select a primary provider.",
        &rows,
        None,
    )?;
    let mut kb: Vec<Vec<InlineButton>> = Vec::new();
    let mut items = CLOUD_PROVIDERS.iter();
    loop {
        let Some(a) = items.next() else { break };
        let mut row = vec![btn(
            a.1,
            menu_data(&["cloud", a.0]),
            Some(ButtonStyle::Primary),
        )];
        if let Some(b) = items.next() {
            row.push(btn(
                b.1,
                menu_data(&["cloud", b.0]),
                Some(ButtonStyle::Primary),
            ));
        }
        kb.push(row);
    }
    kb.push(back_btn_owned(&["cloud"])[0].clone());
    Ok((text, kb))
}

pub async fn cloud_prov_menu(ctx: &Ctx, prov: &str) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let label = provider_label(prov);
    let key = db_get(ctx, &agent_var("cloud_key", prov))
        .await
        .ok()
        .flatten();
    let stored_base = db_get(ctx, &agent_var("cloud_base", prov))
        .await
        .ok()
        .flatten();
    let base = match (&stored_base, prov) {
        (Some(b), _) => Some(b.clone()),
        (None, "openai") => Some("https://api.openai.com/v1".to_string()),
        (None, _) => None,
    };
    let model = db_get(ctx, &agent_var("cloud_model", prov))
        .await
        .ok()
        .flatten();
    let system_prompt = db_get(ctx, &agent_var("cloud_sys", prov))
        .await
        .ok()
        .flatten();
    let pinned = config_default_provider(ctx).await.as_deref() == Some(prov);
    let mut data = vec![
        vec!["API key".into(), crate::cbs::mask_secret(key.as_deref())],
        vec![
            "Model".into(),
            model.clone().unwrap_or_else(|| "Not set".into()),
        ],
        vec![
            "Primary for .ai/.ask".into(),
            if pinned { "Yes".into() } else { "No".into() },
        ],
    ];
    if prov == "openai" {
        data.insert(
            2,
            vec![
                "Base URL".into(),
                base.clone().unwrap_or_else(|| "Not set".into()),
            ],
        );
    }
    let text = settings_page(
        &format!("Cloud · {label}"),
        "Credentials are masked. Validate Key performs a read-only authenticated provider check.",
        &data,
        Some((
            "System prompt",
            system_prompt.as_deref().unwrap_or("Not set"),
        )),
    )?;
    let mut rows = vec![
        vec![
            btn(
                "API Key",
                conv_data(&["cloud_key", prov]),
                state_style(key.as_deref()),
            ),
            btn(
                "Model",
                conv_data(&["cloud_model", prov]),
                state_style(model.as_deref()),
            ),
        ],
        vec![btn(
            "System Prompt",
            conv_data(&["cloud_sys", prov]),
            state_style(system_prompt.as_deref()),
        )],
    ];
    if prov == "openai" {
        rows.insert(
            1,
            vec![btn(
                "Base URL",
                conv_data(&["cloud_base", prov]),
                state_style(stored_base.as_deref()),
            )],
        );
    }
    if has_value(key.as_deref()) {
        rows.push(vec![btn(
            "Validate Key",
            act_data(&["cloud_validate", prov]),
            Some(ButtonStyle::Success),
        )]);
        let pin_label = if !has_value(key.as_deref()) {
            "Not configured"
        } else if pinned {
            "Primary for .ai/.ask"
        } else {
            "Set as primary"
        };
        rows.push(vec![btn(
            pin_label,
            act_data(&["cloud_primary", prov]),
            if pinned {
                Some(ButtonStyle::Success)
            } else {
                None
            },
        )]);
    }
    rows.push(back_btn_owned(&["cloud", prov])[0].clone());
    Ok((text, rows))
}

pub async fn claude_menu(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let key = db_get(ctx, "AGENT_CLAUDE_KEY").await.ok().flatten();
    let model = db_get(ctx, "AGENT_CLAUDE_MODEL").await.ok().flatten();
    let fallback = db_get(ctx, "AGENT_CLAUDE_FALLBACK").await.ok().flatten();
    let text = settings_page(
        "Claude · Anthropic",
        "Configure model selection and fallback. Fetch Models checks Anthropic's live catalog and returns shareable specs.",
        &[
            vec!["API key".into(), crate::cbs::mask_secret(key.as_deref())],
            vec![
                "Default model".into(),
                model.clone().unwrap_or_else(|| "Not set".into()),
            ],
            vec![
                "Fallback model".into(),
                fallback.clone().unwrap_or_else(|| "Not set".into()),
            ],
        ],
        None,
    )?;
    let kb = vec![
        vec![
            btn(
                "API Key",
                conv_data(&["claude_key"]),
                state_style(key.as_deref()),
            ),
            btn(
                "Default Model",
                conv_data(&["claude_model"]),
                state_style(model.as_deref()),
            ),
        ],
        vec![btn(
            "Fallback Model",
            conv_data(&["claude_fallback"]),
            state_style(fallback.as_deref()),
        )],
        vec![btn("Fetch Models", act_data(&["claude_fetch"]), None)],
        back_btn_owned(&["claude"])[0].clone(),
    ];
    Ok((text, kb))
}

pub async fn keys_menu(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (pid, label) in CLOUD_PROVIDERS {
        let k = db_get(ctx, &agent_var("cloud_key", pid))
            .await
            .ok()
            .flatten();
        rows.push(vec![
            format!("Cloud · {label}"),
            if has_value(k.as_deref()) {
                crate::cbs::mask_secret(k.as_deref())
            } else {
                "Not set".to_string()
            },
        ]);
    }
    let ck = db_get(ctx, "AGENT_CLAUDE_KEY").await.ok().flatten();
    rows.push(vec![
        "Claude · Anthropic".into(),
        if has_value(ck.as_deref()) {
            crate::cbs::mask_secret(ck.as_deref())
        } else {
            "Not set".to_string()
        },
    ]);
    let mut configured = Vec::new();
    for (pid, label, _, _) in LLAMA_PROVIDERS {
        if has_value(
            db_get(ctx, &agent_var("llama_base", pid))
                .await
                .ok()
                .flatten()
                .as_deref(),
        ) {
            configured.push(*label);
        }
    }
    rows.push(vec![
        "Local LLM endpoints".into(),
        if configured.is_empty() {
            "None configured".to_string()
        } else {
            configured.join(", ")
        },
    ]);
    let text = settings_page(
        "Credentials",
        "Keys are shown masked and never echoed in full. Use the buttons to clear a provider section.",
        &rows,
        None,
    )?;
    let kb = vec![
        vec![
            btn(
                "Clear Llama",
                act_data(&["keys_clear", "llama"]),
                Some(ButtonStyle::Danger),
            ),
            btn(
                "Clear Cloud",
                act_data(&["keys_clear", "cloud"]),
                Some(ButtonStyle::Danger),
            ),
        ],
        vec![
            btn(
                "Clear Claude",
                act_data(&["keys_clear", "claude"]),
                Some(ButtonStyle::Danger),
            ),
            btn(
                "Clear All",
                act_data(&["keys_clear", "all"]),
                Some(ButtonStyle::Danger),
            ),
        ],
        back_btn_owned(&["keys"])[0].clone(),
    ];
    Ok((text, kb))
}

pub fn modules_menu() -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let rows: Vec<Vec<String>> = MODULES
        .iter()
        .map(|(_, label, install)| vec![label.to_string(), format!("Missing · {install}")])
        .collect();
    let text = settings_page(
        "AI Modules",
        "Importability of optional AI client libraries. Missing modules affect only the features that use them. This Rust build has no Python client libraries; HTTP goes through reqwest instead.",
        &rows,
        None,
    )?;
    Ok((text, back_btn_owned(&["modules"])))
}

/// Dispatch a `menu:<path...>` callback to its screen renderer.
/// Returns `None` for a dead path (caller lands on home).
pub async fn render_menu(
    ctx: &Ctx,
    key: &str,
) -> Result<Option<(RichDoc, Vec<Vec<InlineButton>>)>> {
    let levels: Vec<&str> = if key.is_empty() {
        vec![]
    } else {
        key.split(':').collect()
    };
    Ok(match levels.as_slice() {
        ["home"] => Some(home_menu(ctx).await?),
        ["llama"] => Some(llama_menu(ctx).await?),
        ["llama", prov] => Some(llama_prov_menu(ctx, prov).await?),
        ["cloud"] => Some(cloud_menu(ctx).await?),
        ["cloud", prov] => Some(cloud_prov_menu(ctx, prov).await?),
        ["claude"] => Some(claude_menu(ctx).await?),
        ["keys"] => Some(keys_menu(ctx).await?),
        ["modules"] => Some(modules_menu()?),
        _ => None,
    })
}

// ── Actions ───────────────────────────────────────────────────────────────

async fn llama_fetch(ctx: &Ctx, prov: &str) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let back = back_btn_owned(&["llama", prov]);
    let Some((_, label, default_base, api)) =
        LLAMA_PROVIDERS.iter().find(|(id, _, _, _)| *id == prov)
    else {
        return Ok((
            body_page("Llama", "<p>Unknown provider.</p>", 1, 1, 1, None)?,
            back,
        ));
    };
    let base = db_get(ctx, &agent_var("llama_base", prov))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| default_base.to_string());
    let base = base.trim_end_matches('/').to_string();
    let url = if api == &"ollama" {
        format!("{base}/api/tags")
    } else if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let head = format!("Llama - {label} - Models");
    let (ok, status, body) = http_get(ctx, &url, &[], 10).await;
    if !ok {
        let reason = body.as_str().unwrap_or("unreachable").to_string();
        return Ok((
            body_page(
                &head,
                &format!(
                    "<p>Unreachable - needs a running provider at <code>{}</code>.</p><p>Reason: <code>{}</code></p>",
                    escape_html(&url),
                    escape_html(&reason)
                ),
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    }
    if status != Some(200) {
        return Ok((
            body_page(
                &head,
                &format!(
                    "<p>Server responded HTTP {} at <code>{}</code>. Is the right provider running there?</p>",
                    status.unwrap_or(0),
                    escape_html(&url)
                ),
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    }
    let (count, lines) = if api == &"ollama" {
        let models = body
            .get("models")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let lines: Vec<String> = models
            .iter()
            .take(30)
            .map(|m| {
                let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let det = m.get("details").cloned().unwrap_or(serde_json::Value::Null);
                let size = det
                    .get("parameter_size")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let quant = det
                    .get("quantization_level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                format!(
                    "- <b>{}</b> - {} - {}",
                    escape_html(name),
                    escape_html(size),
                    escape_html(quant)
                )
            })
            .collect();
        (models.len(), lines)
    } else {
        let models = body
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let lines: Vec<String> = models
            .iter()
            .take(30)
            .map(|m| {
                let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                format!("- <b>{}</b>", escape_html(id))
            })
            .collect();
        (models.len(), lines)
    };
    let msg = if lines.is_empty() {
        "No models returned by the server.".to_string()
    } else {
        let mut m = format!(
            "Fetched {count} model(s) from <code>{}</code>:\n\n{}",
            escape_html(&url),
            lines.join("\n")
        );
        if count > 30 {
            m.push_str(&format!("\n... and {} more", count - 30));
        }
        m
    };
    Ok((
        body_page(
            &head,
            &format!("<p>{}</p>", escape_html(&msg).replace('\n', "<br>")),
            1,
            1,
            1,
            None,
        )?,
        back,
    ))
}

async fn cloud_validate(ctx: &Ctx, prov: &str) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let back = back_btn_owned(&["cloud", prov]);
    let label = provider_label(prov);
    let head = format!("Cloud - {label} - Validate");
    let key = db_get(ctx, &agent_var("cloud_key", prov))
        .await
        .ok()
        .flatten();
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return Ok((
            body_page(
                &head,
                "<p>No API key set for this provider. Set one first.</p>",
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    };
    let base = db_get(ctx, &agent_var("cloud_base", prov))
        .await
        .ok()
        .flatten();
    let Some((url, headers)) = validate_spec(prov, &key, base.as_deref()) else {
        return Ok((
            body_page(
                &head,
                "<p>No safe validation endpoint is known for this provider. Validate by sending a real completion request with your key.</p>",
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    };
    let headers_ref: Vec<(&str, String)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let (ok, status, body) = http_get(ctx, &url, &headers_ref, 12).await;
    if !ok {
        let reason = body.as_str().unwrap_or("unreachable").to_string();
        return Ok((
            body_page(
                &head,
                &format!(
                    "<p>Could not reach provider.</p><p>Reason: <code>{}</code></p>",
                    escape_html(&reason)
                ),
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    }
    let status = status.unwrap_or(0);
    let html = if status == 200 {
        format!(
            "<p>Key is valid. Provider authenticated the request.{}</p>",
            escape_html(&validate_hint(prov, &body))
        )
    } else if status == 401 || status == 403 {
        format!("<p>Provider rejected the key (HTTP {status}). Check the key and try again.</p>")
    } else {
        format!("<p>Unexpected response (HTTP {status}).</p>")
    };
    Ok((body_page(&head, &html, 1, 1, 1, None)?, back))
}

async fn claude_fetch(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let back = back_btn_owned(&["claude"]);
    let head = "Claude - Models";
    let key = db_get(ctx, "AGENT_CLAUDE_KEY").await.ok().flatten();
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return Ok((
            body_page(
                head,
                "<p>No Anthropic API key set. Set one first (Claude API Key).</p>",
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    };
    let (ok, status, body) = http_get(
        ctx,
        "https://api.anthropic.com/v1/models",
        &[
            ("x-api-key", key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ],
        12,
    )
    .await;
    if !ok {
        let reason = body.as_str().unwrap_or("unreachable").to_string();
        return Ok((
            body_page(
                head,
                &format!(
                    "<p>Could not reach Anthropic.</p><p>Reason: <code>{}</code></p>",
                    escape_html(&reason)
                ),
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    }
    if status != Some(200) {
        let status = status.unwrap_or(0);
        let mut msg = format!("Anthropic rejected the request (HTTP {status}).");
        if status == 401 || status == 403 {
            msg.push_str(" Check your API key.");
        }
        return Ok((
            body_page(
                head,
                &format!("<p>{}</p>", escape_html(&msg)),
                1,
                1,
                1,
                None,
            )?,
            back,
        ));
    }
    let data = body
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if data.is_empty() {
        return Ok((
            body_page(head, "<p>No models returned.</p>", 1, 1, 1, None)?,
            back,
        ));
    }
    let mut specs = vec![
        format!(
            "Anthropic Models - fetched {}",
            crate::cbs::fmt_utc(crate::cbs::now_unix())
        ),
        String::new(),
    ];
    for m in &data {
        specs.push(format!(
            "- {}",
            m.get("id").and_then(|v| v.as_str()).unwrap_or("?")
        ));
        if let Some(v) = m.get("display_name").and_then(|v| v.as_str()) {
            specs.push(format!("    display_name: {v}"));
        }
        if let Some(v) = m.get("created_at").and_then(|v| v.as_str()) {
            specs.push(format!("    created_at: {v}"));
        }
        if let Some(v) = m.get("type").and_then(|v| v.as_str()) {
            specs.push(format!("    type: {v}"));
        }
    }
    let specs_text = specs.join("\n");
    let paste_url = match crate::callbackstuffs::spacebin_paste(ctx, &specs_text).await {
        Ok((true, key)) => Some(format!("https://spaceb.in/{key}")),
        _ => None,
    };
    let tele_url = telegraph_post(ctx, "Anthropic Models", &specs_text).await;
    let mut summary: Vec<String> = data
        .iter()
        .take(20)
        .map(|m| {
            format!(
                "- {}",
                escape_html(m.get("id").and_then(|v| v.as_str()).unwrap_or("?"))
            )
        })
        .collect();
    if data.len() > 20 {
        summary.push(format!("... and {} more", data.len() - 20));
    }
    let mut body_html = format!("Fetched {} model(s):\n\n{}", data.len(), summary.join("\n"));
    if paste_url.is_none() && tele_url.is_none() {
        body_html.push_str("\n\n( Paste generation failed - full list shown above. )");
    }
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let mut url_row: Vec<InlineButton> = Vec::new();
    if let Some(u) = &paste_url {
        url_row.push(InlineButton::url("Specs Paste", u));
    }
    if let Some(u) = &tele_url {
        url_row.push(InlineButton::url("Telegraph", u));
    }
    if !url_row.is_empty() {
        rows.push(url_row);
    }
    rows.push(back_btn_owned(&["claude"])[0].clone());
    Ok((
        body_page(
            head,
            &format!("<p>{}</p>", escape_html(&body_html).replace('\n', "<br>")),
            1,
            1,
            1,
            None,
        )?,
        rows,
    ))
}

/// Post a page to Telegraph with a stored token (udB
/// `TELEGRAPH_TOKEN`). Returns `None` when no token is stored —
/// this port never creates a Telegraph account implicitly.
async fn telegraph_post(ctx: &Ctx, title: &str, text: &str) -> Option<String> {
    let token = db_get(ctx, "TELEGRAPH_TOKEN").await.ok().flatten()?;
    if token.trim().is_empty() {
        return None;
    }
    let body = serde_json::json!({
        "access_token": token,
        "title": title,
        "content": [{"tag": "pre", "children": [text]}],
        "return_content": false,
    });
    let resp: serde_json::Value = ctx
        .http
        .post("https://api.telegra.ph/createPage")
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    resp.get("result")
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

async fn keys_clear(ctx: &Ctx, scope: &str) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let mut cleared: Vec<String> = Vec::new();
    let mut del_key = async |var: &str| {
        if db_get(ctx, var).await.ok().flatten().is_some() {
            let _ = ctx.db.del(var).await;
            cleared.push(var.to_string());
        }
    };
    if scope == "llama" || scope == "all" {
        for (pid, _, _, _) in LLAMA_PROVIDERS {
            for f in ["BASE", "MODEL", "CTX", "TEMP", "SYS"] {
                del_key(&format!("AGENT_LLAMA_{}_{f}", pid.to_ascii_uppercase())).await;
            }
        }
    }
    if scope == "cloud" || scope == "all" {
        for (pid, _) in CLOUD_PROVIDERS {
            for f in ["KEY", "MODEL", "SYS", "BASE"] {
                del_key(&format!("AGENT_CLOUD_{}_{f}", pid.to_ascii_uppercase())).await;
            }
        }
    }
    if scope == "claude" || scope == "all" {
        for f in ["KEY", "MODEL", "FALLBACK"] {
            del_key(&format!("AGENT_CLAUDE_{f}")).await;
        }
    }
    // A pin that outlives its key is a trap: drop it when the pinned
    // provider just lost its credentials.
    if let Some(pinned) = config_default_provider(ctx).await {
        if !config_ready(ctx, &pinned).await {
            let _ = config_use_default(ctx, None, false).await;
        }
    }
    let msg = if cleared.is_empty() {
        "Nothing to clear - no credentials stored for this section.".to_string()
    } else {
        format!("Cleared {} stored credential(s).", cleared.len())
    };
    Ok((
        body_page(
            "Credentials - Cleared",
            &format!("<p>{}</p>", escape_html(&msg)),
            1,
            1,
            1,
            None,
        )?,
        back_btn_owned(&["keys"]),
    ))
}

async fn syscheck(ctx: &Ctx) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    let mem = mem_info().await;
    let endpoints = collect_endpoints(ctx).await;
    let mut lines = vec![
        format!(
            "<p>Runtime: <code>{}</code></p>",
            escape_html("ultroid-rs (Rust)")
        ),
        format!(
            "<p>Platform: <code>{} {}</code></p>",
            escape_html(std::env::consts::OS),
            escape_html(std::env::consts::ARCH)
        ),
        format!("<p>Memory: <code>{}</code></p>", escape_html(&mem)),
        "<p><b>Endpoint reachability:</b></p>".to_string(),
    ];
    if endpoints.is_empty() {
        lines.push("<p>- No provider endpoints configured yet.</p>".to_string());
    } else {
        for (label, url) in endpoints {
            let (ok, _status, body) = http_get(ctx, &url, &[], 6).await;
            let state = if ok {
                "reachable".to_string()
            } else {
                body.as_str().unwrap_or("unreachable").to_string()
            };
            lines.push(format!(
                "<p>- {} - {} (<code>{}</code>)</p>",
                escape_html(&label),
                escape_html(&state),
                escape_html(&url)
            ));
        }
    }
    Ok((
        body_page("System Check", &lines.join(""), 1, 1, 1, None)?,
        back_btn_owned(&["home"]),
    ))
}

/// Memory readout from `/proc/meminfo` (Python `_mem_info` without
/// psutil; the psutil branch has no Rust equivalent here).
async fn mem_info() -> String {
    let Ok(content) = tokio::fs::read_to_string("/proc/meminfo").await else {
        return "unavailable (install psutil for details)".to_string();
    };
    let mut d: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for line in content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if let Some(n) = v
                .trim()
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<i64>().ok())
            {
                d.insert(k.trim().to_string(), n);
            }
        }
    }
    let (Some(total), Some(avail)) = (d.get("MemTotal"), d.get("MemAvailable")) else {
        return "unavailable (install psutil for details)".to_string();
    };
    let (total, avail) = (total / 1024, avail / 1024);
    let pct = if total > 0 {
        100 * (total - avail) / total
    } else {
        0
    };
    format!("{pct}% used - {avail} MB free / {total} MB total")
}

/// Endpoints to probe (Python `_collect_endpoints`).
async fn collect_endpoints(ctx: &Ctx) -> Vec<(String, String)> {
    let mut eps: Vec<(String, String)> = Vec::new();
    for (pid, label, _, _) in LLAMA_PROVIDERS {
        if let Some(base) = db_get(ctx, &agent_var("llama_base", pid))
            .await
            .ok()
            .flatten()
        {
            eps.push((
                format!("Llama - {label}"),
                base.trim_end_matches('/').to_string(),
            ));
        }
    }
    for (pid, label) in CLOUD_PROVIDERS {
        if has_value(
            db_get(ctx, &agent_var("cloud_key", pid))
                .await
                .ok()
                .flatten()
                .as_deref(),
        ) {
            if *pid == "openai" {
                let b = db_get(ctx, &agent_var("cloud_base", pid))
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let b = b.trim_end_matches('/').to_string();
                let host = b.split("/v1").next().unwrap_or(&b).to_string();
                eps.push((format!("Cloud - {label}"), host));
            } else {
                eps.push((format!("Cloud - {label}"), cloud_host(pid).to_string()));
            }
        }
    }
    if has_value(
        db_get(ctx, "AGENT_CLAUDE_KEY")
            .await
            .ok()
            .flatten()
            .as_deref(),
    ) {
        eps.push((
            "Claude - Anthropic".to_string(),
            "https://api.anthropic.com".to_string(),
        ));
    }
    if eps.is_empty() {
        eps.push((
            "Ollama (default)".to_string(),
            LLAMA_PROVIDERS[0].2.to_string(),
        ));
    }
    eps
}

// ── Conversation state ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingAgentConv {
    pub field: String,
    pub scope: String,
}

fn pending_key(user_id: i64) -> String {
    format!("asst:agentconv:{user_id}")
}

/// Route an incoming private message to a pending agent-config
/// conversation. Returns `true` when it consumed the message.
pub async fn handle_conversation_reply(ctx: &mut Ctx) -> Result<bool> {
    let key = pending_key(ctx.sender_id);
    let Some(raw) = ctx.db.get(&key).await? else {
        return Ok(false);
    };
    let Ok(pending) = serde_json::from_str::<PendingAgentConv>(&raw) else {
        return Ok(false);
    };
    let text = ctx.text.trim().to_string();
    let Some((label, _prompt, validator)) = conv_field(&pending.field) else {
        return Ok(false);
    };
    let back = conv_back(&pending.field, &pending.scope);
    if is_cancel(&text) {
        ctx.db.del(&key).await?;
        ctx.reply("Cancelled.").await?;
        let _ = back;
        return Ok(true);
    }
    let value: String = match validator {
        Some("int") => match text.parse::<i64>() {
            Ok(n) => n.to_string(),
            Err(_) => {
                ctx.reply(&format!("Invalid value for {label}.")).await?;
                return Ok(true);
            }
        },
        Some("float") => match text.parse::<f64>() {
            Ok(n) if (0.0..=2.0).contains(&n) => text,
            _ => {
                ctx.reply(&format!("Invalid value for {label}.")).await?;
                return Ok(true);
            }
        },
        _ => text,
    };
    let var = agent_var(&pending.field, &pending.scope);
    ctx.db.set(&var, &value).await?;
    ctx.db.del(&key).await?;
    let shown = if is_echo_field(&pending.field) {
        value
    } else if pending.field.ends_with("_key") {
        "   - (masked)".to_string()
    } else {
        "saved".to_string()
    };
    ctx.reply(&format!("{label} set to {shown}.")).await?;
    Ok(true)
}

fn conv_back(field: &str, scope: &str) -> String {
    let section = field.split('_').next().unwrap_or("");
    match section {
        "llama" => menu_data(&["llama", scope]),
        "cloud" => menu_data(&["cloud", scope]),
        _ => menu_data(&["claude"]),
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// `/agent` — open the dashboard (owner-only).
pub async fn agent_cmd(ctx: &Ctx) -> Result<()> {
    let (text, _kb) = home_menu(ctx).await?;
    ctx.send_rich_or_plain(&text).await?;
    Ok(())
}

/// Dispatch callback data owned by this module. Returns `true` when
/// the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    use crate::cbs::cb_tail;
    if let Some(tail) = cb_tail(data, &MENU_PREFIX)? {
        match render_menu(ctx, &tail).await? {
            Some((text, kb)) => {
                edit_callback_page(ctx, &text, kb).await?;
            }
            None => {
                let (text, kb) = home_menu(ctx).await?;
                ctx.reply("That screen no longer exists; reopened home.")
                    .await?;
                edit_callback_page(ctx, &text, kb).await?;
            }
        }
        return Ok(true);
    }
    if let Some(tail) = cb_tail(data, &CONV_PREFIX)? {
        return conv_start(ctx, &tail).await;
    }
    if let Some(tail) = cb_tail(data, &ACT_PREFIX)? {
        return act_handler(ctx, &tail).await;
    }
    Ok(false)
}

async fn conv_start(ctx: &Ctx, key: &str) -> Result<bool> {
    let (field, scope) = match key.split_once(':') {
        Some((f, s)) => (f.to_string(), s.to_string()),
        None => (key.to_string(), String::new()),
    };
    let Some((_label, prompt, _validator)) = conv_field(&field) else {
        return Ok(true);
    };
    ctx.delete(ctx.message_id).await?;
    let pending = PendingAgentConv {
        field: field.clone(),
        scope: scope.clone(),
    };
    ctx.db
        .set(
            &pending_key(ctx.sender_id),
            &serde_json::to_string(&pending).map_err(Error::from)?,
        )
        .await?;
    ctx.reply(prompt).await?;
    Ok(true)
}

async fn act_handler(ctx: &mut Ctx, key: &str) -> Result<bool> {
    let (action, rest) = match key.split_once(':') {
        Some((a, r)) => (a.to_string(), r.to_string()),
        None => (key.to_string(), String::new()),
    };
    if action == "close" {
        ctx.delete(ctx.message_id).await?;
        return Ok(true);
    }
    ctx.reply("Working...").await?;
    let (text, kb) = match action.as_str() {
        "syscheck" => syscheck(ctx).await?,
        "llama_fetch" => llama_fetch(ctx, &rest).await?,
        "cloud_validate" => cloud_validate(ctx, &rest).await?,
        "cloud_primary" => match config_use_default_toggle(ctx, &rest).await {
            Ok(page) => page,
            Err(e) => {
                let page = body_page(
                    "Primary provider",
                    &format!(
                        "<p>Could not set {} as primary: {}</p>",
                        escape_html(&provider_label(&rest)),
                        escape_html(&e.to_string())
                    ),
                    1,
                    1,
                    1,
                    None,
                )?;
                (page, back_btn_owned(&["cloud", &rest]))
            }
        },
        "claude_fetch" => claude_fetch(ctx).await?,
        "keys_clear" => keys_clear(ctx, &rest).await?,
        _ => return Ok(true),
    };
    // Python wraps plain-string results in `body_page("Agent result", ...)`;
    // our actions already return pages.
    edit_callback_page(ctx, &text, kb).await?;
    Ok(true)
}

async fn config_use_default_toggle(
    ctx: &Ctx,
    prov: &str,
) -> Result<(RichDoc, Vec<Vec<InlineButton>>)> {
    if config_default_provider(ctx).await.as_deref() == Some(prov) {
        config_use_default(ctx, None, false).await?;
    } else {
        config_use_default(ctx, Some(prov), true).await?;
    }
    cloud_prov_menu(ctx, prov).await
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("agent", "agent", "Open the agent configuration menu.")
        },
        |ctx| Box::pin(agent_cmd(ctx)),
    );
}
