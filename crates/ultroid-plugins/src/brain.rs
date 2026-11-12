//! Ultroid AI Brain — port of `plugins/brain.py` (1,016 LOC).
//!
//! Commands:.ai/.ask (one-shot), .aip (forced provider), .ais
//! (streaming), .ailocal (Ollama only), .agent/.do (full agentic loop,
//! owner-only), .aichat (auto-respond toggle), .aireset, .aisys,
//! .aisysreset, .aiskills, .ollama, .aistatus, .aiverify, .aiflush,
//! .aiproviders, .aiconf, .aitools, .airounds, .aitemp, .aimodel.
//!
//! All AI work goes through the already-ported `ultroid-agent` crate
//! (`UltroidAgent` + `AgentRuntime`); no provider is reimplemented
//! here. There is no process-global agent singleton in Rust (Python's
//! `agent()`), so each command builds an `UltroidAgent`/`AgentRuntime`
//! pair from `ctx.db`/`ctx.config` — history is database-backed and
//! survives the rebuild; the per-process MiniAI response cache does
//! not (see the report).
//!
//! The `.aichat` auto-reply listener cannot be registered through the
//! command `Registry`; the binary wires [`handle_ai_chatbot`] into its
//! incoming-message path. Rich delivery uses `send_rich_or_plain`
//! (bot client rich, user client plain fallback); the `Ctx` cannot edit
//! a message *as* rich, so rich answers delete the placeholder and
//! send fresh, while plain answers edit it in place.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use serde_json::Value;
use ultroid_agent::agent::{AgentResponse, UltroidAgent};
use ultroid_agent::config::AgentConfig;
use ultroid_agent::controls::{describe_controls, get_controls, set_control};
use ultroid_agent::providers::{OllamaProvider, ProviderRegistry};
use ultroid_agent::reporting::{ReportParams, report_run};
use ultroid_agent::runtime::{AgentRequest, AgentRuntime};
use ultroid_agent::skills::skill_names;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::rich::RichDoc;
use ultroid_core::{ClientKind, CommandDef, Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub const AGENT_COMMAND_TIMEOUT_SECONDS: f64 = 105.0;

const PROVIDER_ALIASES: [(&str, &str); 8] = [
    ("grok", "xai"),
    ("google", "gemini"),
    ("claude", "anthropic"),
    ("meta", "meta_llama"),
    ("llama", "meta_llama"),
    ("apmix", "apimix"),
    ("custom", "openai_compat"),
    ("compat", "openai_compat"),
];

const PROVIDER_LABELS: [(&str, &str); 10] = [
    ("openai", "OpenAI"),
    ("gemini", "Gemini"),
    ("groq", "Groq"),
    ("xai", "xAI / Grok"),
    ("anthropic", "Anthropic / Claude"),
    ("meta_llama", "Meta / Llama"),
    ("apmix", "apmix.ai"),
    ("openai_compat", "Custom compatible"),
    ("ollama", "Ollama"),
    ("cloudflare", "Cloudflare Workers AI"),
];

fn provider_name(name: &str) -> String {
    let lower = name.to_lowercase();
    PROVIDER_ALIASES
        .iter()
        .find(|(k, _)| *k == lower)
        .map(|(_, v)| v.to_string())
        .unwrap_or(lower)
}

fn provider_label(name: &str) -> String {
    PROVIDER_LABELS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| name.replace('_', " "))
}

fn rich_allowed(ctx: &Ctx) -> bool {
    matches!(ctx.client_kind, ClientKind::AsstBot)
}

fn is_private(ctx: &Ctx) -> bool {
    matches!(ctx.chat_type, ultroid_core::ChatType::Private)
}

/// Build the agent + runtime pair for one command, from the command
/// context (Python's `agent()` singleton has no Rust equivalent).
fn build_agent(ctx: &Ctx) -> (Arc<UltroidAgent>, AgentRuntime) {
    let db = ctx.db.clone();
    let config = Arc::new(AgentConfig::new(db.clone()));
    let agent = Arc::new(UltroidAgent::new(
        Some(db.clone()),
        ctx.config.data_dir.clone(),
        None,
        None,
    ));
    let mut privileged: HashSet<i64> = ctx.config.sudo_special_ids.iter().copied().collect();
    privileged.insert(ctx.config.owner_id);
    let runtime = AgentRuntime::new(
        Arc::clone(&agent),
        Some(Arc::clone(&config)),
        Some(db),
        privileged,
        ctx.config.data_dir.clone(),
    );
    (agent, runtime)
}

