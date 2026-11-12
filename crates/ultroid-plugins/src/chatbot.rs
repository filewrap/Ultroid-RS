//! Chatbot reply engines — port of `plugins/chatbot.py` (893 LOC).
//!
//! Three engines behind one dispatcher (`ReplyEngine`):
//! * `sol` — the Sol AI HTTP service (`SOL_BASE_URL`), per-user
//!   conversation sessions, JWT refresh-on-401, shared-conversation
//!   fallback, global error counting with shutdown after 3 failures.
//! * `chatbot` — Google PaLM `text-bison-001` (`GOOGLEAPI` udB key).
//! * `oracle` — the Mongo-backed Gemini oracle (`fns.get_oracle_reply`).
//!
//! Per-chat engine choice lives in `CHATBOT_ENGINE_<chat_id>`; per-user
//! enrolment in `SOL_USERS` / `CHATBOT_USERS` / `ORACLE_USERS`
//! (`{chat_id: [user_id]}` JSON maps); global Sol mode in
//! `SOL_GLOBAL_CHATS`; default replies in `DEFAULT_REPLIES`.
//!
//! The auto-reply listener (`_auto_reply` in Python, registered on the
//! userbot's `NewMessage` stream) cannot be registered through the
//! command `Registry`; the binary wires [`handle_auto_reply`] into its
//! incoming-message path instead.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::Timelike;
use regex::Regex;
use serde_json::Value;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, db_set, def, reg_add};

pub const SOL_BASE_URL: &str = "http://confidante-alb-150133398.ap-south-1.elb.amazonaws.com";
pub const SOL_FALLBACK_CONVO_ID: &str = "edd8adb8-c7bb-4c0f-883b-3a413e816ec2";
pub const SOL_MAX_ERRORS: i64 = 3;

const ENGINES: [&str; 3] = ["sol", "chatbot", "oracle"];
const ENGINE_LABELS: [(&str, &str); 3] =
    [("sol", "Sol"), ("chatbot", "ChatBot"), ("oracle", "Oracle")];

fn engine_label(engine: &str) -> &str {
    ENGINE_LABELS
        .iter()
        .find(|(k, _)| *k == engine)
        .map(|(_, v)| *v)
        .unwrap_or(engine)
}

fn sol_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(SolAI|SolAi|Sol|sol)\b").expect("sol name regex"))
}

// ── Small DB helpers ─────────────────────────────────────────────────────

async fn user_map(ctx: &Ctx, key: &str) -> HashMap<String, Vec<i64>> {
    db_get_json(ctx.db.as_ref(), key)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn set_user_map(ctx: &Ctx, key: &str, map: &HashMap<String, Vec<i64>>) -> Result<()> {
    db_set_json(ctx.db.as_ref(), key, map).await
}

async fn chat_bool_map(ctx: &Ctx, key: &str) -> HashMap<String, bool> {
    db_get_json(ctx.db.as_ref(), key)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn sol_disabled(ctx: &Ctx) -> bool {
    matches!(
        db_get(ctx, "SOL_DISABLED").await.ok().flatten().as_deref(),
        Some("true") | Some("1")
    )
}

async fn sol_shutdown(ctx: &Ctx) {
    let _ = db_set_json(
        ctx.db.as_ref(),
        "SOL_USERS",
        &HashMap::<String, Vec<i64>>::new(),
    )
    .await;
    let _ = db_set_json(
        ctx.db.as_ref(),
        "SOL_GLOBAL_CHATS",
        &HashMap::<String, bool>::new(),
    )
    .await;
    let _ = db_set(ctx, "SOL_DISABLED", "true").await;
    tracing::error!("Sol: shutting down after too many consecutive failures.");
}

async fn sol_increment_error(ctx: &Ctx) -> i64 {
    let count: i64 = db_get_json(ctx.db.as_ref(), "SOL_ERRORS_GLOBAL")
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
        + 1;
    let _ = db_set_json(ctx.db.as_ref(), "SOL_ERRORS_GLOBAL", &count).await;
    count
}

async fn sol_clear_errors(ctx: &Ctx) {
    let _ = db_set_json(ctx.db.as_ref(), "SOL_ERRORS_GLOBAL", &0i64).await;
}

// ── Sol HTTP ─────────────────────────────────────────────────────────────

async fn sol_refresh_auth(ctx: &Ctx) -> Option<String> {
    let refresh_token = db_get(ctx, "SOL_REFRESH_TOKEN").await.ok().flatten()?;
    if refresh_token.is_empty() {
        tracing::error!("Sol: no SOL_REFRESH_TOKEN stored, cannot refresh.");
        return None;
    }
    let resp = ctx
        .http
        .post(format!("{SOL_BASE_URL}/auth/refresh"))
        .json(&serde_json::json!({ "refreshToken": refresh_token }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        tracing::error!("Sol: refresh endpoint returned {}", resp.status());
        return None;
    }
    let data: Value = resp.json().await.ok()?;
    let new_token = data.get("jwt")?.as_str()?.to_string();
    if new_token.is_empty() {
        tracing::error!("Sol: refresh response missing jwt field");
        return None;
    }
    let _ = db_set(ctx, "SOL_TOKEN", &new_token).await;
    if let Some(new_refresh) = data.get("refresh_token").and_then(|v| v.as_str()) {
        let _ = db_set(ctx, "SOL_REFRESH_TOKEN", new_refresh).await;
    }
    tracing::info!("Sol: auth refreshed successfully.");
    Some(new_token)
}

/// Send one message. Returns `(Some(reply), false)` on success,
/// `(None, true)` on 401, `(None, false)` on any other failure.
async fn sol_send_message(
    ctx: &Ctx,
    token: &str,
    convo_id: &str,
    message: &str,
) -> (Option<String>, bool) {
    let resp = match ctx
        .http
        .post(format!("{SOL_BASE_URL}/conversations/message"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "conversationId": convo_id, "text": message }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Sol: send failed on convo {convo_id}: {e}");
            return (None, false);
        }
    };
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return (None, true);
    }
    let data: Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Sol: send failed on convo {convo_id}: {e}");
            return (None, false);
        }
    };
    match data.get("replyText").and_then(|v| v.as_str()) {
        Some(reply) if !reply.is_empty() => (Some(reply.to_string()), false),
        _ => {
            tracing::error!("Sol: empty replyText in response");
            (None, false)
        }
    }
}

