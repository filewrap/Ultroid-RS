//! Ephemeral messages / whispers / welcome — port of `plugins/ephemeral.py` (959 LOC).
//!
//! Python delivers these through the Telethon fork's `ephemeral.*` TL
//! requests (`SendMessageRequest` / `DeleteMessageRequest` /
//! `GetWelcomeMessagesRequest` / …), which have no counterpart on the
//! command `Responder` surface. The routing, argument parsing, receiver
//! resolution, welcome-text state and inline pending store are ported
//! faithfully; every actual ephemeral send/delete/list call returns the
//! honest `TODO(responder-op)` error at its call site.
//!
//! The assistant-bot variants (`/eph`, `/whisper` via `asst_cmd`) share
//! the same patterns and handlers as the user-account ones — the client
//! they run on is a binary routing concern, so they are registered once.
//! The inline-query articles and callbacks (`eph_inline`,
//! `whisper_inline`, `eph_inline_send`, `whisper_inline_send`) and the
//! raw-update join listener cannot be registered through the command
//! `Registry`; the pending store ([`store_pending`]/[`take_pending`])
//! and [`handle_join_welcome`] are exposed for the binary/assistant to
//! wire.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use ultroid_core::rich::RichDoc;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

const PENDING_TTL_SECS: u64 = 300;

/// One inline-pending delivery payload: text is kept in-memory only and
/// never rendered into the inline result (Python `_PENDING`).
#[derive(Debug, Clone)]
pub struct PendingPayload {
    pub text: String,
    /// `None` → broadcast ephemeral; `Some(raw)` → whisper receiver token
    /// (`@user`, id, phone) resolved at delivery time.
    pub receiver: Option<String>,
    pub expires_at: u64,
}

fn pending_store() -> &'static Mutex<HashMap<String, PendingPayload>> {
    static STORE: OnceLock<Mutex<HashMap<String, PendingPayload>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending_seq() -> &'static AtomicU64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    &SEQ
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Store an inline delivery payload; return its monotonic key.
pub fn store_pending(text: &str, receiver: Option<&str>) -> String {
    prune_pending();
    let key = pending_seq().fetch_add(1, Ordering::SeqCst).to_string();
    pending_store().lock().expect("pending lock").insert(
        key.clone(),
        PendingPayload {
            text: text.to_string(),
            receiver: receiver.map(|r| r.to_string()),
            expires_at: now_secs() + PENDING_TTL_SECS,
        },
    );
    key
}

/// Take (and remove) a pending payload, `None` when missing/expired.
pub fn take_pending(key: &str) -> Option<PendingPayload> {
    let payload = pending_store().lock().expect("pending lock").remove(key)?;
    if now_secs() > payload.expires_at {
        return None;
    }
    Some(payload)
}

fn prune_pending() {
    let now = now_secs();
    pending_store()
        .lock()
        .expect("pending lock")
        .retain(|_, v| now <= v.expires_at);
}

/// Welcome text per chat (Python `_WELCOME_DATA`, in-memory).
fn welcome_data() -> &'static Mutex<HashMap<i64, String>> {
    static DATA: OnceLock<Mutex<HashMap<i64, String>>> = OnceLock::new();
    DATA.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn welcome_text(chat_id: i64) -> Option<String> {
    welcome_data()
        .lock()
        .expect("welcome lock")
        .get(&chat_id)
        .cloned()
}

fn set_welcome_text(chat_id: i64, text: Option<&str>) {
    let mut data = welcome_data().lock().expect("welcome lock");
    match text {
        Some(t) => {
            data.insert(chat_id, t.to_string());
        }
        None => {
            data.remove(&chat_id);
        }
    }
}

// ── Pure helpers ─────────────────────────────────────────────────────────

/// True when a leading token is unambiguously a receiver, not text.
pub fn looks_like_receiver(token: &str) -> bool {
    if token.starts_with('@') || token.starts_with('+') {
        return true;
    }
    if !token.is_empty()
        && token
            .trim_start_matches(|c| c == '+' || c == '-')
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return true;
    }
    matches!(token.to_lowercase().as_str(), "me" | "self")
}

/// Safe human label for a resolved user.
pub fn display_receiver(username: Option<&str>, first_name: Option<&str>, id: i64) -> String {
    if let Some(u) = username.filter(|u| !u.is_empty()) {
        return format!("@{u}");
    }
    if let Some(f) = first_name.filter(|f| !f.is_empty()) {
        return f.to_string();
    }
    id.to_string()
}

fn usage(pattern: &str) -> String {
    format!(".{pattern}")
}

