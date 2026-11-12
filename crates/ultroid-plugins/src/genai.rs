//! Port of `plugins/genai.py` — GenAI command adapters over the shared
//! agent runtime.
//!
//! `.genai <prompt>` / `.genai clear` (owner-only, assistant command in
//! the Python version) and `.cleargenai`. The `{query} !?` suffix trigger
//! from the Python version is not a command: `Registry` only matches
//! handler-prefixed commands, so the trigger is ported as
//! `parse_trigger` + `check_trigger` for the binary's outgoing-message
//! path (see `genai_module.rs`, which shares this module's turn runner).
//!
//! Turns run through `ultroid_agent::runtime::AgentRuntime` with the
//! per-chat controls from `ultroid_agent::controls` and the caller-scoped
//! history key `genai:<chat_id>:<user_id>`. If no provider is configured
//! the runtime answers with its own configuration error — nothing is
//! invented here.

use crate::util::{def, reg_add};
use regex::Regex;
use std::collections::HashSet;
use std::sync::Arc;
use ultroid_agent::agent::AgentResponse;
use ultroid_agent::agent::UltroidAgent;
use ultroid_agent::config::AgentConfig;
use ultroid_agent::controls::get_controls;
use ultroid_agent::runtime::{AgentRequest, AgentRuntime};
use ultroid_core::{ChatType, ClientKind, Ctx, Registry, Result, RichDoc};

/// The `{query} !? [-i]` trigger (Python `genai.py:pattern`).
pub fn trigger_regex() -> Regex {
    Regex::new(r"(?s)^(?:(.+?)\s)?\!\?(?:\s-i)?$").expect("static regex")
}

/// Parse an outgoing message into `(query, ignore_reply)` when it
/// matches the `!?` trigger.
pub fn parse_trigger(text: &str) -> Option<(String, bool)> {
    let re = trigger_regex();
    let caps = re.captures(text.trim())?;
    let query = caps
        .get(1)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();
    let ignore_reply = text.trim_end().ends_with("-i");
    Some((query, ignore_reply))
}

/// Stable conversation key scoped to chat and caller (Python
/// `GenAI.history_key`).
pub fn history_key(chat_id: i64, user_id: i64) -> String {
    format!("genai:{chat_id}:{user_id}")
}

fn build_runtime(ctx: &Ctx) -> AgentRuntime {
    let agent = Arc::new(UltroidAgent::new(
        Some(ctx.db.clone()),
        ctx.config.data_dir.clone(),
        None,
        None,
    ));
    let config = Arc::new(AgentConfig::new(ctx.db.clone()));
    let mut privileged = HashSet::new();
    privileged.insert(ctx.config.owner_id);
    AgentRuntime::new(
        agent,
        Some(config),
        Some(ctx.db.clone()),
        privileged,
        ctx.config.data_dir.clone(),
    )
}

/// Run one GenAI turn through the shared runtime and return the raw
/// response (used by `genai.rs` and `bot2bot.rs`).
pub async fn run_agent_request(
    ctx: &Ctx,
    chat_key: &str,
    prompt: &str,
    origin: &str,
) -> Result<AgentResponse> {
    let runtime = build_runtime(ctx);
    let controls = get_controls(Some(&ctx.db), &ctx.chat_id.to_string()).await;
    let mut req = AgentRequest::new(prompt.to_string(), chat_key.to_string());
    req.origin = origin.to_string();
    req.user_id = Some(ctx.sender_id);
    req.is_private = ctx.chat_type == ChatType::Private;
    req.use_tools = Some(controls.tools);
    req.max_rounds = controls.rounds;
    req.model = controls.model;
    req.temperature = Some(controls.temp);
    req.rich_messages_allowed = Some(ctx.client_kind == ClientKind::AsstBot);
    req.timeout_seconds = Some(105.0);
    Ok(runtime.runtime_run(&req).await)
}