/// Try sending once; on 401 refresh auth and retry once.
async fn sol_try_send(
    ctx: &Ctx,
    token: &str,
    convo_id: &str,
    message: &str,
    user_id: i64,
) -> Option<String> {
    let (reply, is_401) = sol_send_message(ctx, token, convo_id, message).await;
    if reply.is_some() {
        return reply;
    }
    if is_401 {
        tracing::warn!("Sol: 401 for user {user_id}, attempting token refresh...");
        let new_token = sol_refresh_auth(ctx).await?;
        if new_token.is_empty() {
            tracing::error!("Sol: token refresh failed — shutting down.");
            sol_shutdown(ctx).await;
            return None;
        }
        let (reply, _) = sol_send_message(ctx, &new_token, convo_id, message).await;
        return reply;
    }
    None
}

/// Full Sol flow: per-user session, shared-conversation fallback,
/// global error counting / shutdown. Mirrors `sol_ai()`.
pub async fn sol_ai(ctx: &Ctx, user_id: i64, message: &str) -> String {
    if sol_disabled(ctx).await {
        return "⚠️ AI is currently unavailable.".to_string();
    }
    let Some(token) = db_get(ctx, "SOL_TOKEN")
        .await
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
    else {
        return "⚠️ SOL_TOKEN is not set in your Ultroid database.".to_string();
    };

    let session_key = format!("SOL_SESSION_{user_id}");
    let mut convo_id = db_get(ctx, &session_key).await.ok().flatten();
    if convo_id.is_none() {
        let resp = ctx
            .http
            .post(format!("{SOL_BASE_URL}/conversations"))
            .header("authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({}))
            .send()
            .await;
        match resp {
            Ok(r) => match r.json::<Value>().await {
                Ok(data) => {
                    if let Some(id) = data
                        .get("conversation")
                        .and_then(|c| c.get("id"))
                        .and_then(|v| v.as_str())
                    {
                        let id = id.to_string();
                        let _ = db_set(ctx, &session_key, &id).await;
                        tracing::info!("Sol: new session {id} for user {user_id}");
                        convo_id = Some(id);
                    }
                }
                Err(e) => tracing::error!("Sol: failed to create session for {user_id}: {e}"),
            },
            Err(e) => tracing::error!("Sol: failed to create session for {user_id}: {e}"),
        }
    }

    if let Some(convo) = &convo_id {
        if let Some(reply) = sol_try_send(ctx, &token, convo, message, user_id).await {
            sol_clear_errors(ctx).await;
            // The alias is read synchronously from the DB snapshot path
            // below; mask with the stored alias value.
            let alias = db_get(ctx, "SOL_ALIAS")
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "Aria".to_string());
            return sol_name_re()
                .replace_all(&reply, alias.as_str())
                .into_owned();
        }
        // Session bad — drop it so the next call gets a fresh one.
        let _ = ctx.db.del(&session_key).await;
    }

    if sol_disabled(ctx).await {
        return "⚠️ AI is currently unavailable.".to_string();
    }

    tracing::warn!("Sol: falling back to shared convo for user {user_id}");
    let token = db_get(ctx, "SOL_TOKEN")
        .await
        .ok()
        .flatten()
        .unwrap_or(token);
    if let Some(reply) = sol_try_send(ctx, &token, SOL_FALLBACK_CONVO_ID, message, user_id).await {
        sol_clear_errors(ctx).await;
        let alias = db_get(ctx, "SOL_ALIAS")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "Aria".to_string());
        return sol_name_re()
            .replace_all(&reply, alias.as_str())
            .into_owned();
    }

    if sol_disabled(ctx).await {
        return "⚠️ AI is currently unavailable.".to_string();
    }

    let error_count = sol_increment_error(ctx).await;
    tracing::error!("Sol: global error count is now {error_count}/{SOL_MAX_ERRORS}");
    if error_count >= SOL_MAX_ERRORS {
        sol_shutdown(ctx).await;
        return "⚠️ AI failed too many times and has been disabled.".to_string();
    }
    format!("⚠️ AI is unavailable right now. ({error_count}/{SOL_MAX_ERRORS})")
}

