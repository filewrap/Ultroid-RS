//! Per-chat user mutes — port of `plugins/mute.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/mute_db.py`):
//! `MUTE` maps chat id -> list of muted user ids. That is the
//! *delete-messages* mute (`.dmute` / `.undmute`), fully ported including
//! the live watcher predicate (`is_muted` check + delete, where the delete
//! itself is a `TODO(responder-op)` site).
//!
//! `.tmute` / `.mute` / `.unmute` use Telegram's `edit_permissions`
//! rights-mute instead; their parsing is ported and the RPCs are
//! `TODO(responder-op)` sites. Patterns are kept exactly as written in
//! Python, including the literal `dbs.mute.<cmd>` prefixes on `mute` /
//! `unmute`.

use crate::util::{def, reg_add};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}mute <reply to msg/ user id>`\n    Mute user in current chat.\n\n• `{i}unmute <reply to msg/ user id>`\n    Unmute user in current chat.\n\n• `{i}dmute <reply to msg/ user id>`\n    Mute user in current chat by deleting msgs.\n\n• `{i}undmute <reply to msg/ use id>`\n    Unmute dmuted user in current chat.\n\n• `{i}tmute <time> <reply to msg/ use id>`\n    s- seconds\n    m- minutes\n    h- hours\n    d- days\n    Mute user in current chat with time.\n";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("dmute", "dmute( (.*)|$)", HELP), |ctx| {
        Box::pin(dmute(ctx))
    });
    reg_add(registry, def("undmute", "undmute( (.*)|$)", HELP), |ctx| {
        Box::pin(undmute(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "tmute",
            pattern: "tmute",
            help: HELP,
            groups_only: true,
            ..def("tmute", "tmute", HELP)
        },
        |ctx| Box::pin(tmute(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "unmute",
            pattern: "dbs.mute.unmute( (.*)|$)",
            help: HELP,
            admins_only: true,
            ..def("unmute", "dbs.mute.unmute( (.*)|$)", HELP)
        },
        |ctx| Box::pin(unmute_perm(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "mute",
            pattern: "dbs.mute.mute( (.*)|$)",
            help: HELP,
            admins_only: true,
            ..def("mute", "dbs.mute.mute( (.*)|$)", HELP)
        },
        |ctx| Box::pin(mute_perm(ctx)),
    );
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Markdown mention of a user id (Python `fns.inline_mention` with only
/// the id known).
pub fn mention_id(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
}

/// All delete-mutes (Python `dbs.mute.get_muted()`).
pub async fn get_muted(ctx: &Ctx) -> Result<HashMap<String, Vec<i64>>> {
    Ok(db_get_json(&*ctx.db, "MUTE").await?.unwrap_or_default())
}

async fn set_muted(ctx: &Ctx, map: &HashMap<String, Vec<i64>>) -> Result<()> {
    db_set_json(&*ctx.db, "MUTE", map).await
}

/// Is this user delete-muted in this chat? (Python `dbs.mute.is_muted`.)
pub async fn is_muted(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<bool> {
    Ok(get_muted(ctx)
        .await?
        .get(&chat_id.to_string())
        .map(|l| l.contains(&user_id))
        .unwrap_or(false))
}

pub async fn mute_user(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<()> {
    let mut map = get_muted(ctx).await?;
    let entry = map.entry(chat_id.to_string()).or_default();
    if !entry.contains(&user_id) {
        entry.push(user_id);
    }
    set_muted(ctx, &map).await
}

pub async fn unmute_user(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<()> {
    let mut map = get_muted(ctx).await?;
    if let Some(entry) = map.get_mut(&chat_id.to_string()) {
        entry.retain(|x| *x != user_id);
    }
    set_muted(ctx, &map).await
}

/// Resolve a mute target from an argument or a reply (port of the repeated
/// pattern in `mute.py`): reply -> sender (needs fetch); numeric arg ->
/// id; `@username` -> needs resolve; private chat -> the chat itself.
fn resolve_target(ctx: &Ctx, py_fn: &str) -> Result<i64> {
    let input = ctx.args.trim();
    if !input.is_empty() {
        let first = input.split_whitespace().next().unwrap_or("");
        return match first.trim_start_matches('@').parse::<i64>() {
            Ok(id) => Ok(id),
            Err(_) => Err(todo_op("resolve user id/username", py_fn)),
        };
    }
    if ctx.reply_to_id.is_some() {
        return Err(todo_op("get replied message sender", py_fn));
    }
    if ctx.chat_type == ultroid_core::ChatType::Private {
        return Ok(ctx.chat_id);
    }
    Err(Error::Other("missing_target".to_string()))
}

async fn dmute(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Muting...`").await?;
    let user_id = match resolve_target(ctx, "plugins/mute.py:startmute") {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to a user or add their userid.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    // Python also refuses muting yourself / the assistant bot, and checks
    // the chat's delete_messages admin right — both need entity/chat fetches.
    if is_muted(ctx, ctx.chat_id, user_id).await? {
        ctx.reply("`This user is already muted in this chat.`")
            .await?;
        return Ok(());
    }
    mute_user(ctx, ctx.chat_id, user_id).await?;
    ctx.reply("`Successfully muted...`").await?;
    Ok(())
}