fn registry(ctx: &Ctx) -> ProviderRegistry {
    ProviderRegistry::new(AgentConfig::new(ctx.db.clone()))
}

fn agent_config(ctx: &Ctx) -> AgentConfig {
    AgentConfig::new(ctx.db.clone())
}

fn chat_key(ctx: &Ctx) -> String {
    ctx.chat_id.to_string()
}

fn group(ctx: &Ctx, index: usize) -> String {
    ctx.captures.get(index).cloned().unwrap_or_default()
}

fn report(ctx: &Ctx, origin: &str, prompt: &str, response: &AgentResponse, duration_secs: f64) {
    let _ = report_run(
        Some(ctx.db.clone()),
        None,
        ReportParams {
            origin: origin.to_string(),
            chat_id: chat_key(ctx),
            user: None,
            provider: Some(response.provider.clone()),
            model: Some(response.model.clone()),
            prompt: prompt.to_string(),
            tools_used: response.tools_used.clone(),
            duration_secs: Some(duration_secs),
            ok: response.ok(),
            error: response.error.clone(),
        },
    );
}

// ── Delivery ─────────────────────────────────────────────────────────────

/// Deliver plain/rich text for a placeholder message: >4000 chars go to
/// a `response.txt` file (Python's rule); rich goes out as a fresh
/// `send_rich_or_plain` after deleting the placeholder (the `Ctx` cannot
/// edit a message as rich); plain text edits the placeholder in place.
async fn deliver_text(ctx: &Ctx, msg_id: i64, text: &str) -> Result<()> {
    let mut text = text.to_string();
    if text.chars().count() > 4000 {
        let _ = ctx.delete(msg_id).await;
        let path =
            std::env::temp_dir().join(format!("ultroid-response-{}.txt", rand::random::<u32>()));
        tokio::fs::write(&path, text.as_bytes())
            .await
            .map_err(Error::Io)?;
        let r = ctx.send_file(&path, "").await;
        let _ = tokio::fs::remove_file(&path).await;
        r?;
        return Ok(());
    }
    if rich_allowed(ctx) {
        if text.chars().count() > 32_000 {
            text = text.chars().take(32_000).collect();
        }
        let _ = ctx.delete(msg_id).await;
        let doc = RichDoc::markdown(&text)?;
        ctx.send_rich_or_plain(&doc).await?;
    } else {
        ctx.edit(msg_id, &text).await?;
    }
    Ok(())
}

/// Deliver an agent response (error text, empty-answer fallback, and
/// the `process_image` result, mirroring `_send_ai_response`).
async fn send_ai_response(ctx: &Ctx, msg_id: i64, response: &AgentResponse) -> Result<()> {
    let text = if let Some(_err) = &response.error {
        if response.provider == "runtime"
            && (response.model == "timeout" || response.model == "error")
            && !response.content.is_empty()
        {
            response.content.clone()
        } else {
            "Sorry, the AI service couldn't complete that request. Please try again.".to_string()
        }
    } else {
        let t = response.content.trim().to_string();
        if t.is_empty() {
            "Sorry, I couldn't produce a user-facing answer. Please try again.".to_string()
        } else {
            t
        }
    };
    // If the agent produced an image, send it too. The Rust agent never
    // populates `image_result` today (no `_image_result` producer in
    // ultroid-agent), so this only fires when a tool result carries a
    // `path` field — see the report.
    if response.error.is_none() {
        if let Some(path) = response
            .image_result
            .as_ref()
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
        {
            let p = Path::new(path);
            if p.exists() {
                let _ = ctx.delete(msg_id).await;
                ctx.send_file(p, "").await?;
            }
        }
    }
    deliver_text(ctx, msg_id, &text).await
}