// ── ChatBot (Google PaLM) + Oracle engines ───────────────────────────────

/// Port of `fns.get_chatbot_reply`: PaLM `text-bison-001:generateText`
/// with the `GOOGLEAPI` udB key.
pub async fn get_chatbot_reply(ctx: &Ctx, message: &str) -> String {
    let Some(key) = db_get(ctx, "GOOGLEAPI")
        .await
        .ok()
        .flatten()
        .filter(|k| !k.is_empty())
    else {
        return "Sorry you need to set a GOOGLEAPI key to use this chatbot".to_string();
    };
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta3/models/text-bison-001:generateText?key={key}"
    );
    let resp = match ctx
        .http
        .post(&url)
        .json(&serde_json::json!({ "prompt": { "text": message } }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("chatbot HTTP error: {e}");
            return "Error connecting to the chatbot server.".to_string();
        }
    };
    let data: Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("chatbot JSON error: {e}");
            return "Error decoding JSON response from the chatbot server.".to_string();
        }
    };
    match data
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("output"))
        .and_then(|v| v.as_str())
    {
        Some(reply) => reply.to_string(),
        None => {
            tracing::warn!("Unexpected JSON format in the chatbot response.");
            "Unexpected response from the chatbot server.".to_string()
        }
    }
}

/// Port of `fns.get_oracle_reply`.
///
/// Without `MONGO_URI` Python returns the sentinel below; with one it
/// calls the external `ChatBot(...).get_response_gemini_oracle` service,
/// whose HTTP surface is not in the source tree, so the engine reports
/// itself unavailable (the dispatcher then falls back to a default reply).
pub async fn get_oracle_reply(ctx: &Ctx, _query: &str, _user_id: i64) -> Result<String> {
    let mongo = db_get(ctx, "MONGO_URI").await.ok().flatten();
    if mongo.as_deref().unwrap_or("").is_empty() {
        return Ok("You cannot use this without setting a MONGO_URI first".to_string());
    }
    // TODO(responder-op): oracle reply requires the ChatBot oracle service
    // (Python: chatbot.py:get_oracle_reply -> pyUltroid/fns/tools.py:get_oracle_reply).
    Err(Error::Other(
        "oracle reply requires the ChatBot oracle service (Python: plugins/chatbot.py:get_oracle_reply, pyUltroid/fns/tools.py:get_oracle_reply) — no HTTP endpoint in source".to_string(),
    ))
}

// ── ReplyEngine ──────────────────────────────────────────────────────────

/// Per-chat engine persistence key.
fn engine_key(chat_id: i64) -> String {
    format!("CHATBOT_ENGINE_{chat_id}")
}

pub async fn get_engine(ctx: &Ctx, chat_id: i64) -> String {
    db_get(ctx, &engine_key(chat_id))
        .await
        .ok()
        .flatten()
        .map(|e| e.to_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "sol".to_string())
}

pub async fn set_engine(ctx: &Ctx, chat_id: i64, engine: &str) -> Result<()> {
    db_set(ctx, &engine_key(chat_id), engine).await
}