/// Run one GenAI turn and render the response into the chat (Python
/// `GenAI.execute`).
pub async fn run_genai_turn(ctx: &Ctx, prompt: &str, ignore_reply: bool) -> Result<()> {
    let mut query = prompt.trim().to_string();
    if !ignore_reply {
        if let Some(rep) = &ctx.reply_to_text {
            let context = rep.trim();
            if !context.is_empty() {
                query = format!("Context from the replied message:\n{context}\n\n{query}");
            }
        }
    }
    if query.trim().is_empty() {
        ctx.reply(&ctx.get_str("genai_no_query")).await?;
        return Ok(());
    }
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    let key = history_key(ctx.chat_id, ctx.sender_id);
    let response = run_agent_request(ctx, &key, query.trim(), "genai").await?;
    render_response(ctx, status, &response).await
}

async fn render_response(ctx: &Ctx, status: i64, response: &AgentResponse) -> Result<()> {
    if let Some(err) = &response.error {
        ctx.edit(status, err).await?;
        return Ok(());
    }
    let content = response.content.trim();
    if content.is_empty() {
        ctx.edit(status, "(no response)").await?;
        return Ok(());
    }
    match RichDoc::markdown(content) {
        Ok(doc) => match ctx.send_rich_or_plain(&doc).await {
            Ok(_) => {
                let _ = ctx.delete(status).await;
                Ok(())
            }
            Err(e) => {
                ctx.edit(status, content).await?;
                Err(e)
            }
        },
        Err(_) => {
            ctx.edit(status, content).await?;
            Ok(())
        }
    }
}

/// Clear the caller's GenAI history (Python `GenAI.clear`).
pub async fn clear_history(ctx: &Ctx) -> Result<()> {
    let runtime = build_runtime(ctx);
    runtime
        .agent
        .clear_history(&history_key(ctx.chat_id, ctx.sender_id))
        .await;
    Ok(())
}

pub fn register(registry: &mut Registry) {
    let mut genai = def(
        "genai",
        "genai( (.*)|$)",
        "GenAI assistant: `.genai <prompt>`, `.genai clear`.",
    );
    genai.owner_only = true;
    reg_add(registry, genai, |ctx| Box::pin(genai_cmd(ctx)));

    reg_add(
        registry,
        def(
            "cleargenai",
            "cleargenai$",
            "Clear your GenAI conversation history.",
        ),
        |ctx| Box::pin(cleargenai_cmd(ctx)),
    );
}

async fn genai_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg == "clear" {
        clear_history(ctx).await?;
        ctx.reply(&ctx.get_str("genai_cleared")).await?;
        return Ok(());
    }
    run_genai_turn(ctx, arg, false).await
}

async fn cleargenai_cmd(ctx: &Ctx) -> Result<()> {
    clear_history(ctx).await?;
    ctx.delete(ctx.message_id).await?;
    ctx.reply(&ctx.get_str("genai_cleared")).await?;
    Ok(())
}

/// Outgoing-message trigger entry point (Python `GenAI.execute` on the
/// `!?` pattern): returns `Ok(true)` when the message matched the
/// trigger and was answered. The binary's outgoing-message path calls
/// this; `Registry` cannot match the bare `!?` pattern.
pub async fn check_trigger(ctx: &Ctx) -> Result<bool> {
    let Some((query, ignore_reply)) = parse_trigger(&ctx.text) else {
        return Ok(false);
    };
    if ctx.reply_to_text.is_some() && !ignore_reply {
        // The replied message has media attached — the runtime cannot
        // interpret attached media (Python `GenAI.execute` refuses this).
        // `Ctx` cannot tell whether the reply carries media, so a reply
        // with no text is treated as media.
        if ctx
            .reply_to_text
            .as_ref()
            .map(|t| t.trim().is_empty())
            .unwrap_or(true)
        {
            ctx.reply("This agent runtime cannot interpret attached media yet. Please send the relevant text.")
                .await?;
            return Ok(true);
        }
    }
    run_genai_turn(ctx, &query, ignore_reply).await?;
    Ok(true)
}