/// The rich HTML payload Python builds for a strong whisper.
pub fn strong_whisper_html(cleaned_text: &str) -> String {
    format!("<blockquote expandable><b>🔒 Private Whisper</b></blockquote>\n\n{cleaned_text}")
}

pub fn welcome_rich_html(chat_title: &str, cleaned_text: &str) -> String {
    format!(
        "<blockquote expandable><b>👋 Welcome to {chat_title}</b></blockquote>\n\n{cleaned_text}"
    )
}

// ── Delivery primitives (Responder-op TODOs) ─────────────────────────────

/// Deliver one ephemeral message.
///
/// `receiver: None` → broadcast (everyone in the chat sees it);
/// `Some(user_id)` → whisper (only that user sees it). `welcome`
/// marks it as a Telegram welcome message; `noforwards` blocks
/// forwarding.
#[allow(clippy::too_many_arguments)]
pub fn send_ephemeral(
    _peer_chat_id: i64,
    _text: &str,
    _rich: Option<&RichDoc>,
    _receiver: Option<i64>,
    _reply_to: Option<i64>,
    _noforwards: bool,
    _welcome: bool,
) -> Result<i64> {
    // TODO(responder-op): ephemeral.sendMessage
    // (Python: plugins/ephemeral.py:_send_ephemeral via telethon
    // ephemeral.SendMessageRequest).
    Err(Error::Other(
        "sending an ephemeral message requires a binary Responder op (Python: plugins/ephemeral.py:_send_ephemeral via ephemeral.SendMessageRequest)".to_string(),
    ))
}

/// Delete a broadcast ephemeral message by id.
pub fn delete_ephemeral(_peer_chat_id: i64, _message_id: i64) -> Result<()> {
    // TODO(responder-op): ephemeral.deleteMessage
    // (Python: plugins/ephemeral.py:_delete_ephemeral).
    Err(Error::Other(
        "deleting an ephemeral message requires a binary Responder op (Python: plugins/ephemeral.py:_delete_ephemeral via ephemeral.DeleteMessageRequest)".to_string(),
    ))
}

/// Fetch the saved welcome messages for a chat.
pub fn get_welcome_messages(_peer_chat_id: i64) -> Result<Option<Vec<i64>>> {
    // TODO(responder-op): ephemeral.getWelcomeMessages
    // (Python: plugins/ephemeral.py:_get_welcome_messages via
    // ephemeral.GetWelcomeMessagesRequest).
    Err(Error::Other(
        "listing welcome messages requires a binary Responder op (Python: plugins/ephemeral.py:_get_welcome_messages via ephemeral.GetWelcomeMessagesRequest)".to_string(),
    ))
}

/// Delete all saved welcome messages for a chat.
pub fn delete_all_welcome_messages(_peer_chat_id: i64) -> Result<()> {
    // TODO(responder-op): ephemeral.deleteAllWelcomeMessages
    // (Python: plugins/ephemeral.py:_delete_all_welcome_messages).
    Err(Error::Other(
        "deleting welcome messages requires a binary Responder op (Python: plugins/ephemeral.py:_delete_all_welcome_messages via ephemeral.DeleteAllWelcomeMessagesRequest)".to_string(),
    ))
}

/// Edit a saved welcome message.
pub fn edit_welcome_message(_peer_chat_id: i64, _message_id: i64, _text: &str) -> Result<()> {
    // TODO(responder-op): ephemeral.editMessage (welcome)
    // (Python: plugins/ephemeral.py:_edit_welcome_message via
    // ephemeral.EditMessageRequest).
    Err(Error::Other(
        "editing a welcome message requires a binary Responder op (Python: plugins/ephemeral.py:_edit_welcome_message via ephemeral.EditMessageRequest)".to_string(),
    ))
}

// ── Receiver resolution ──────────────────────────────────────────────────