async fn undmute(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Unmuting...`").await?;
    let user_id = match resolve_target(ctx, "plugins/mute.py:endmute") {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to a user or add their userid.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if !is_muted(ctx, ctx.chat_id, user_id).await? {
        ctx.reply("`This user is not muted in this chat.`").await?;
        return Ok(());
    }
    unmute_user(ctx, ctx.chat_id, user_id).await?;
    ctx.reply("`Successfully unmuted...`").await?;
    Ok(())
}

async fn tmute(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Muting...`").await?;
    let parts: Vec<&str> = ctx.text.split_whitespace().collect();
    let Some(tme) = parts.get(1).copied() else {
        ctx.reply("`Time till mute?`").await?;
        return Ok(());
    };
    let user_id = match parts.get(2).copied() {
        Some(tok) => match tok.trim_start_matches('@').parse::<i64>() {
            Ok(id) => id,
            Err(_) => return Err(todo_op("resolve user id/username", "plugins/mute.py:tmute")),
        },
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_op(
                    "get replied message sender",
                    "plugins/mute.py:tmute",
                ));
            }
            ctx.reply(&ctx.get_str("tban_1")).await?;
            return Ok(());
        }
    };
    if user_id == ctx.config.owner_id {
        ctx.reply("`I can't mute myself.`").await?;
        return Ok(());
    }
    let until = crate::admintools::ban_time(tme)?;
    let _ = until;
    // TODO(responder-op): edit_permissions(chat, user, until_date, send_messages=False).
    // Python: plugins/mute.py:tmute handler.
    Err(todo_op(
        "temp-mute user (edit_permissions)",
        "plugins/mute.py:tmute",
    ))
}

async fn unmute_perm(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Unmuting...`").await?;
    let user_id = match resolve_target(ctx, "plugins/mute.py:unmute") {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply(&ctx.get_str("tban_1")).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let _ = user_id;
    // TODO(responder-op): edit_permissions(chat, user, until_date=None, send_messages=True).
    Err(todo_op(
        "unmute user (edit_permissions)",
        "plugins/mute.py:unmute",
    ))
}

async fn mute_perm(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Muting...`").await?;
    let user_id = match resolve_target(ctx, "plugins/mute.py:mute") {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply(&ctx.get_str("tban_1")).await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if user_id == ctx.config.owner_id {
        ctx.reply("`I can't mute myself.`").await?;
        return Ok(());
    }
    // TODO(responder-op): edit_permissions(chat, user, until_date=None, send_messages=False).
    Err(todo_op(
        "mute user (edit_permissions)",
        "plugins/mute.py:mute",
    ))
}

/// The live watcher (Python `watcher`): delete a message when its sender
/// — or the bot it was sent via — is delete-muted in this chat.
/// The delete itself is a client op.
pub async fn watcher_should_delete(
    ctx: &Ctx,
    sender_id: i64,
    via_bot_id: Option<i64>,
) -> Result<bool> {
    if is_muted(ctx, ctx.chat_id, sender_id).await? {
        return Ok(true);
    }
    if let Some(via) = via_bot_id {
        if is_muted(ctx, ctx.chat_id, via).await? {
            return Ok(true);
        }
    }
    Ok(false)
}
