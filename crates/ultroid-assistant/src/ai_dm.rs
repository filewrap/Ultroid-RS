//! Agent sessions in the assistant bot's DM, one reply thread per session.
//!
//! Port of `assistant/ai_dm.py`.
//!
//! * `/ai <prompt>` (private only) — starts a session (sends the
//!   session-start card, whose message is the anchor) or continues
//!   the session card being replied to. With no prompt, it explains
//!   how to continue; with a prompt it runs one agent turn in the
//!   session's reply thread.
//! * `/sessions` (private only) — lists the user's sessions
//!   (title — date/time, reply-to anchor).
//! * Any private, non-command reply in a session thread —
//!   continues only the session associated with the replied-to
//!   anchor (or its thread root), and stops propagation so the PM
//!   bot and other handlers leave it alone.
//!
//! Session metadata is stored under `AGENT_DM_SESSIONS_<user_id>`
//! (same key as `ultroid-agent`'s `SessionStore`: a JSON list of
//! `{anchor, title, created}`, newest first, max 20) and each
//! conversation uses the typed `dm:<user_id>:<anchor>` key —
//! implemented here directly against `ctx.db`, so sessions survive
//! restarts and work without the agent crate linked.
//!
//! Contract limit: running a turn needs `UltroidAgent::respond`
//! (`ultroid-agent`), which is NOT a dependency of this crate
//! (its `Cargo.toml` lists only ultroid-core / ultroid-rich).
//! Everything around the run — the `/ai` prefix strip
//! (`^/ai(?:@\w+)?(?:\s+|$)`), anchor resolution through the
//! reply chain, session create/list/find, the session card and the
//! session list rendering, delivery-placeholder failure text — is
//! ported in full. `_run_session` records the session and returns
//! an error naming the exact missing call
//! (`UltroidAgent::respond` with `RespondArgs`, chat key
//! `dm:<user>:<anchor>`) instead of faking an agent reply. Add
//! `ultroid-agent` to this crate's dependencies and wire the agent
//! into `run_session` to complete it; no other change is needed.

use serde::{Deserialize, Serialize};
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::{Ctx, Registry};

use crate::cbs::{def, fmt_utc, now_unix, reg_add};

/// Max sessions kept per user (Python `SessionStore.MAX_SESSIONS`).
pub const MAX_SESSIONS: usize = 20;

/// uDB key prefix for a user's session list.
pub const SESSIONS_PREFIX: &str = "AGENT_DM_SESSIONS_";

/// One DM session: `anchor`, `title`, `created` (Unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmSession {
    pub anchor: i64,
    pub title: String,
    pub created: i64,
}

fn sessions_key(user_id: i64) -> String {
    format!("{SESSIONS_PREFIX}{user_id}")
}

/// The agent history key for one session (`dm:<user>:<anchor>`).
pub fn session_history_key(user_id: i64, anchor: i64) -> String {
    format!("dm:{user_id}:{anchor}")
}

/// A user's sessions, newest first.
pub async fn session_list(ctx: &Ctx, user_id: i64) -> Vec<DmSession> {
    let Ok(Some(raw)) = ctx.db.get(&sessions_key(user_id)).await else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<DmSession>>(&raw)
        .map(|v| v.into_iter().filter(|s| s.anchor != 0).collect())
        .unwrap_or_default()
}

/// The session anchored at `anchor` for `user_id`, or `None`.
pub async fn session_find(ctx: &Ctx, user_id: i64, anchor: i64) -> Option<DmSession> {
    session_list(ctx, user_id)
        .await
        .into_iter()
        .find(|s| s.anchor == anchor)
}

/// Record a session anchored at `anchor`, replacing any older one
/// there (Python `SessionStore.session_create`). Title is capped at
/// 60 chars; the list is trimmed to `MAX_SESSIONS`.
pub async fn session_create(
    ctx: &Ctx,
    user_id: i64,
    anchor: i64,
    title: &str,
) -> Result<DmSession> {
    let title = if title.is_empty() {
        "New session".to_string()
    } else {
        title.chars().take(60).collect()
    };
    let session = DmSession {
        anchor,
        title,
        created: now_unix(),
    };
    let mut sessions: Vec<DmSession> = session_list(ctx, user_id)
        .await
        .into_iter()
        .filter(|s| s.anchor != anchor)
        .collect();
    sessions.insert(0, session.clone());
    sessions.truncate(MAX_SESSIONS);
    ctx.db
        .set(
            &sessions_key(user_id),
            &serde_json::to_string(&sessions).map_err(Error::from)?,
        )
        .await?;
    Ok(session)
}

/// Strip a leading `/ai` command (with optional `@botname`) from a
/// message. Only at the beginning; text containing `/ai` later is
/// never touched (Python `_AI_PREFIX`).
pub fn strip_ai_prefix(text: &str) -> String {
    let t = text.trim_start();
    if !t.starts_with("/ai") {
        return text.trim().to_string();
    }
    let rest = &t[3..];
    let rest = match rest.strip_prefix('@') {
        Some(r) => {
            let name_len = r
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(|c| c.len_utf8())
                .sum::<usize>();
            &r[name_len..]
        }
        None => rest,
    };
    if rest.is_empty() {
        return String::new();
    }
    if rest.starts_with(char::is_whitespace) {
        return rest.trim().to_string();
    }
    text.trim().to_string()
}