fn prompt_or_reply(ctx: &Ctx, prompt: &str) -> String {
    let p = prompt.trim();
    if !p.is_empty() {
        return p.to_string();
    }
    ctx.reply_to_text
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ── Chatbot-mode state ───────────────────────────────────────────────────

async fn chatbot_chats(ctx: &Ctx) -> Vec<i64> {
    db_get_json(ctx.db.as_ref(), "AI_CHATBOT_CHATS")
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn set_chatbot_chats(ctx: &Ctx, chats: &[i64]) -> Result<()> {
    db_set_json(ctx.db.as_ref(), "AI_CHATBOT_CHATS", &chats.to_vec()).await
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_ai(ctx: &Ctx) -> Result<()> {
    let prompt = prompt_or_reply(ctx, &group(ctx, 2));
    if prompt.is_empty() {
        ctx.reply("Usage: .ai <your prompt>").await?;
        return Ok(());
    }
    let msg = ctx.reply("Thinking...").await?;
    let (_, runtime) = build_agent(ctx);
    let t0 = Instant::now();
    let mut req = AgentRequest::new(prompt.clone(), chat_key(ctx));
    req.origin = ".ai".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    let response = runtime.runtime_run(&req).await;
    report(ctx, ".ai", &prompt, &response, t0.elapsed().as_secs_f64());
    send_ai_response(ctx, msg, &response).await
}

async fn cmd_aip(ctx: &Ctx) -> Result<()> {
    let requested = group(ctx, 1).trim().to_lowercase();
    let provider = provider_name(&requested);
    let prompt = group(ctx, 2).trim().to_string();
    if prompt.is_empty() {
        ctx.reply("Usage: .aip <provider> <prompt>").await?;
        return Ok(());
    }
    let reg = registry(ctx);
    let p = match reg.provider(&provider) {
        Ok(p) => p,
        Err(_) => {
            let options = reg
                .all()
                .iter()
                .map(|p| p.name().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            ctx.reply(&format!("Unknown provider {requested}. Options: {options}"))
                .await?;
            return Ok(());
        }
    };
    if !p.is_configured() {
        ctx.reply(&format!(
            "Provider {provider} is not configured. Configure it, then run .aiverify {provider}."
        ))
        .await?;
        return Ok(());
    }
    let msg = ctx
        .reply(&format!("Asking {}...", provider_label(&provider)))
        .await?;
    let (_, runtime) = build_agent(ctx);
    let mut req = AgentRequest::new(prompt.clone(), chat_key(ctx));
    req.origin = ".aip".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.provider = Some(provider);
    req.use_tools = Some(false);
    req.use_cache = true;
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    let response = runtime.runtime_run(&req).await;
    send_ai_response(ctx, msg, &response).await
}

async fn cmd_ais(ctx: &Ctx) -> Result<()> {
    let prompt = group(ctx, 1).trim().to_string();
    if prompt.is_empty() {
        ctx.reply("Usage: .ais <prompt>").await?;
        return Ok(());
    }
    let msg = ctx.reply("Thinking...").await?;
    let (_, runtime) = build_agent(ctx);
    let mut req = AgentRequest::new(prompt.clone(), chat_key(ctx));
    req.origin = ".ais".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    let mut rx = runtime.runtime_stream(&req).await;
    let mut buf = String::new();
    let mut last = Instant::now();
    while let Some(chunk) = rx.recv().await {
        buf.push_str(&chunk);
        if last.elapsed().as_secs_f64() >= 1.8 {
            let visible = buf.trim();
            if !visible.is_empty() {
                let tail: String = visible
                    .chars()
                    .rev()
                    .take(3500)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let _ = ctx.edit(msg, &tail).await;
                last = Instant::now();
            }
        }
    }
    let final_text = buf.trim().to_string();
    let final_text = if final_text.is_empty() {
        "Sorry, I couldn't produce a user-facing answer. Please try again.".to_string()
    } else {
        final_text
    };
    deliver_text(ctx, msg, &final_text).await
}

async fn cmd_ailocal(ctx: &Ctx) -> Result<()> {
    let prompt = group(ctx, 1).trim().to_string();
    if prompt.is_empty() {
        ctx.reply("Usage: .ailocal <prompt>").await?;
        return Ok(());
    }
    let msg = ctx.reply("Asking the local model...").await?;
    let reg = registry(ctx);
    let running = match reg.provider("ollama") {
        Ok(p) => p.verify().await,
        Err(_) => false,
    };
    if !running {
        deliver_text(
            ctx,
            msg,
            "The local model service is not running.\nStart it with: ollama serve\nOr pull a model first: .ollama pull llama3",
        )
        .await?;
        return Ok(());
    }
    let (_, runtime) = build_agent(ctx);
    let mut req = AgentRequest::new(prompt.clone(), chat_key(ctx));
    req.origin = ".ailocal".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.provider = Some("ollama".to_string());
    req.use_tools = Some(false);
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    let response = runtime.runtime_run(&req).await;
    send_ai_response(ctx, msg, &response).await
}

async fn cmd_agent(ctx: &Ctx) -> Result<()> {
    let prompt = group(ctx, 2).trim().to_string();
    if prompt.is_empty() {
        ctx.reply(".agent <what you want done>\n\nThe agent can: run shell commands, search code, read traces, check updates, install packages, process images, search the web.").await?;
        return Ok(());
    }
    let msg = ctx.reply("Working...").await?;
    let (_, runtime) = build_agent(ctx);
    let t0 = Instant::now();
    let mut req = AgentRequest::new(prompt.clone(), chat_key(ctx));
    req.origin = ".agent".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.use_cache = false;
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    req.timeout_seconds = Some(AGENT_COMMAND_TIMEOUT_SECONDS);
    let response = runtime.runtime_run(&req).await;
    report(
        ctx,
        ".agent",
        &prompt,
        &response,
        t0.elapsed().as_secs_f64(),
    );
    send_ai_response(ctx, msg, &response).await
}

async fn cmd_aiskills(ctx: &Ctx) -> Result<()> {
    let names = skill_names(Some(".agent"));
    let mut text = "**Agent skills**".to_string();
    for name in names {
        text.push_str(&format!("\n- {name}"));
    }
    ctx.reply(&text).await?;
    Ok(())
}

async fn cmd_aichat(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1).trim().to_lowercase();
    let mut chats = chatbot_chats(ctx).await;
    let cid = ctx.chat_id;
    match arg.as_str() {
        "on" => {
            if !chats.contains(&cid) {
                chats.push(cid);
            }
            set_chatbot_chats(ctx, &chats).await?;
            ctx.reply(" AI chatbot mode **ON** for this chat.").await?;
        }
        "off" => {
            chats.retain(|c| *c != cid);
            set_chatbot_chats(ctx, &chats).await?;
            ctx.reply(" AI chatbot mode **OFF** for this chat.").await?;
        }
        _ => {
            let state = if chats.contains(&cid) { "ON" } else { "OFF" };
            ctx.reply(&format!(
                " AI chatbot mode is currently **{state}**.\nUse .aichat on or .aichat off."
            ))
            .await?;
        }
    }
    Ok(())
}

async fn cmd_aireset(ctx: &Ctx) -> Result<()> {
    let (agent, _) = build_agent(ctx);
    agent.clear_history(&chat_key(ctx)).await;
    ctx.reply(" AI conversation history cleared.").await?;
    Ok(())
}

async fn cmd_aisys(ctx: &Ctx) -> Result<()> {
    let prompt = group(ctx, 1).trim().to_string();
    let (agent, _) = build_agent(ctx);
    if prompt.is_empty() {
        let current = agent.chat_system(&chat_key(ctx)).await.unwrap_or_default();
        let shown = if current.is_empty() {
            "(default)".to_string()
        } else {
            current.chars().take(500).collect()
        };
        ctx.reply(&format!("**Current system prompt (first 500 chars):**\n\n{shown}\n\nUse .aisys <new prompt> to change it.\nUse .aisysreset to restore default.")).await?;
        return Ok(());
    }
    agent.set_chat_system(&chat_key(ctx), &prompt).await?;
    ctx.reply(&format!(
        " System prompt updated for this chat ({} chars).",
        prompt.chars().count()
    ))
    .await?;
    Ok(())
}

async fn cmd_aisysreset(ctx: &Ctx) -> Result<()> {
    let (agent, _) = build_agent(ctx);
    agent.reset_chat_system(&chat_key(ctx)).await?;
    ctx.reply(" Chat system prompt reset.").await?;
    Ok(())
}

async fn cmd_ollama(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1).trim().to_string();
    let mut parts = arg.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("status").to_lowercase();
    let rest = parts.next().unwrap_or("").trim().to_string();
    let cfg = agent_config(ctx);
    let reg = registry(ctx);

    match sub.as_str() {
        "status" | "" => {
            let provider = reg.provider("ollama")?;
            let running = provider.verify().await;
            let models = if running {
                provider.list_models().await.unwrap_or_default()
            } else {
                Vec::new()
            };
            let base = cfg
                .config_field("ollama", "BASE", Some("http://localhost:11434".to_string()))
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let model = provider.default_model();
            let status = if running { " Running" } else { " Not running" };
            let mut text = format!(
                "** Ollama Status**\n\nServer:  {status}\nURL:     {base}\nDefault: {model}\nModels:  {}",
                models.len()
            );
            if !models.is_empty() {
                text.push_str("\n\n**Available models:**\n");
                for m in models.iter().take(10) {
                    text.push_str(&format!("    {m}\n"));
                }
            }
            ctx.reply(&text).await?;
        }
        "models" => {
            let msg = ctx.reply(" fetching models").await?;
            let provider = reg.provider("ollama")?;
            let models = provider.list_models().await.unwrap_or_default();
            if models.is_empty() {
                ctx.edit(msg, "No models found. Try .ollama pull llama3")
                    .await?;
                return Ok(());
            }
            let mut text = "** Local Models**\n\n".to_string();
            for m in &models {
                text.push_str(&format!("    {m}\n"));
            }
            ctx.edit(msg, &text).await?;
        }
        "pull" => {
            if rest.is_empty() {
                ctx.reply("Usage: .ollama pull <model_name>\nExample: .ollama pull llama3")
                    .await?;
                return Ok(());
            }
            let msg = ctx.reply(&format!(" pulling {rest}")).await?;
            let ollama = OllamaProvider::new(cfg, ctx.http.clone());
            let mut stream = ollama.pull_model(&rest).await;
            let mut last = Instant::now();
            while let Some(item) = stream.next().await {
                if let Ok(line) = item {
                    if last.elapsed().as_secs_f64() >= 2.0 {
                        let _ = ctx
                            .edit(
                                msg,
                                &format!(
                                    " pulling {rest}\n{}",
                                    line.chars().take(100).collect::<String>()
                                ),
                            )
                            .await;
                        last = Instant::now();
                    }
                }
            }
            let provider = reg.provider("ollama")?;
            let models = provider.list_models().await.unwrap_or_default();
            let base_name = rest.split(':').next().unwrap_or("");
            let pulled = models
                .iter()
                .any(|m| m.split(':').next().unwrap_or("") == base_name);
            ctx.edit(
                msg,
                &format!(
                    "{rest} {}",
                    if pulled {
                        "is ready"
                    } else {
                        "pull may have failed   check Ollama logs"
                    }
                ),
            )
            .await?;
        }
        "use" => {
            if rest.is_empty() {
                ctx.reply("Usage: .ollama use <model_name>").await?;
                return Ok(());
            }
            if cfg
                .config_set("ollama", "MODEL", Some(&rest))
                .await
                .unwrap_or(false)
            {
                ctx.reply(&format!(" Default local model set to {rest}."))
                    .await?;
            } else {
                ctx.reply(" Could not save the model choice; it applies until the next restart.")
                    .await?;
            }
        }
        _ => {
            ctx.reply("** Ollama Commands**\n\n.ollama status         server status\n.ollama models         list local models\n.ollama pull <name>    download a model\n.ollama use <name>     set default model").await?;
        }
    }
    Ok(())
}

async fn cmd_aistatus(ctx: &Ctx) -> Result<()> {
    let msg = ctx.reply("Fetching AI status...").await?;
    let (agent, _) = build_agent(ctx);
    let status = agent.status().await;
    let reg = registry(ctx);
    let providers = status
        .get("providers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut names: Vec<String> = providers.keys().cloned().collect();
    names.sort();
    for name in &names {
        let configured = providers
            .get(name)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let state = if configured {
            "configured"
        } else {
            "not configured"
        };
        let mut detail = String::new();
        if let Ok(p) = reg.provider(name) {
            let caps = p.model_capabilities(Some(&p.default_model()));
            let mut features: Vec<&str> = Vec::new();
            if caps.tools {
                features.push("tools");
            }
            if caps.stream {
                features.push("stream");
            }
            detail = format!(" - {}", p.default_model());
            if !features.is_empty() {
                detail.push_str(&format!(" - {}", features.join(", ")));
            }
        }
        lines.push(format!(
            "- **{}** (`{name}`) - {state}{detail}",
            provider_label(name)
        ));
    }
    let text = format!(
        "**AI Service Status**\n\n**Providers ({} configured):**\n{}\n\n**Cache entries:** `{}`\n**Active histories:** `{}`\n**Max tool rounds:** `{}`\n**History window:** `{}` messages",
        status
            .get("configured_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        lines.join("\n"),
        status
            .get("cache_entries")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        status
            .get("active_histories")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        status
            .get("max_tool_rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        status
            .get("max_history")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    );
    ctx.edit(msg, &text).await?;
    Ok(())
}

async fn cmd_aiverify(ctx: &Ctx) -> Result<()> {
    let requested = group(ctx, 1).trim().to_lowercase();
    let reg = registry(ctx);
    let targets: Vec<String> = if !requested.is_empty() {
        let name = provider_name(&requested);
        if reg.provider(&name).is_err() {
            let options = reg
                .all()
                .iter()
                .map(|p| p.name().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            ctx.reply(&format!("Unknown provider {requested}. Options: {options}"))
                .await?;
            return Ok(());
        }
        vec![name]
    } else {
        reg.all().iter().map(|p| p.name().to_string()).collect()
    };
    let msg = ctx.reply(" verifying AI providers").await?;
    let mut lines: Vec<String> = Vec::new();
    for name in &targets {
        let Ok(p) = reg.provider(name) else { continue };
        if !p.is_configured() {
            lines.push(format!(
                " **{}** ({name}): not configured",
                provider_label(name)
            ));
            continue;
        }
        let verified = p.verify().await;
        let detail = if verified {
            "verified"
        } else {
            "verification failed"
        };
        lines.push(format!(" **{}** ({name}): {detail}", provider_label(name)));
    }
    let scope = if requested.is_empty() {
        String::new()
    } else {
        format!(" {}", provider_name(&requested))
    };
    ctx.edit(msg, &format!("** AI Provider Verification{scope}**\n\n{}\n\n_Read-only checks only; no prompt or model generation was sent._", lines.join("\n"))).await?;
    Ok(())
}

async fn cmd_aiflush(ctx: &Ctx) -> Result<()> {
    let msg = ctx.reply(" flushing cache").await?;
    let (agent, _) = build_agent(ctx);
    let count = agent.mini().mini_flush_cache().await;
    ctx.edit(msg, &format!(" Flushed {count} cached responses."))
        .await?;
    Ok(())
}

async fn cmd_aiproviders(ctx: &Ctx) -> Result<()> {
    let reg = registry(ctx);
    let mut seen: Vec<String> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let push =
        |name: &str, reg: &ProviderRegistry, lines: &mut Vec<String>, seen: &mut Vec<String>| {
            if seen.contains(&name.to_string()) {
                return;
            }
            seen.push(name.to_string());
            let Ok(p) = reg.provider(name) else { return };
            let caps = p.model_capabilities(Some(&p.default_model()));
            let mut features: Vec<&str> = Vec::new();
            if caps.tools {
                features.push("tools");
            }
            if caps.stream {
                features.push("stream");
            }
            let detail = format!(" - {}", p.default_model());
            let detail = if features.is_empty() {
                detail
            } else {
                format!("{detail} - {}", features.join(", "))
            };
            lines.push(format!(" **{}** ({name}){detail}", provider_label(name)));
        };
    for p in reg.all() {
        push(p.name(), &reg, &mut lines, &mut seen);
    }
    for extra in ["xai", "meta_llama"] {
        push(extra, &reg, &mut lines, &mut seen);
    }
    let text = format!(
        "** AI Providers**\n\n{}\n\nUse .aip <provider> <prompt> to select one, or .aiverify [provider] for read-only credential checks.",
        lines.join("\n")
    );
    ctx.reply(&text).await?;
    Ok(())
}

fn mask_key(value: &str) -> String {
    if value.is_empty() {
        return " ".to_string();
    }
    if value.chars().count() <= 8 {
        return "    ".to_string();
    }
    let first: String = value.chars().take(4).collect();
    let last: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{first}{last}")
}

async fn cmd_aiconf(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1).trim().to_string();
    let cfg = agent_config(ctx);
    if arg == "reset" {
        cfg.config_use_default(None, false).await?;
        ctx.reply("**AI provider pin cleared.** .ai now uses automatic routing.")
            .await?;
        return Ok(());
    }
    if let Some(name) = arg.strip_prefix("provider") {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            ctx.reply("Usage: .aiconf provider <name>   e.g. .aiconf provider gemini")
                .await?;
            return Ok(());
        }
        match cfg.config_use_default(Some(&name), true).await {
            Ok(_) => {
                ctx.reply(&format!(
                    "**.ai now uses {name}.** Use .aiconf to inspect the effective settings."
                ))
                .await?;
            }
            Err(e) => {
                ctx.reply(&e.to_string()).await?;
            }
        }
        return Ok(());
    }
    let default = cfg.config_default_provider().await.ok().flatten();
    let status = cfg.config_status().await?;
    let mut lines = vec![
        format!(
            "**Default for .ai:** {}",
            default.unwrap_or_else(|| "automatic (best available)".to_string())
        ),
        String::new(),
    ];
    for info in &status {
        let model = info
            .fields
            .iter()
            .find(|(f, _)| f == "MODEL")
            .and_then(|(_, v)| v.clone())
            .unwrap_or_else(|| "default".to_string());
        let base = info
            .fields
            .iter()
            .find(|(f, _)| f == "BASE")
            .and_then(|(_, v)| v.clone())
            .unwrap_or_default();
        let key = info
            .fields
            .iter()
            .find(|(f, _)| f == "KEY")
            .and_then(|(_, v)| v.clone())
            .unwrap_or_default();
        let key = mask_key(&key);
        let mut line = format!(" **{}** ({})   model {model}", info.label, info.provider);
        if !base.is_empty() {
            line.push_str(&format!("   {base}"));
        }
        if key != " " {
            line.push_str(&format!("   key {key}"));
        }
        lines.push(line);
    }
    let controls = describe_controls(Some(&ctx.db), &chat_key(ctx)).await;
    lines.push(String::new());
    lines.push(format!("**This chat:** {controls}"));
    lines.push(String::new());
    lines.push(
        "Configure providers with /agent in the bot's DM, or .aiconf provider <name> to pin one."
            .to_string(),
    );
    ctx.reply(&lines.join("\n")).await?;
    Ok(())
}

async fn cmd_aitools(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1).to_lowercase();
    if arg == "on" || arg == "off" {
        set_control(
            Some(&ctx.db),
            &chat_key(ctx),
            "tools",
            Some(Value::Bool(arg == "on")),
        )
        .await?;
    }
    let controls = get_controls(Some(&ctx.db), &chat_key(ctx)).await;
    ctx.reply(&format!(
        "**AI tools for this chat:** {}",
        if controls.tools { "on" } else { "off" }
    ))
    .await?;
    Ok(())
}

async fn cmd_airounds(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1);
    if arg == "reset" {
        set_control(Some(&ctx.db), &chat_key(ctx), "rounds", None).await?;
    } else if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = arg.parse::<u64>() {
            set_control(
                Some(&ctx.db),
                &chat_key(ctx),
                "rounds",
                Some(Value::from(n)),
            )
            .await?;
        }
    }
    let controls = get_controls(Some(&ctx.db), &chat_key(ctx)).await;
    let rounds = controls
        .rounds
        .map(|r| r.to_string())
        .unwrap_or_else(|| "default".to_string());
    ctx.reply(&format!("**Tool rounds for this chat:** {rounds}"))
        .await?;
    Ok(())
}

async fn cmd_aitemp(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1);
    if arg == "reset" {
        set_control(Some(&ctx.db), &chat_key(ctx), "temp", None).await?;
    } else if !arg.is_empty() {
        match arg.parse::<f64>() {
            Ok(value) if (0.0..=2.0).contains(&value) => {
                let v = serde_json::Number::from_f64(value).map(Value::Number);
                set_control(Some(&ctx.db), &chat_key(ctx), "temp", v).await?;
            }
            _ => {
                ctx.reply("Usage: .aitemp <0.0-2.0> or .aitemp reset")
                    .await?;
                return Ok(());
            }
        }
    }
    let controls = get_controls(Some(&ctx.db), &chat_key(ctx)).await;
    ctx.reply(&format!("**Temperature for this chat:** {}", controls.temp))
        .await?;
    Ok(())
}

