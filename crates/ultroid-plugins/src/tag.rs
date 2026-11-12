//! Mass member tagging — port of `plugins/tag.py`.
//!
//! `.tagall` / `.tagcancel` / `.tagowner` — flag parsing, filter
//! validation, session bookkeeping and cancel logic are fully ported.
//! Fetching participants and sending the tag messages need raw client
//! calls the `Ctx` surface does not expose, so the session runner
//! returns the honest `TODO(responder-op)` error at the fetch/send
//! points (see the report).
//!
//! Sessions live in process memory (Python `_SESSIONS`), keyed
//! `tag_<unix_seconds>` (Python `_generate_session_id`).

use crate::util::{def, reg_add};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available\n\n• `{i}tagall [-e] [-a] [-h] [-c <msg>] <query>` — Tag members with robust modes.\n    `-e` — Ephemeral: only tagged users can see their mention.\n    `-a` — Tag only admins.\n    `-h` — Hidden tag using invisible Unicode characters.\n    `-c <msg>` — Custom message for each tagged user.\n    `-c <session_id>` — Cancel an active tagging session.\n    `<query>` — filter: on, off, all, bots, rec, admins, owner.\n\n• `{i}tagcancel` — Cancel the current tagging session.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "tagall",
            pattern: r"tagall(?:\s+(.*))?(?:\s+(.*))?$",
            help: HELP,
            groups_only: true,
            ..def("tagall", r"tagall(?:\s+(.*))?(?:\s+(.*))?$", HELP)
        },
        |ctx| Box::pin(tagall(ctx)),
    );
    reg_add(
        registry,
        def("tagcancel", r"tagcancel(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(tagcancel(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "tagowner",
            pattern: "tagowner$",
            help: HELP,
            groups_only: true,
            ..def("tagowner", "tagowner$", HELP)
        },
        |ctx| Box::pin(tag_owner(ctx)),
    );
}