/// Resolve a whisper receiver token to a user id.
///
/// Numeric ids and `me`/`self` resolve locally. `@username`s and phone
/// numbers need entity resolution; a bare reply needs the replied
/// message's sender — both are Responder ops.
pub fn resolve_receiver(ctx: &Ctx, raw: &str) -> Result<Option<i64>> {
    let raw = raw.trim();
    if raw.is_empty() {
        if ctx.reply_to_id.is_some() {
            // The binary fills `reply_to_sender_id` from the one reply
            // fetch it already does (Python: `_resolve_receiver` via
            // `event.get_reply_message()`).
            return ctx
                .reply_to_sender_id
                .ok_or_else(|| {
                    Error::Other(
                        "replied message has no sender (deleted or fetch failed)".to_string(),
                    )
                })
                .map(Some);
        }
        return Ok(None);
    }
    let lower = raw.to_lowercase();
    if lower == "me" || lower == "self" {
        return Ok(Some(ctx.sender_id));
    }
    if let Ok(id) = raw.trim_start_matches('@').parse::<i64>() {
        return Ok(Some(id));
    }
    // TODO(responder-op): resolve @username / phone to a user entity
    // (Python: plugins/ephemeral.py:_resolve_receiver via client.get_entity).
    Err(Error::Other(
        "resolving a @username/phone to a user requires a binary Responder op (Python: plugins/ephemeral.py:_resolve_receiver via client.get_entity)".to_string(),
    ))
}

struct WhisperParts {
    receiver: Option<i64>,
    text: String,
}

/// Split `.whisper <@user|id> <text>` args; when the first token is not
/// a receiver, the whole text is the message and the receiver comes from
/// the reply.
fn split_whisper(ctx: &Ctx) -> Result<WhisperParts> {
    let raw = ctx.args.trim();
    let mut parts = raw.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return Ok(WhisperParts {
            receiver: None,
            text: String::new(),
        });
    }
    if looks_like_receiver(first) {
        let receiver = resolve_receiver(ctx, first)?;
        return Ok(WhisperParts {
            receiver,
            text: rest,
        });
    }
    let receiver = resolve_receiver(ctx, "")?;
    Ok(WhisperParts {
        receiver,
        text: raw.to_string(),
    })
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_eph(ctx: &Ctx) -> Result<()> {
    let text = ctx.args.trim();
    if text.is_empty() {
        ctx.reply(&format!(
            "Ephemeral Message\nUsage: {}\nSends: a disappearing message to this chat\n{} sends to one person",
            usage("eph <text>"),
            usage("whisper <user> <text>")
        ))
        .await?;
        return Ok(());
    }
    let rich = RichDoc::markdown(text)?;
    send_ephemeral(
        ctx.chat_id,
        text,
        Some(&rich),
        None,
        ctx.reply_to_id,
        false,
        false,
    )?;
    ctx.reply("Ephemeral Message\nDelivered to this chat\nvisible to everyone — auto-disappears")
        .await?;
    Ok(())
}

async fn cmd_whisper(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        ctx.reply(&format!(
            "Whisper\nUsage: {}\nEphemeral message only that user can see.\nAlias: {}",
            usage("whisper <@user|id> <text>"),
            usage("whspr")
        ))
        .await?;
        return Ok(());
    }
    let parts = split_whisper(ctx)?;
    let Some(receiver) = parts.receiver else {
        ctx.reply(&format!(
            "Whisper\nError: unable to resolve a receiver\nUsage: {} — or reply to someone.",
            usage("whisper <@user|id> <text>")
        ))
        .await?;
        return Ok(());
    };
    if parts.text.is_empty() {
        ctx.reply(&format!(
            "Whisper\nError: no message text provided\nUsage: {}",
            usage("whisper <@user|id> <text>")
        ))
        .await?;
        return Ok(());
    }
    let html = strong_whisper_html(&parts.text);
    let rich = RichDoc::html(html)?;
    send_ephemeral(
        ctx.chat_id,
        &parts.text,
        Some(&rich),
        Some(receiver),
        ctx.reply_to_id,
        true,
        false,
    )?;
    ctx.reply(&format!(
        "Whisper\nReceiver: {}\n🔒 Private — only you and the recipient can see it",
        display_receiver(None, None, receiver)
    ))
    .await?;
    Ok(())
}

async fn cmd_ephdel(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let mid: i64 = match raw.parse() {
        Ok(n) => n,
        Err(_) => {
            ctx.reply(&format!(
                "Ephemeral · Delete\nUsage: {}",
                usage("ephdel <message-id>")
            ))
            .await?;
            return Ok(());
        }
    };
    delete_ephemeral(ctx.chat_id, mid)?;
    ctx.reply(&format!("Ephemeral · Delete\nDeleted id: {mid}"))
        .await?;
    Ok(())
}