/// Pick a random default reply for *engine* and substitute
/// `{name}` / `{chat}` / `{time}` templates.
async fn default_reply(
    ctx: &Ctx,
    engine: &str,
    sender_name: &str,
    chat_title: &str,
) -> Option<String> {
    let defaults: HashMap<String, Vec<String>> = db_get_json(ctx.db.as_ref(), "DEFAULT_REPLIES")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let replies = defaults.get(engine)?;
    if replies.is_empty() {
        return None;
    }
    let idx = (rand::random::<u64>() as usize) % replies.len();
    let template = &replies[idx];
    let now = chrono::Local::now();
    let time = format!("{:02}:{:02}", now.hour(), now.minute());
    let mut out = template.replace(
        "{name}",
        if sender_name.is_empty() {
            "friend"
        } else {
            sender_name
        },
    );
    out = out.replace(
        "{chat}",
        if chat_title.is_empty() {
            "this chat"
        } else {
            chat_title
        },
    );
    out = out.replace("{time}", &time);
    Some(out)
}

/// Dispatcher — pick the engine and delegate. Returns `(reply, success)`.
/// On engine failure a configured default reply is substituted.
pub async fn get_reply(
    ctx: &Ctx,
    message: &str,
    engine: Option<&str>,
    chat_id: Option<i64>,
    user_id: Option<i64>,
    sender_name: &str,
    chat_title: &str,
) -> (String, bool) {
    let engine = match engine {
        Some(e) => e.to_lowercase(),
        None => match chat_id {
            Some(id) => get_engine(ctx, id).await,
            None => "sol".to_string(),
        },
    };
    let (reply, ok): (Option<String>, bool) = match engine.as_str() {
        "chatbot" => {
            let r = get_chatbot_reply(ctx, message).await;
            let ok = !r.is_empty() && !r.contains("Sorry you need");
            (Some(r), ok)
        }
        "oracle" => match get_oracle_reply(ctx, message, user_id.unwrap_or(ctx.sender_id)).await {
            Ok(r) => {
                let ok = !r.is_empty() && !r.to_lowercase().contains("cannot use this without");
                (Some(r), ok)
            }
            Err(e) => {
                tracing::error!("ReplyEngine: oracle error: {e}");
                (None, false)
            }
        },
        _ => {
            let r = sol_ai(ctx, user_id.unwrap_or(ctx.sender_id), message).await;
            let ok = !r.is_empty() && !r.contains('⚠');
            (Some(r), ok)
        }
    };

    if ok {
        if let Some(r) = reply {
            return (r, true);
        }
    }
    if let Some(fallback) = default_reply(ctx, &engine, sender_name, chat_title).await {
        return (fallback, false);
    }
    (
        reply.unwrap_or_else(|| format!("⚠️ {} engine is unavailable.", engine_label(&engine))),
        false,
    )
}

// ── Shared management helpers ────────────────────────────────────────────

/// Resolve the target user for add/remove commands.
///
/// Python resolves a replied-to sender, or parses an id/username from
/// the args. A reply target comes from `ctx.reply_to_sender_id`; a
/// `@username` arg resolves through the `Responder`.
async fn resolve_target_id(ctx: &Ctx) -> Result<i64> {
    let arg = ctx.args.trim();
    if !arg.is_empty() {
        let first = arg.split_whitespace().next().unwrap_or("");
        let cleaned = first.trim_start_matches('@');
        if let Ok(id) = cleaned.parse::<i64>() {
            return Ok(id);
        }
        return Ok(ctx.responder.resolve_user(cleaned).await?.id);
    }
    if ctx.reply_to_id.is_some() {
        if let Some(sender_id) = ctx.reply_to_sender_id {
            return Ok(sender_id);
        }
    }
    Err(Error::Other(ctx.get_str("chab_1")))
}

async fn manage_ai_user(ctx: &Ctx, db_key: &str, label: &str, add: bool) -> Result<()> {
    let user_id = resolve_target_id(ctx).await?;
    let mut key = user_map(ctx, db_key).await;
    let chat = ctx.chat_id.to_string();
    if add {
        let users = key.entry(chat).or_default();
        if !users.contains(&user_id) {
            users.push(user_id);
        }
        // Adding to SOL_USERS removes the user from the other two lists.
        if db_key == "SOL_USERS" {
            for other in ["CHATBOT_USERS", "ORACLE_USERS"] {
                let mut map = user_map(ctx, other).await;
                if let Some(users) = map.get_mut(&ctx.chat_id.to_string()) {
                    users.retain(|u| *u != user_id);
                    if users.is_empty() {
                        map.remove(&ctx.chat_id.to_string());
                    }
                    let _ = set_user_map(ctx, other, &map).await;
                }
            }
        }
    } else {
        if let Some(users) = key.get_mut(&chat) {
            users.retain(|u| *u != user_id);
            if users.is_empty() {
                key.remove(&chat);
            }
        }
    }
    set_user_map(ctx, db_key, &key).await?;
    let verb = if add { "add" } else { "remov" };
    ctx.reply(&format!(
        "**{label}:**\n{verb}ed [{user_id}](tg://user?id={user_id})"
    ))
    .await?;
    Ok(())
}

