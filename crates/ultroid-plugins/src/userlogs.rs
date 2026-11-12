//! Tag/mention logging + join/add logs — port of `plugins/_userlogs.py`.
//!
//! This module has **no commands** in the Python source — it is entirely
//! event-driven (`NewMessage` mentions catcher, `MessageEdited` tracker,
//! `ChatAction` join/add logs, `leave_ch_*` / `do_nothing` callbacks).
//! The `Registry` only carries commands, so `register` legitimately
//! registers nothing. Everything that can be expressed without the raw
//! update stream is ported below as real, callable helpers for the binary's
//! update loop:
//!
//! * the `TAG_LOG` gate + tag bookkeeping (`BOTCHAT` / `TAG` map),
//! * the `#ADD_LOG` / `#JOIN_LOG` / `#APPROVAL_LOG` text builders,
//! * the mention/edit decision predicates,
//! * the `leave_ch_<chat>|<bot|user>` callback target parser.
//!
//! Steps that need live Telegram data (fetching a message, its sender and
//! entities, the assistant-bot forward, deleting a dialog) are marked with
//! the exact `TODO(responder-op)` they need.

use serde_json::Value;
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

/// No commands in the source; see the module docs. Kept so the module
/// exposes the same `register` entry point as every other plugin.
pub fn register(_registry: &mut Registry) {}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// The `TAG_LOG` channel id, if tag logging is configured
/// (Python `udB.get_key("TAG_LOG")`).
pub async fn tag_log_channel(ctx: &Ctx) -> Result<Option<i64>> {
    let v: Option<Value> = db_get_json(&*ctx.db, "TAG_LOG").await?;
    Ok(v.and_then(|v| v.as_i64()))
}

/// Should an incoming message be forwarded to the tag log?
/// Python `all_messages_catcher`: only when TAG_LOG is set, the sender is
/// not a bot/verified user, and the message mentions the owner.
pub fn should_log_mention(
    tag_log_set: bool,
    sender_is_bot: bool,
    sender_is_verified: bool,
    mentioned: bool,
) -> bool {
    tag_log_set && !sender_is_bot && !sender_is_verified && mentioned
}

/// Record a forwarded tag-log message (Python `dbs.botchat.tag_add`):
/// `BOTCHAT` -> `{"TAG": {sent_msg_id: [chat_id, orig_msg_id]}}`.
pub async fn tag_add(ctx: &Ctx, sent_id: i64, chat_id: i64, orig_msg_id: i64) -> Result<()> {
    let mut map: HashMap<String, Value> =
        db_get_json(&*ctx.db, "BOTCHAT").await?.unwrap_or_default();
    let mut tag = map
        .get("TAG")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    tag.insert(
        sent_id.to_string(),
        Value::Array(vec![Value::from(chat_id), Value::from(orig_msg_id)]),
    );
    map.insert("TAG".to_string(), Value::Object(tag));
    db_set_json(&*ctx.db, "BOTCHAT", &map).await
}

/// Look a tag-log reply up again (Python `dbs.botchat.who_tag`):
/// returns `(chat_id, orig_msg_id)` for a logged message id.
pub async fn who_tag(ctx: &Ctx, sent_id: i64) -> Result<(Option<i64>, Option<i64>)> {
    let map: HashMap<String, Value> = db_get_json(&*ctx.db, "BOTCHAT").await?.unwrap_or_default();
    let pair = map
        .get("TAG")
        .and_then(|v| v.get(sent_id.to_string()))
        .and_then(|v| v.as_array());
    match pair {
        Some(a) if a.len() == 2 => Ok((a[0].as_i64(), a[1].as_i64())),
        _ => Ok((None, None)),
    }
}

/// Build the `#ADD_LOG` / `#JOIN_LOG` / `#APPROVAL_LOG` text
/// (Python `when_added_or_joined`). `chat_link` is the `https://t.me/…`
/// link to the action message; `scope` is `"bot"` when the assistant bot
/// joined, else `"user"`.
pub fn join_log_text(
    kind: JoinKind,
    added_by_mention: &str,
    user_mention: &str,
    chat_link: &str,
    chat_title: &str,
) -> String {
    let chat = format!("[{chat_title}]({chat_link})");
    match kind {
        JoinKind::Added => {
            format!("#ADD_LOG\n\n{added_by_mention} just added {user_mention} to {chat}.")
        }
        JoinKind::Approval => {
            format!("#APPROVAL_LOG\n\n{user_mention} just got Chat Join Approval to {chat}.")
        }
        JoinKind::Joined => format!("#JOIN_LOG\n\n{user_mention} just joined {chat}."),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Added,
    Approval,
    Joined,
}

/// The inline "leave this chat" button payload for a join log
/// (Python `ikm([(get_string("userlogs_3"), f"leave_ch_{chat_id}|{key}")])`).
pub fn leave_button_payload(chat_id: i64, scope: &str) -> String {
    format!("leave_ch_{chat_id}|{scope}")
}

/// Parse a `leave_ch_<chat_id>|<bot|user>` callback payload
/// (Python `leave_ch_at`).
pub fn parse_leave_payload(payload: &str) -> Option<(i64, String)> {
    let rest = payload.strip_prefix("leave_ch_")?;
    let (chat, scope) = rest.split_once('|')?;
    Some((chat.parse::<i64>().ok()?, scope.to_string()))
}

/// Handle the `leave_ch_*` tap: resolve the payload, then leave/delete the
/// dialog. The dialog lookup + delete need client ops.
pub async fn leave_chat_via_payload(_ctx: &Ctx, payload: &str) -> Result<()> {
    let Some((chat_id, scope)) = parse_leave_payload(payload) else {
        return Ok(());
    };
    if scope != "bot" && scope != "user" {
        return Ok(());
    }
    let _ = chat_id;
    // TODO(responder-op): get chat title + delete_dialog(chat_id).
    // Python: _userlogs.py:leave_ch_at.
    Err(todo_op(
        "get chat entity + delete_dialog",
        "plugins/_userlogs.py:leave_ch_at",
    ))
}

/// Format one edited-message entry for the tag log (Python `upd_edits`).
/// `count` is how many edits of this message have been logged so far
/// (capped at 10 by the caller).
pub fn edit_log_line(edit_time: &str, new_text: &str, count: usize) -> String {
    let mut line = if new_text.contains('\n') {
        format!("\n• `{edit_time}` :\n-> {new_text}")
    } else {
        format!("\n• `{edit_time}` : {new_text}")
    };
    if count == 10 {
        line.push_str("\n\n• __Only the first 10 Edits are shown.__");
    }
    line
}

/// The one-shot header appended to a logged message on its first edit
/// (Python `upd_edits`).
pub const EDIT_HEADER: &str = "\n\n🖋 **Later Edited to !**";

/// Decision for the `MessageEdited` handler (Python `upd_edits`):
/// an edit is only logged when the original message was already forwarded
/// to the tag log and its text actually changed.
pub fn should_log_edit(
    was_logged: bool,
    old_text: &str,
    new_text: &str,
    edit_count: usize,
) -> bool {
    was_logged && old_text != new_text && edit_count <= 10
}
