//! Per-group AFK status.
//!
//! Port of `assistant/manager/afk.py`.
//!
//! The `afk` command is ported in full: per-chat, per-user in-memory state
//! (`AFK` dict in Python), text reasons from the command args or from the
//! replied message's text, and the rich status cards ("`{name} went AFK`"
//! / "`{name} is no longer AFK`" with "Reason: …" / "Away for …"
//! details, `fns::time_formatter` durations).
//!
//! What is NOT portable through the current core surface:
//!
//! * The `make_change` watcher (`@asst.on(NewMessage(...))`): there is no
//!   NewMessage watcher registry — `Registry` dispatches commands only —
//!   so nothing in this crate sees non-command messages. The state
//!   machine it implements is exposed as plain functions
//!   ([`mark_returned`], [`afk_entry`], [`is_afk_command`],
//!   [`afk_status_doc`]) for the binary's update loop to call per message.
//! * Mention notification needs message entities
//!   (`MessageEntityMention` / `MessageEntityMentionName`) and a
//!   `client.get_entity` lookup; [`Ctx`] carries neither entities nor a
//!   client handle.
//! * Media reasons: Python forwards the replied media message into the
//!   chat (`client.forward_messages`); there is no forward surface on
//!   [`ultroid_core::Responder`], so a media reply sets AFK with no reason
//!   text and no forward.
//! * Sender checks: Python ignores non-`User` senders and bots, and uses
//!   `get_display_name(sender)` for the status titles. [`Ctx`] carries no
//!   sender kind and no display name, so titles use `User {sender_id}`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use ultroid_core::Ctx;
use ultroid_core::error::Result;
use ultroid_core::fns::time_formatter;
use ultroid_core::rich::RichDoc;
use ultroid_core::router::{CommandDef, Registry};

use crate::misc_assistant::fn_handler;

/// One user's AFK entry (Python: `{"reason": …, "time": dt.now()}`).
#[derive(Debug, Clone)]
pub struct AfkEntry {
    /// Text reason, or `None` when the reason was a media message (which
    /// cannot be forwarded through the core surface — see module docs).
    pub reason: Option<String>,
    pub started: SystemTime,
}

/// The process-wide AFK store: chat id -> sender id -> entry.
/// Python's module-level `AFK = {}`.
pub static AFK: OnceLock<Mutex<HashMap<i64, HashMap<i64, AfkEntry>>>> = OnceLock::new();

fn afk_store() -> &'static Mutex<HashMap<i64, HashMap<i64, AfkEntry>>> {
    AFK.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a user as AFK in a chat (Python: `AFK.setdefault(chat,
/// {})[sender] = {"reason": reason, "time": dt.now()}`).
pub fn set_afk(chat_id: i64, sender_id: i64, reason: Option<String>) {
    afk_store()
        .lock()
        .unwrap()
        .entry(chat_id)
        .or_default()
        .insert(
            sender_id,
            AfkEntry {
                reason,
                started: SystemTime::now(),
            },
        );
}

/// Look up one user's AFK entry without removing it.
pub fn afk_entry(chat_id: i64, sender_id: i64) -> Option<AfkEntry> {
    afk_store()
        .lock()
        .unwrap()
        .get(&chat_id)
        .and_then(|chat| chat.get(&sender_id))
        .cloned()
}

/// Clear a returning user's AFK entry and return how long they were away
/// (Python `make_change`: pop the sender's entry, and drop the chat when
/// it becomes empty). Returns `None` when the user was not AFK.
pub fn mark_returned(chat_id: i64, sender_id: i64) -> Option<Duration> {
    let mut store = afk_store().lock().unwrap();
    let entry = store.get_mut(&chat_id)?.remove(&sender_id)?;
    if store.get(&chat_id).is_some_and(|chat| chat.is_empty()) {
        store.remove(&chat_id);
    }
    entry.started.elapsed().ok()
}

/// Format an AFK duration (Python `_afk_duration`: milliseconds through
/// `fns.time_formatter`).
pub fn afk_duration(started: SystemTime) -> String {
    let elapsed_ms = started.elapsed().map(|d| d.as_millis() as u64).unwrap_or(0);
    time_formatter(elapsed_ms)
}

/// Whether an incoming message text is itself an `/afk` (or `{HNDLR}afk`)
/// command, so the watcher must not treat it as a return from AFK
/// (Python `_is_afk_command`).
pub fn is_afk_command(raw_text: &str, hndlr: &str) -> bool {
    let first = raw_text.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        return false;
    }
    for prefix in ["/", hndlr] {
        if !prefix.is_empty() && first.starts_with(prefix) {
            let name = first[prefix.len()..].split('@').next().unwrap_or("");
            if name.eq_ignore_ascii_case("afk") {
                return true;
            }
        }
    }
    false
}

/// A rich status card (Python `_send_status`: level-3 header + one
/// paragraph per non-empty detail).
pub fn afk_status_doc(title: &str, details: &[&str]) -> Result<RichDoc> {
    let mut text = format!("### {title}");
    for detail in details {
        if !detail.is_empty() {
            text.push_str(&format!("\n\n{detail}"));
        }
    }
    RichDoc::markdown(text)
}

/// Display name for a status title — Python uses
/// `get_display_name(sender)`; no display name exists on [`Ctx`], so the
/// sender id is used (see module docs).
pub fn display_name(sender_id: i64) -> String {
    format!("User {sender_id}")
}

/// The `afk` command (Python `go_afk`, `@asst_cmd(pattern="afk",
/// func=lambda event: not event.is_private)` — groups only).
///
/// Reason: the text after the command, else the replied message's text
/// ([`Ctx::reply_to_text`]). A reply with no text (media) sets AFK with
/// no reason — the media forward is not portable, see module docs.
pub async fn go_afk(ctx: &Ctx) -> Result<()> {
    let reason: Option<String> = if !ctx.args.trim().is_empty() {
        Some(ctx.args.trim().to_string())
    } else {
        ctx.reply_to_text.clone()
    };
    set_afk(ctx.chat_id, ctx.sender_id, reason.clone());
    let title = format!("{} went AFK", display_name(ctx.sender_id));
    let doc = match reason.as_deref().filter(|r| !r.is_empty()) {
        Some(reason) => afk_status_doc(&title, &[&format!("Reason: {reason}")])?,
        None => afk_status_doc(&title, &[])?,
    };
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

/// Register the `afk` command. The `make_change` watcher has no
/// NewMessage registry to attach to — see the module docs.
pub fn register(registry: &mut Registry) {
    registry.add(
        CommandDef {
            name: "afk",
            pattern: "afk",
            help: "Set or clear your away status.",
            groups_only: true,
            ..Default::default()
        },
        fn_handler(|ctx| Box::pin(go_afk(ctx))),
    );
}