async fn list_ai_users(ctx: &Ctx, db_key: &str, label: &str) -> Result<()> {
    let key = user_map(ctx, db_key).await;
    let users = key
        .get(&ctx.chat_id.to_string())
        .cloned()
        .unwrap_or_default();
    if users.is_empty() {
        ctx.reply(&ctx.get_str("chab_2")).await?;
        return Ok(());
    }
    let mut msg = format!("**{label} Enabled Users In This Chat:**\n\n");
    for id in users {
        msg.push_str(&format!("• [{id}](tg://user?id={id})\n"));
    }
    ctx.reply(&msg).await?;
    Ok(())
}

async fn rep_query(ctx: &Ctx) -> Result<String> {
    if let Some(text) = &ctx.reply_to_text {
        if !text.trim().is_empty() {
            return Ok(text.trim().to_string());
        }
    }
    let arg = ctx.args.trim();
    if !arg.is_empty() {
        return Ok(arg.to_string());
    }
    Err(Error::Other(ctx.get_str("tban_1")))
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_repsol(ctx: &Ctx) -> Result<()> {
    let message = rep_query(ctx).await?;
    let reply = sol_ai(ctx, ctx.sender_id, &message).await;
    ctx.reply(&reply).await?;
    Ok(())
}

async fn cmd_addsol(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "SOL_USERS", "Sol", true).await
}

async fn cmd_remsol(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "SOL_USERS", "Sol", false).await
}

async fn cmd_listsol(ctx: &Ctx) -> Result<()> {
    list_ai_users(ctx, "SOL_USERS", "Sol").await
}

async fn cmd_solmode(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_lowercase();
    let mut key = chat_bool_map(ctx, "SOL_GLOBAL_CHATS").await;
    match arg.as_str() {
        "on" => {
            key.insert(ctx.chat_id.to_string(), true);
            db_set_json(ctx.db.as_ref(), "SOL_GLOBAL_CHATS", &key).await?;
            ctx.reply("**Sol Global Mode:** `ON` — all messages in this chat will go to Sol.")
                .await?;
        }
        "off" => {
            key.remove(&ctx.chat_id.to_string());
            db_set_json(ctx.db.as_ref(), "SOL_GLOBAL_CHATS", &key).await?;
            ctx.reply("**Sol Global Mode:** `OFF`").await?;
        }
        "" => {
            let current = if key.get(&ctx.chat_id.to_string()).copied().unwrap_or(false) {
                "ON"
            } else {
                "OFF"
            };
            ctx.reply(&format!(
                "**Sol Global Mode:** currently `{current}`\nUsage: `.solmode on` or `.solmode off`"
            ))
            .await?;
        }
        _ => {
            ctx.reply("Usage: `.solmode on` or `.solmode off`").await?;
        }
    }
    Ok(())
}

async fn cmd_solalias(ctx: &Ctx) -> Result<()> {
    let alias = ctx.args.trim();
    if alias.is_empty() {
        let current = db_get(ctx, "SOL_ALIAS")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "Aria".to_string());
        ctx.reply(&format!(
            "**AI Alias:** currently `{current}`\nUsage: `.solalias <name>`"
        ))
        .await?;
        return Ok(());
    }
    db_set(ctx, "SOL_ALIAS", alias).await?;
    ctx.reply(&format!("**AI Alias:** set to `{alias}`"))
        .await?;
    Ok(())
}

async fn cmd_solreset(ctx: &Ctx) -> Result<()> {
    db_set(ctx, "SOL_DISABLED", "false").await?;
    db_set_json(ctx.db.as_ref(), "SOL_ERRORS_GLOBAL", &0i64).await?;
    ctx.reply("**Sol:** re-enabled. Error counter reset.")
        .await?;
    Ok(())
}

async fn cmd_repai(ctx: &Ctx) -> Result<()> {
    let message = rep_query(ctx).await?;
    let reply = get_chatbot_reply(ctx, &message).await;
    ctx.reply(&reply).await?;
    Ok(())
}

async fn cmd_addai(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "CHATBOT_USERS", "ChatBot", true).await
}

async fn cmd_remai(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "CHATBOT_USERS", "ChatBot", false).await
}

async fn cmd_listai(ctx: &Ctx) -> Result<()> {
    list_ai_users(ctx, "CHATBOT_USERS", "ChatBot").await
}

async fn cmd_repoai(ctx: &Ctx) -> Result<()> {
    let message = rep_query(ctx).await?;
    let reply = get_oracle_reply(ctx, &message, ctx.sender_id).await?;
    ctx.reply(&reply).await?;
    Ok(())
}