async fn cmd_aimodel(ctx: &Ctx) -> Result<()> {
    let arg = group(ctx, 1).trim().to_string();
    if arg.is_empty() {
        let controls = get_controls(Some(&ctx.db), &chat_key(ctx)).await;
        ctx.reply(&format!(
            "**Model for this chat:** {}",
            controls
                .model
                .clone()
                .unwrap_or_else(|| "default".to_string())
        ))
        .await?;
        return Ok(());
    }
    let value = if matches!(arg.to_lowercase().as_str(), "reset" | "clear" | "default") {
        None
    } else {
        Some(Value::String(arg.clone()))
    };
    set_control(Some(&ctx.db), &chat_key(ctx), "model", value).await?;
    let shown = if matches!(arg.to_lowercase().as_str(), "reset" | "clear" | "default") {
        "default".to_string()
    } else {
        arg
    };
    ctx.reply(&format!("**Model for this chat:** {shown}"))
        .await?;
    Ok(())
}

/// The `.aichat` auto-reply listener, as a binary-callable function.
/// Returns `true` when a reply was sent.
pub async fn handle_ai_chatbot(ctx: &Ctx) -> Result<bool> {
    let prompt = ctx.text.trim();
    if prompt.is_empty() {
        return Ok(false);
    }
    if !chatbot_chats(ctx).await.contains(&ctx.chat_id) {
        return Ok(false);
    }
    let msg = ctx.reply("Thinking...").await?;
    let (_, runtime) = build_agent(ctx);
    let mut req = AgentRequest::new(prompt.to_string(), chat_key(ctx));
    req.origin = ".aichat".to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = is_private(ctx);
    req.rich_messages_allowed = Some(rich_allowed(ctx));
    let response = runtime.runtime_run(&req).await;
    send_ai_response(ctx, msg, &response).await?;
    Ok(true)
}