/// One active tagging session (Python `_SESSIONS` entry).
#[derive(Debug, Clone)]
pub struct TagSession {
    pub filter: String,
    pub mode: String,
    pub admins_only: bool,
    pub hidden: bool,
    pub custom_msg: Option<String>,
    pub total: usize,
    pub cancelled: bool,
    pub started: f64,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, TagSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn generate_session_id() -> String {
    format!("tag_{}", now_secs() as i64)
}

pub fn get_session(session_id: &str) -> Option<TagSession> {
    SESSIONS.lock().ok()?.get(session_id).cloned()
}

pub fn set_session(session_id: String, data: TagSession) {
    if let Ok(mut m) = SESSIONS.lock() {
        m.insert(session_id, data);
    }
}

pub fn del_session(session_id: &str) {
    if let Ok(mut m) = SESSIONS.lock() {
        m.remove(session_id);
    }
}

pub fn active_sessions() -> Vec<String> {
    match SESSIONS.lock() {
        Ok(m) => m
            .iter()
            .filter(|(_, s)| !s.cancelled)
            .map(|(k, _)| k.clone())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Does this text use invisible Unicode characters for hidden tagging?
/// (Python `_is_hidden_tag`: any char of Unicode category Cf.)
pub fn is_hidden_tag(text: &str) -> bool {
    text.chars().any(|c| {
        // Cf = format characters: zero-width space/joiner/non-joiner, BOM, etc.
        matches!(
            c,
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}' | '\u{00AD}'
        )
    })
}

/// Wrap text in invisible characters for hidden tagging
/// (Python `_build_hidden_text`: zero-width space after every char, and
/// around spaces).
pub fn build_hidden_text(text: &str) -> String {
    let invis = '\u{200B}';
    let mut result = String::new();
    for ch in text.chars() {
        if ch == ' ' {
            result.push(invis);
            result.push(' ');
            result.push(invis);
        } else {
            result.push(ch);
            result.push(invis);
        }
    }
    result
}

/// How a tag mention is rendered (Python `_normalize_mention_text`).
pub fn normalize_mention_text(text: &str, hidden: bool) -> String {
    if hidden {
        build_hidden_text(text)
    } else {
        text.to_string()
    }
}

/// Parsed `.tagall` flags (Python `tagall` flag loop).
#[derive(Debug, Clone, Default)]
pub struct TagFlags {
    pub mode: String, // "regular" | "ephemeral"
    pub filter: String,
    pub admins_only: bool,
    pub hidden: bool,
    pub custom_msg: Option<String>,
}

pub const VALID_FILTERS: [&str; 7] = ["on", "off", "all", "bots", "rec", "admins", "owner"];

/// Parse the `.tagall` argument tokens. Returns `Err(message)` for the
/// usage replies (Python's two `fmtmsg("Tag", {"Usage": ...})` branches).
pub fn parse_tag_args(args: &[&str]) -> std::result::Result<TagFlags, String> {
    let mut flags = TagFlags {
        mode: "regular".to_string(),
        filter: "all".to_string(),
        ..Default::default()
    };
    let mut filter_candidates: Vec<String> = Vec::new();
    if args.is_empty() {
        return Err(format!(
            "Usage: .tagall [-e] [-a] [-h] [-c <msg>] <{}>",
            VALID_FILTERS.join("/")
        ));
    }
    let mut i = 0;
    while i < args.len() {
        let p = args[i];
        match p {
            "-e" => flags.mode = "ephemeral".to_string(),
            "-a" => flags.admins_only = true,
            "-h" => flags.hidden = true,
            "-c" if i + 1 < args.len() => {
                flags.custom_msg = Some(args[i + 1].to_string());
                i += 1;
            }
            _ if p.starts_with("-c") => {
                flags.custom_msg = Some(p[2..].trim().to_string());
            }
            _ if p.starts_with('-') => {}
            _ if VALID_FILTERS.contains(&p) => flags.filter = p.to_lowercase(),
            _ => filter_candidates.push(p.to_string()),
        }
        i += 1;
    }
    if !filter_candidates.is_empty() && flags.filter.is_empty() {
        flags.filter = filter_candidates[0].to_lowercase();
    }
    if !VALID_FILTERS.contains(&flags.filter.as_str()) {
        return Err(format!(
            "Usage: .tagall {} [-e] [-a] [-h] [-c <msg>]",
            VALID_FILTERS.join(" ")
        ));
    }
    Ok(flags)
}

/// Build one tag line for a user (Python `_run_tag_session`).
pub fn tag_text_for(user_mention: &str, username_or_id: &str, flags: &TagFlags) -> String {
    if let Some(custom) = &flags.custom_msg {
        return custom
            .replace("{name}", user_mention)
            .replace("{user}", username_or_id);
    }
    if flags.hidden {
        return format!("🗿 {}", normalize_mention_text(user_mention, true));
    }
    format!("🗿 {user_mention}")
}

/// The session progress card (Python `fmtmsg("Tag Session", {...})`).
pub fn session_card(status: &str, tagged: usize, total: usize, failed: usize) -> String {
    format!(
        "**Tag Session**\n\n**Status:** {status}\n**Tagged:** {tagged}/{total}\n**Failed:** {failed}"
    )
}

async fn tagall(ctx: &Ctx) -> Result<()> {
    let args: Vec<&str> = ctx.args.split_whitespace().collect();
    let _flags = match parse_tag_args(&args) {
        Ok(f) => f,
        Err(usage) => {
            ctx.reply(&format!("**Tag**\n\n**Usage:** {usage}")).await?;
            return Ok(());
        }
    };
    let active = active_sessions();
    if !active.is_empty() {
        ctx.reply(&format!(
            "**Tag**\n\n**Warning:** Active session(s): {}\n**Use:** .tagcancel <id> to cancel",
            active
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await?;
        return Ok(());
    }
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    // Ephemeral tags need `SendMessageRequest(receiver_id=...)`, which
    // the trait does not expose; online/offline/recent filters need
    // `UserStatus`, which `ParticipantInfo` does not carry.
    if _flags.mode == "ephemeral" {
        // TODO(responder-op): ephemeral tag send (SendMessageRequest
        // with receiver_id). Python: plugins/tag.py:_send_ephemeral_tag.
        return Err(todo_op(
            "send ephemeral tag messages",
            "plugins/tag.py:_send_ephemeral_tag",
        ));
    }
    if matches!(_flags.filter.as_str(), "on" | "off" | "rec") {
        // TODO(responder-op): filter participants by UserStatus.
        // Python: plugins/tag.py:_get_filtered_participants.
        return Err(todo_op(
            "fetch participants filtered by online status",
            "plugins/tag.py:_get_filtered_participants",
        ));
    }
    // Python fetches at most 200 participants.
    let (page, _total) = ctx.responder.participants(ctx.chat_id, 200, 0).await?;
    let users: Vec<&ultroid_core::ParticipantInfo> = page
        .iter()
        .filter(|p| {
            if _flags.admins_only && !p.admin {
                return false;
            }
            match _flags.filter.as_str() {
                "bots" => p.user.bot,
                "owner" => p.owner,
                // "admins" matches no filter branch in Python's
                // `_get_filtered_participants` (it checks "admin",
                // which is not a valid tagall filter), so it tags
                // everyone; `-a` is the admin filter.
                _ => true,
            }
        })
        .collect();
    if users.is_empty() {
        ctx.reply("**Tag**\n\n**Error:** No participants found")
            .await?;
        return Ok(());
    }
    let total = users.len();
    let session_id = generate_session_id();
    set_session(
        session_id.clone(),
        TagSession {
            filter: _flags.filter.clone(),
            mode: _flags.mode.clone(),
            admins_only: _flags.admins_only,
            hidden: _flags.hidden,
            custom_msg: _flags.custom_msg.clone(),
            total,
            cancelled: false,
            started: now_secs(),
        },
    );
    let mut sent = 0usize;
    let mut failed = 0usize;
    for p in &users {
        if get_session(&session_id)
            .map(|s| s.cancelled)
            .unwrap_or(false)
        {
            break;
        }
        let mention = format!("[{}](tg://user?id={})", p.user.first_name, p.user.id);
        let username_or_id = p
            .user
            .username
            .clone()
            .unwrap_or_else(|| p.user.id.to_string());
        let text = tag_text_for(&mention, &username_or_id, &_flags);
        match ctx.responder.send_to(ctx.chat_id, &text).await {
            Ok(_) => sent += 1,
            Err(_) => failed += 1,
        }
    }
    del_session(&session_id);
    ctx.reply(&session_card("Completed", sent, total, failed))
        .await?;
    Ok(())
}

async fn tagcancel(ctx: &Ctx) -> Result<()> {
    let session_id = ctx.args.trim().to_string();
    if !session_id.is_empty() {
        match get_session(&session_id) {
            Some(mut s) => {
                s.cancelled = true;
                set_session(session_id.clone(), s);
                ctx.reply(&format!(
                    "**Tag**\n\n**Status:** 🔴 Cancelled\n**ID:** {session_id}"
                ))
                .await?;
            }
            None => {
                ctx.reply("**Tag**\n\n**Error:** Session not found").await?;
            }
        }
        return Ok(());
    }
    let mut cancelled = 0;
    if let Ok(mut m) = SESSIONS.lock() {
        for s in m.values_mut() {
            if !s.cancelled {
                s.cancelled = true;
                cancelled += 1;
            }
        }
    }
    if cancelled > 0 {
        ctx.reply(&format!(
            "**Tag**\n\n**Status:** 🔴 Cancelled {cancelled} session(s)"
        ))
        .await?;
    } else {
        ctx.reply("**Tag**\n\n**Info:** No active sessions").await?;
    }
    Ok(())
}

async fn tag_owner(ctx: &Ctx) -> Result<()> {
    // Python fetches at most 200 participants and tags the creator.
    let (page, _total) = ctx.responder.participants(ctx.chat_id, 200, 0).await?;
    for p in &page {
        if p.owner {
            let mention = format!("[{}](tg://user?id={})", p.user.first_name, p.user.id);
            ctx.responder
                .send_to(ctx.chat_id, &format!("👑 {mention}"))
                .await?;
            ctx.reply(&format!(
                "**Tag**\n\n**Status:** Tagged owner\n**Owner:** {mention}"
            ))
            .await?;
            return Ok(());
        }
    }
    ctx.reply("**Tag**\n\n**Error:** No owner found").await?;
    Ok(())
}