async fn cmd_addoai(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "ORACLE_USERS", "Oracle", true).await
}

async fn cmd_remoai(ctx: &Ctx) -> Result<()> {
    manage_ai_user(ctx, "ORACLE_USERS", "Oracle", false).await
}

async fn cmd_listoai(ctx: &Ctx) -> Result<()> {
    list_ai_users(ctx, "ORACLE_USERS", "Oracle").await
}

async fn cmd_chatai(ctx: &Ctx) -> Result<()> {
    let choice = ctx.args.trim();
    if choice.is_empty() {
        let current = get_engine(ctx, ctx.chat_id).await;
        ctx.reply(&format!(
            "**AI Engine for this chat:** `{current}`\n\nAvailable engines: `sol`, `chatbot`, `oracle`\nUsage: `.chatai <engine>` to switch."
        ))
        .await?;
        return Ok(());
    }
    let choice = choice
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if !ENGINES.contains(&choice.as_str()) {
        ctx.reply(&format!(
            "❌ Invalid engine `{choice}`.\nChoose from: {}",
            ENGINES
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    set_engine(ctx, ctx.chat_id, &choice).await?;
    ctx.reply(&format!(
        "**AI Engine** set to `{}` for this chat.",
        engine_label(&choice)
    ))
    .await?;
    Ok(())
}

async fn cmd_chatsettings(ctx: &Ctx) -> Result<()> {
    let chat_id = ctx.chat_id;
    let current_engine = get_engine(ctx, chat_id).await;
    let sleep: f64 = db_get_json(ctx.db.as_ref(), "CHATBOT_SLEEP")
        .await
        .ok()
        .flatten()
        .unwrap_or(1.5);
    let sol_global = chat_bool_map(ctx, "SOL_GLOBAL_CHATS")
        .await
        .get(&chat_id.to_string())
        .copied()
        .unwrap_or(false);
    let sol_users = user_map(ctx, "SOL_USERS")
        .await
        .get(&chat_id.to_string())
        .map(|v| v.len())
        .unwrap_or(0);
    let cb_users = user_map(ctx, "CHATBOT_USERS")
        .await
        .get(&chat_id.to_string())
        .map(|v| v.len())
        .unwrap_or(0);
    let or_users = user_map(ctx, "ORACLE_USERS")
        .await
        .get(&chat_id.to_string())
        .map(|v| v.len())
        .unwrap_or(0);
    let defaults: HashMap<String, Vec<String>> = db_get_json(ctx.db.as_ref(), "DEFAULT_REPLIES")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let text = format!(
        "**Chatbot Settings** — `{chat_id}`\n\n• **Engine:** `{current_engine}`\n• **Sleep delay:** `{sleep}s`\n• **Sol global mode:** `{}`\n\n**Enabled users:**\n• Sol: `{sol_users}`\n• ChatBot: `{cb_users}`\n• Oracle: `{or_users}`\n\n**Default replies:**\n• Sol: `{}`\n• ChatBot: `{}`\n• Oracle: `{}`\n",
        if sol_global { "ON" } else { "OFF" },
        defaults.get("sol").map(|v| v.len()).unwrap_or(0),
        defaults.get("chatbot").map(|v| v.len()).unwrap_or(0),
        defaults.get("oracle").map(|v| v.len()).unwrap_or(0),
    );
    ctx.reply(&text).await?;
    Ok(())
}

async fn cmd_setreply(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply("Usage: `.setreply <engine> <text>`\nEngines: `sol`, `chatbot`, `oracle`\nTemplates: `{name}`, `{chat}`, `{time}`").await?;
        return Ok(());
    }
    let mut parts = arg.splitn(2, char::is_whitespace);
    let eng = parts.next().unwrap_or("").to_lowercase();
    let text = parts.next().unwrap_or("").trim();
    if text.is_empty() {
        ctx.reply("Usage: `.setreply <engine> <text>`").await?;
        return Ok(());
    }
    if !ENGINES.contains(&eng.as_str()) {
        ctx.reply(&format!(
            "❌ Invalid engine. Choose from: {}",
            ENGINES
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    let mut defaults: HashMap<String, Vec<String>> =
        db_get_json(ctx.db.as_ref(), "DEFAULT_REPLIES")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    defaults
        .entry(eng.clone())
        .or_default()
        .push(text.to_string());
    let idx = defaults[&eng].len();
    db_set_json(ctx.db.as_ref(), "DEFAULT_REPLIES", &defaults).await?;
    ctx.reply(&format!(
        "**Default reply** added for `{}` (#{idx}).",
        engine_label(&eng)
    ))
    .await?;
    Ok(())
}

async fn cmd_delreply(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply("Usage: `.delreply <engine> <index>`").await?;
        return Ok(());
    }
    let mut parts = arg.split_whitespace();
    let eng = parts.next().unwrap_or("").to_lowercase();
    let idx_s = parts.next().unwrap_or("");
    if parts.next().is_some() {
        ctx.reply("Usage: `.delreply <engine> <index>`").await?;
        return Ok(());
    }
    let idx: usize = match idx_s.parse() {
        Ok(n) => n,
        Err(_) => {
            ctx.reply("Index must be a number.").await?;
            return Ok(());
        }
    };
    if !ENGINES.contains(&eng.as_str()) {
        ctx.reply(&format!(
            "❌ Invalid engine. Choose from: {}",
            ENGINES
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    let mut defaults: HashMap<String, Vec<String>> =
        db_get_json(ctx.db.as_ref(), "DEFAULT_REPLIES")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
    let replies = defaults.get(&eng).cloned().unwrap_or_default();
    if replies.is_empty() {
        ctx.reply(&format!(
            "No default replies set for `{}`.",
            engine_label(&eng)
        ))
        .await?;
        return Ok(());
    }
    if idx < 1 || idx > replies.len() {
        ctx.reply(&format!(
            "Index out of range. Valid: `1`–`{}`.",
            replies.len()
        ))
        .await?;
        return Ok(());
    }
    let mut replies = replies;
    replies.remove(idx - 1);
    defaults.insert(eng.clone(), replies);
    db_set_json(ctx.db.as_ref(), "DEFAULT_REPLIES", &defaults).await?;
    ctx.reply(&format!(
        "**Removed** default reply #{idx} for `{}`.",
        engine_label(&eng)
    ))
    .await?;
    Ok(())
}

async fn cmd_listreplies(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply(&format!(
            "Usage: `.listreplies <engine>`\nEngines: {}",
            ENGINES
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    let eng = arg.split_whitespace().next().unwrap_or("").to_lowercase();
    if !ENGINES.contains(&eng.as_str()) {
        ctx.reply(&format!(
            "❌ Invalid engine. Choose from: {}",
            ENGINES
                .iter()
                .map(|e| format!("`{e}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    let defaults: HashMap<String, Vec<String>> = db_get_json(ctx.db.as_ref(), "DEFAULT_REPLIES")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let replies = defaults.get(&eng).cloned().unwrap_or_default();
    if replies.is_empty() {
        ctx.reply(&format!(
            "No default replies set for `{}`.",
            engine_label(&eng)
        ))
        .await?;
        return Ok(());
    }
    let mut msg = format!("**Default replies for {}:**\n\n", engine_label(&eng));
    for (i, r) in replies.iter().enumerate() {
        msg.push_str(&format!("{}. `{r}`\n", i + 1));
    }
    ctx.reply(&msg).await?;
    Ok(())
}

/// Track username changes (was in `_chatactions`, moved to chatbot.py).
/// Stored in `USERNAME_DB` as `{user_id: {username, first_name}}`.
pub async fn uname_stuff(ctx: &Ctx, user_id: i64, username: &str, first_name: &str) {
    if user_id == 0 || username.trim().is_empty() {
        return;
    }
    let mut db_map: HashMap<String, Value> = db_get_json(ctx.db.as_ref(), "USERNAME_DB")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let key = user_id.to_string();
    let changed = db_map
        .get(&key)
        .and_then(|v| v.get("username"))
        .and_then(|v| v.as_str())
        .map(|u| u != username)
        .unwrap_or(true);
    if changed {
        db_map.insert(
            key,
            serde_json::json!({ "username": username, "first_name": first_name }),
        );
        let _ = db_set_json(ctx.db.as_ref(), "USERNAME_DB", &db_map).await;
    }
}

/// The consolidated auto-reply handler (`_auto_reply` in Python).
///
/// The binary calls this for every incoming (non-outgoing) message; it
/// returns `true` when a reply was sent. `sender_is_bot` / username
/// tracking come from the update itself, which the command `Ctx` does
/// not carry.
pub async fn handle_auto_reply(
    ctx: &Ctx,
    sender_first_name: &str,
    sender_username: Option<&str>,
    sender_is_bot: bool,
) -> Result<bool> {
    if sender_is_bot {
        return Ok(false);
    }
    if ctx.text.trim().is_empty() {
        return Ok(false);
    }
    let sender_id = ctx.sender_id;
    let chat_id = ctx.chat_id;

    let sol_global = chat_bool_map(ctx, "SOL_GLOBAL_CHATS").await;
    let sol_key = user_map(ctx, "SOL_USERS").await;
    let cb_key = user_map(ctx, "CHATBOT_USERS").await;
    let or_key = user_map(ctx, "ORACLE_USERS").await;

    let engine = if sol_global
        .get(&chat_id.to_string())
        .copied()
        .unwrap_or(false)
    {
        Some("sol")
    } else if sol_key
        .get(&chat_id.to_string())
        .map(|v| v.contains(&sender_id))
        .unwrap_or(false)
    {
        Some("sol")
    } else if cb_key
        .get(&chat_id.to_string())
        .map(|v| v.contains(&sender_id))
        .unwrap_or(false)
    {
        Some("chatbot")
    } else if or_key
        .get(&chat_id.to_string())
        .map(|v| v.contains(&sender_id))
        .unwrap_or(false)
    {
        Some("oracle")
    } else {
        None
    };

    if let Some(engine) = engine {
        let (msg, _) = get_reply(
            ctx,
            &ctx.text,
            Some(engine),
            Some(chat_id),
            Some(sender_id),
            sender_first_name,
            "",
        )
        .await;
        let sleep: f64 = db_get_json(ctx.db.as_ref(), "CHATBOT_SLEEP")
            .await
            .ok()
            .flatten()
            .unwrap_or(1.5);
        tokio::time::sleep(std::time::Duration::from_secs_f64(sleep.max(0.0))).await;
        ctx.reply(&msg).await?;
        if let Some(username) = sender_username {
            uname_stuff(ctx, sender_id, username, sender_first_name).await;
        }
        return Ok(true);
    }
    if let Some(username) = sender_username {
        uname_stuff(ctx, sender_id, username, sender_first_name).await;
    }
    Ok(false)
}

pub fn register(registry: &mut Registry) {
    let help = "repsol/addsol/remsol/listsol/solmode/solalias/solreset, repai/addai/remai/listai, repoai/addoai/remoai/listoai, chatai/chatsettings/setreply/delreply/listreplies — Sol / ChatBot / Oracle reply engines";
    reg_add(registry, def("repsol", "repsol", help), |ctx| {
        Box::pin(cmd_repsol(ctx))
    });
    reg_add(registry, def("addsol", "addsol", help), |ctx| {
        Box::pin(cmd_addsol(ctx))
    });
    reg_add(registry, def("remsol", "remsol", help), |ctx| {
        Box::pin(cmd_remsol(ctx))
    });
    reg_add(registry, def("listsol", "listsol", help), |ctx| {
        Box::pin(cmd_listsol(ctx))
    });
    reg_add(registry, def("solmode", "solmode", help), |ctx| {
        Box::pin(cmd_solmode(ctx))
    });
    reg_add(registry, def("solalias", "solalias", help), |ctx| {
        Box::pin(cmd_solalias(ctx))
    });
    reg_add(registry, def("solreset", "solreset", help), |ctx| {
        Box::pin(cmd_solreset(ctx))
    });
    reg_add(registry, def("repai", "repai", help), |ctx| {
        Box::pin(cmd_repai(ctx))
    });
    reg_add(registry, def("addai", "addai", help), |ctx| {
        Box::pin(cmd_addai(ctx))
    });
    reg_add(registry, def("remai", "remai", help), |ctx| {
        Box::pin(cmd_remai(ctx))
    });
    reg_add(registry, def("listai", "listai", help), |ctx| {
        Box::pin(cmd_listai(ctx))
    });
    reg_add(registry, def("repoai", "repoai", help), |ctx| {
        Box::pin(cmd_repoai(ctx))
    });
    reg_add(registry, def("addoai", "addoai", help), |ctx| {
        Box::pin(cmd_addoai(ctx))
    });
    reg_add(registry, def("remoai", "remoai", help), |ctx| {
        Box::pin(cmd_remoai(ctx))
    });
    reg_add(registry, def("listoai", "listoai", help), |ctx| {
        Box::pin(cmd_listoai(ctx))
    });
    reg_add(registry, def("chatai", "chatai( (.*)|$)", help), |ctx| {
        Box::pin(cmd_chatai(ctx))
    });
    reg_add(registry, def("chatsettings", "chatsettings", help), |ctx| {
        Box::pin(cmd_chatsettings(ctx))
    });
    reg_add(
        registry,
        def("setreply", "setreply( (.*)|$)", help),
        |ctx| Box::pin(cmd_setreply(ctx)),
    );
    reg_add(
        registry,
        def("delreply", "delreply( (.*)|$)", help),
        |ctx| Box::pin(cmd_delreply(ctx)),
    );

    reg_add(
        registry,
        def("listreplies", "listreplies( (.*)|$)", help),
        |ctx| Box::pin(cmd_listreplies(ctx)),
    );
}