pub fn register(registry: &mut Registry) {
    let help = "**Ultroid AI Brain**\nDirect AI commands:\n   .ai <prompt> — one-shot AI response (best available provider)\n   .ask <prompt> — alias for .ai\n   .aip <provider> <msg> — force a specific registered provider\n   .ais <prompt> — streaming response (token by token)\n   .ailocal <prompt> — force Ollama (local LLM only)\nAgent mode (tools enabled):\n   .agent <prompt> — full agentic loop with tool access\n   .do <prompt> — alias for .agent (shorter)\nConversation:\n   .aichat — toggle chatbot mode (auto-respond in chat)\n   .aireset — clear conversation history for this chat\n   .aisys <prompt> — set custom system prompt\n   .aisysreset — reset system prompt to default\nLocal LLM (Ollama):\n   .ollama models — list available local models\n   .ollama pull <model> — pull a model from Ollama registry\n   .ollama use <model> — set default local model\n   .ollama status — check if Ollama is running\nPer-chat agentic controls:\n   .aitools on/off — enable or disable tool use for this chat\n   .airounds <n> — cap the agent's tool loop for this chat\n   .aitemp <x> — sampling temperature for this chat\n   .aimodel <name>/reset — pin or clear a model override for this chat\n   .aiconf [provider <name>/reset] — effective AI configuration, or pin the default provider\nStatus / config:\n   .aistatus — show all provider status + cache stats\n   .aiverify [provider] — safely verify configured provider credentials\n   .aiflush — flush response cache\n   .aiproviders — list registered providers";
    let silent = |d: CommandDef| CommandDef {
        silent_denials: false,
        ..d
    };
    reg_add(
        registry,
        silent(def("ai", "(ai|ask)(?: |$)([\\s\\S]*)", help)),
        |ctx| Box::pin(cmd_ai(ctx)),
    );
    reg_add(
        registry,
        silent(def("aip", "aip(?: |$)(\\w+)(?: |$)([\\s\\S]*)", help)),
        |ctx| Box::pin(cmd_aip(ctx)),
    );
    reg_add(
        registry,
        silent(def("ais", "ais(?: |$)([\\s\\S]*)", help)),
        |ctx| Box::pin(cmd_ais(ctx)),
    );
    reg_add(
        registry,
        silent(def("ailocal", "ailocal(?: |$)([\\s\\S]*)", help)),
        |ctx| Box::pin(cmd_ailocal(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("agent", "(agent|do)(?: |$)([\\s\\S]*)", help)
        },
        |ctx| Box::pin(cmd_agent(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aiskills", "aiskills$", help)
        },
        |ctx| Box::pin(cmd_aiskills(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aichat", "aichat(?: |$)(on|off|$)", help)
        },
        |ctx| Box::pin(cmd_aichat(ctx)),
    );
    reg_add(registry, silent(def("aireset", "aireset$", help)), |ctx| {
        Box::pin(cmd_aireset(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aisys", "aisys(?: |$)([\\s\\S]*)", help)
        },
        |ctx| Box::pin(cmd_aisys(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aisysreset", "aisysreset$", help)
        },
        |ctx| Box::pin(cmd_aisysreset(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("ollama", "ollama(?: |$)(.*)", help)
        },
        |ctx| Box::pin(cmd_ollama(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aistatus", "aistatus$", help)
        },
        |ctx| Box::pin(cmd_aistatus(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aiverify", "aiverify(?: |$)(\\w*)", help)
        },
        |ctx| Box::pin(cmd_aiverify(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            silent_denials: false,
            ..def("aiflush", "aiflush$", help)
        },
        |ctx| Box::pin(cmd_aiflush(ctx)),
    );
    reg_add(
        registry,
        silent(def("aiproviders", "aiproviders$", help)),
        |ctx| Box::pin(cmd_aiproviders(ctx)),
    );
    reg_add(
        registry,
        silent(def("aiconf", "aiconf(?: |$)([\\s\\S]*)$", help)),
        |ctx| Box::pin(cmd_aiconf(ctx)),
    );
    reg_add(
        registry,
        silent(def("aitools", "aitools(?: |$)(on|off)?$", help)),
        |ctx| Box::pin(cmd_aitools(ctx)),
    );
    reg_add(
        registry,
        silent(def("airounds", "airounds(?: |$)(\\d+|reset)?$", help)),
        |ctx| Box::pin(cmd_airounds(ctx)),
    );
    reg_add(
        registry,
        silent(def("aitemp", "aitemp(?: |$)([\\d.]+|reset)?$", help)),
        |ctx| Box::pin(cmd_aitemp(ctx)),
    );
    reg_add(
        registry,
        silent(def("aimodel", "aimodel(?: |$)([\\s\\S]*)$", help)),
        |ctx| Box::pin(cmd_aimodel(ctx)),
    );
}