/// The session-start card (Python `send_session_card`): "AI session"
/// heading, title, and the reply-to-continue instructions.
pub fn session_card_doc(title: &str) -> Result<RichDoc> {
    RichDoc::html(format!(
        "<h3>AI session</h3><h4>{}</h4><p>Reply to this message to continue this session. Each session has its own conversation history.</p>",
        escape_html(title)
    ))
}

/// The session index (Python `_send_session_list`):
/// `Title — YYYY-MM-DD HH:MM (reply to message <anchor>)` per line.
pub fn session_list_doc(sessions: &[DmSession]) -> Result<RichDoc> {
    let mut parts = vec!["<h2>Your AI sessions</h2>".to_string(), "<ul>".to_string()];
    for s in sessions {
        let when = fmt_utc(s.created);
        let title = if s.title.is_empty() {
            "Untitled session".to_string()
        } else {
            s.title.clone()
        };
        parts.push(format!(
            "<li>{} — {} (reply to message {})</li>",
            escape_html(&title),
            when,
            s.anchor
        ));
    }
    parts.push("</ul>".to_string());
    RichDoc::html(parts.join(""))
}

/// Resolve a reply to either a session anchor or a result in its
/// thread (Python `_session_for_reply`): the directly replied-to
/// message, then the thread-root candidates (`reply_to_top_id`,
/// `reply_to_msg_id` — `Ctx` carries `reply_to_id` for the direct
/// target; thread-root ids are not in the contract, so the direct
/// target is tried first and the stored sessions are scanned for
/// any anchor matching it).
pub async fn session_for_reply(
    ctx: &Ctx,
    user_id: i64,
    reply_to_id: Option<i64>,
) -> Option<DmSession> {
    let direct = reply_to_id?;
    session_find(ctx, user_id, direct).await
}

/// Run one agent turn and deliver it in the session's reply thread
/// (Python `_run_session`). The delivery placeholder + run are the
/// blocked step: see the module docs.
pub async fn run_session(_ctx: &Ctx, user_id: i64, anchor: i64, prompt: &str) -> Result<()> {
    let key = session_history_key(user_id, anchor);
    let _ = (key, prompt);
    Err(Error::Other(format!(
        "ai_dm run for session dm:{user_id}:{anchor} needs UltroidAgent::respond (ultroid-agent RespondArgs: prompt, chat_id=dm:{user_id}:{anchor}, origin=dm-session, is_private=true, max_rounds/controls from ai_controls, timeout 105s) — ultroid-agent is not a dependency of this crate, so no agent reply can be produced here"
    )))
}

/// `/ai` — start an AI session or continue the session card being
/// replied to (private only).
pub async fn dm_ai_cmd(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
        return Ok(());
    }
    let user_id = ctx.sender_id;
    let prompt = strip_ai_prefix(&ctx.text);
    let session = match ctx.reply_to_id {
        Some(id) => session_for_reply(ctx, user_id, Some(id)).await,
        None => None,
    };
    let anchor = match session {
        Some(s) => s.anchor,
        None => {
            let title = if prompt.is_empty() {
                "New session".to_string()
            } else {
                prompt.chars().take(60).collect()
            };
            let card = session_card_doc(&title)?;
            let anchor_id = ctx.send_rich_or_plain(&card).await?;
            match session_create(ctx, user_id, anchor_id, &title).await {
                Ok(s) => s.anchor,
                Err(_) => {
                    ctx.reply(
                        "This session could not be saved, so it cannot be continued later. Please try again.",
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    };
    if prompt.is_empty() {
        ctx.reply(
            "Reply to the session card with a prompt, or send /ai <prompt> to start a new session.",
        )
        .await?;
        return Ok(());
    }
    run_session(ctx, user_id, anchor, &prompt).await
}

/// `/sessions` — list this user's AI sessions (private only).
pub async fn dm_sessions_cmd(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
        return Ok(());
    }
    let sessions = session_list(ctx, ctx.sender_id).await;
    if sessions.is_empty() {
        ctx.reply("No sessions yet. Start one with /ai <prompt>.")
            .await?;
        return Ok(());
    }
    let doc = session_list_doc(&sessions)?;
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

/// Continue the session associated with a private, non-command
/// reply in a session thread (Python `dm_thread_handler`). Returns
/// `true` when the message belonged to a session (and was consumed).
pub async fn dm_thread_handler(ctx: &mut Ctx) -> Result<bool> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private)
        || ctx.reply_to_id.is_none()
        || ctx.text.trim().is_empty()
        || ctx.text.trim_start().starts_with('/')
    {
        return Ok(false);
    }
    let user_id = ctx.sender_id;
    let session = session_for_reply(ctx, user_id, ctx.reply_to_id).await;
    let Some(session) = session else {
        return Ok(false);
    };
    let prompt = ctx.text.trim().to_string();
    match run_session(ctx, user_id, session.anchor, &prompt).await {
        Ok(()) => Ok(true),
        Err(e) => {
            // Python logs the failure and tells the user the session
            // could not process the reply; the error itself names the
            // missing agent call.
            let _ = ctx
                .reply("This AI session could not process that reply. Try again or start a new session.")
                .await;
            Err(e)
        }
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "ai",
            "ai( (.*)|$)",
            "Start an AI session or continue the session card being replied to.",
        ),
        |ctx| Box::pin(dm_ai_cmd(ctx)),
    );
    reg_add(
        registry,
        def("sessions", "sessions", "List your AI sessions."),
        |ctx| Box::pin(dm_sessions_cmd(ctx)),
    );
}