async fn cmd_welcome(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let chat_id = ctx.chat_id;
    if raw.is_empty() {
        let current = welcome_text(chat_id);
        let status = if current.is_some() { "ON" } else { "OFF" };
        ctx.reply(&format!(
            "Welcome Status\nStatus: {status}\nText: {}",
            current.as_deref().unwrap_or("Not set")
        ))
        .await?;
        return Ok(());
    }
    let mut parts = raw.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    match cmd.as_str() {
        "on" => {
            if rest.is_empty() {
                ctx.reply("Welcome\nError: provide text after 'on'").await?;
                return Ok(());
            }
            set_welcome_text(chat_id, Some(rest));
            ctx.reply(&format!("Welcome\nStatus: ON\nText: {rest}"))
                .await?;
        }
        "off" => {
            set_welcome_text(chat_id, None);
            ctx.reply("Welcome\nStatus: OFF").await?;
        }
        "text" => {
            if rest.is_empty() {
                ctx.reply("Welcome\nError: provide new text after 'text'")
                    .await?;
                return Ok(());
            }
            set_welcome_text(chat_id, Some(rest));
            ctx.reply(&format!("Welcome\nStatus: text updated\nText: {rest}"))
                .await?;
        }
        "list" => {
            let msgs = get_welcome_messages(chat_id)?;
            match msgs {
                Some(list) if !list.is_empty() => {
                    let mut out = format!("**Welcome messages ({}):**", list.len());
                    for id in list {
                        out.push_str(&format!("\n  • ID {id}"));
                    }
                    ctx.reply(&out).await?;
                }
                _ => {
                    ctx.reply("Welcome\nInfo: No welcome messages set").await?;
                }
            }
        }
        "del" => {
            delete_all_welcome_messages(chat_id)?;
            ctx.reply("Welcome\nStatus: All welcome messages deleted")
                .await?;
        }
        "reset" => {
            set_welcome_text(chat_id, None);
            let _ = delete_all_welcome_messages(chat_id);
            ctx.reply("Welcome\nStatus: Reset to default").await?;
        }
        _ => {
            ctx.reply(&format!(
                "Welcome\nUsage: {} | {} | {} | {} | {} | {}",
                usage("welcome on <text>"),
                usage("welcome off"),
                usage("welcome text <text>"),
                usage("welcome list"),
                usage("welcome del"),
                usage("welcome reset"),
            ))
            .await?;
        }
    }
    Ok(())
}

// ── Join listener (binary-wired) ─────────────────────────────────────────

/// The join listener (`_on_join_update` in Python): when a user joins a
/// chat with welcome text configured, send them the ephemeral welcome.
/// The binary calls this with the joining user's id (detected from the
/// raw `UpdateNewMessage`/`UpdateNewChannelMessage` join action).
pub async fn handle_join_welcome(ctx: &Ctx, join_user_id: i64) -> Result<bool> {
    let Some(text) = welcome_text(ctx.chat_id) else {
        return Ok(false);
    };
    let html = welcome_rich_html("this chat", &text);
    let rich = RichDoc::html(html)?;
    match send_ephemeral(
        ctx.chat_id,
        &text,
        Some(&rich),
        Some(join_user_id),
        None,
        true,
        true,
    ) {
        Ok(_) => Ok(true),
        Err(e) => Err(e),
    }
}

pub fn register(registry: &mut Registry) {
    let help = "User commands (routed on the user account):\n• `.eph <text>` — broadcast ephemeral message to the current chat.\n• `.whisper <@user|id> <text>` (alias `.whspr`) — ephemeral message visible only to that user. Reply to someone and pass only `<text>` to whisper back to the person you replied to.\n• `.ephdel <id>` — delete a broadcast ephemeral message you sent.\n• `.welcome on <text>` — enable auto-welcome for new joiners (ephemeral).\n• `.welcome off` — disable auto-welcome.\n• `.welcome text <text>` — change welcome message text.\n• `.welcome list` — list current welcome messages.\n• `.welcome del` — delete the welcome message.\n• `.welcome reset` — reset welcome to default.\n\nAssistant-bot commands (owner-only, delivered from the user account): `/eph <text>` and `/whisper <@user|id> <text>` — same surface as above.\n\nInline mode: `@<assistant> eph <text>` or `@<assistant> whisper <@user|id> <text>`.";
    reg_add(registry, def("eph", "eph(?: |$)(.*)", help), |ctx| {
        Box::pin(cmd_eph(ctx))
    });
    reg_add(
        registry,
        def("whisper", "(?:whisper|whspr)(?: |$)(.*)", help),
        |ctx| Box::pin(cmd_whisper(ctx)),
    );
    reg_add(registry, def("ephdel", "ephdel(?: |$)(.*)", help), |ctx| {
        Box::pin(cmd_ephdel(ctx))
    });
    reg_add(
        registry,
        def("welcome", "welcome(?: |$)(.*)", help),
        |ctx| Box::pin(cmd_welcome(ctx)),
    );
}
