//! Echo — port of `plugins/echo.py`
//! (`.addecho` / `.remecho` / `.listecho`).
//!
//! Storage is the `ECHO` udB key holding JSON `{chat_id: [user_id, …]}`
//! (Python `EchoDB`, `pyUltroid/dB/echo_db.py`), so the key stays
//! compatible with the Python database.
//!
//! What cannot be expressed through `Ctx` in this wave:
//! * Reply-based user resolution needs the *sender* of the replied
//!   message; `Ctx` carries only the replied message id
//!   (`reply_to_id`), not its sender — that branch is a
//!   `TODO(responder-op)` error.
//! * `@username` resolution needs `get_entity` — same TODO.
//! * `.listecho` display names need `get_entity` per id; ids are listed
//!   with `tg://user?id=` links instead, which still mention the user.
//! * The actual echoing (auto-replying to an echo user's messages) is a
//!   `NewMessage` event handler, not a command; there is no event-handler
//!   registration surface in the plugin crate. The `ECHO_GLOBAL_DISABLED`
//!   manifest toggle is likewise a callback-framework piece, not a
//!   command — its state is the `ECHO_GLOBAL_DISABLED` key, read by the
//!   dispatcher-side echo hook when it exists.

use crate::util::{def, reg_add};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

/// The stored echo map: `{chat_id: [user_id, …]}` (Python `EchoDB`).
pub type EchoMap = HashMap<String, Vec<i64>>;

async fn echo_map(ctx: &Ctx) -> Result<EchoMap> {
    Ok(db_get_json(&*ctx.db, "ECHO").await?.unwrap_or_default())
}

async fn save_echo_map(ctx: &Ctx, map: &EchoMap) -> Result<()> {
    db_set_json(&*ctx.db, "ECHO", map).await
}

fn chat_key(chat_id: i64) -> String {
    chat_id.to_string()
}

/// Resolve the target user id. `Ok(None)` = no usable target (print the
/// usage hint, as Python does on a bad argument). Reply-based and
/// `@username` forms need Responder ops and surface the honest error.
async fn resolve_user(ctx: &Ctx) -> Result<Option<i64>> {
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): fetch replied message -> sender id
        // (Python: echo.py:echo/rm -> r.sender_id).
        return Err(todo_op("get replied message sender id", "echo.py:echo"));
    }
    let arg = ctx.text.split_whitespace().nth(1).unwrap_or("");
    if arg.starts_with('@') {
        // TODO(responder-op): get_entity(username) -> id
        // (Python: echo.py:echo/rm -> e.client.get_entity(user)).
        return Err(todo_op("get_entity(@username)", "echo.py:echo"));
    }
    Ok(arg.parse::<i64>().ok())
}

fn mention(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
}

/// `.addecho [reply|@user|id]` — port of `echo.py::echo`.
async fn addecho(ctx: &Ctx) -> Result<()> {
    let Some(user) = resolve_user(ctx).await? else {
        return ctx.reply("Reply To A user.").await.map(|_| ());
    };
    let mut map = echo_map(ctx).await?;
    let key = chat_key(ctx.chat_id);
    let entry = map.entry(key).or_default();
    if entry.contains(&user) {
        return ctx
            .reply("Echo already activated for this user.")
            .await
            .map(|_| ());
    }
    entry.push(user);
    save_echo_map(ctx, &map).await?;
    ctx.reply(&format!("Activated Echo For {}.", mention(user)))
        .await
        .map(|_| ())
}

/// `.remecho [reply|@user|id]` — port of `echo.py::rm`.
async fn remecho(ctx: &Ctx) -> Result<()> {
    let Some(user) = resolve_user(ctx).await? else {
        return ctx.reply("Reply To A User.").await.map(|_| ());
    };
    let mut map = echo_map(ctx).await?;
    let key = chat_key(ctx.chat_id);
    let in_list = map.get(&key).map(|v| v.contains(&user)).unwrap_or(false);
    if in_list {
        if let Some(v) = map.get_mut(&key) {
            v.retain(|u| *u != user);
        }
        save_echo_map(ctx, &map).await?;
        return ctx
            .reply(&format!("Deactivated Echo For {}.", mention(user)))
            .await
            .map(|_| ());
    }
    ctx.reply("Echo not activated for this user")
        .await
        .map(|_| ())
}

/// `.listecho` — port of `echo.py::lstecho`.
async fn listecho(ctx: &Ctx) -> Result<()> {
    let map = echo_map(ctx).await?;
    let list = map.get(&chat_key(ctx.chat_id)).cloned().unwrap_or_default();
    if list.is_empty() {
        return ctx.reply("`List is Empty, For echo`").await.map(|_| ());
    }
    let mut text = String::from("**Activated Echo For Users:**\n\n");
    for user in list {
        text.push_str(&format!("•{}\n", mention(user)));
    }
    ctx.reply(&text).await.map(|_| ())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "addecho",
            "addecho( (.*)|$)",
            "Start auto-echo of a user's messages in this chat.",
        ),
        |ctx| Box::pin(addecho(ctx)),
    );
    reg_add(
        registry,
        def(
            "remecho",
            "remecho( (.*)|$)",
            "Stop auto-echo of a user's messages in this chat.",
        ),
        |ctx| Box::pin(remecho(ctx)),
    );
    reg_add(
        registry,
        def(
            "listecho",
            "listecho$",
            "List users with echo enabled in this chat.",
        ),
        |ctx| Box::pin(listecho(ctx)),
    );
}
