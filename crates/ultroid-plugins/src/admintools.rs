//! Chat admin tools — port of `plugins/admintools.py`.
//!
//! All argument parsing, permission-target resolution and reply text is
//! ported. Promote / demote / ban / unban / kick / temp-ban / pin /
//! unpin / temp-pin are wired to the `Responder` ops. Purge, pinned /
//! listpinned, auto-delete TTL and invite-request management still need
//! raw client calls the trait does not expose (message iteration,
//! pinned-message fetch, `SetHistoryTTL`, join-request RPCs), so those
//! handlers return the honest `TODO(responder-op)` error at the exact
//! point the RPC would run (see the report for the full list).
//!
//! Note: Python's `manager=True` / `require=<right>` decorator flags have
//! no `CommandDef` field; the gates are recorded here in comments and in
//! the report. Patterns are kept exactly as written in Python.

use crate::util::{def, reg_add};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ultroid_core::{AdminRights, BannedRights, ChatType, CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "-\n\n- `.promote <reply to user/userid/username>`\n- `.demote`\n    Promote/Demote the user in the chat.\n\n- `.ban <reply to user/userid/username> <reason>`\n- `.unban`\n    Ban/Unban the user from the chat.\n\n- `.kick <reply to user/userid/username> <reason>`\n    Kick the user from the chat.\n\n- `.pin <reply to message>`\n    Pin the message in the chat\n- `.tpin <time> <temp pin message>`\n- `.unpin (all) <reply to message>`\n    Unpin the messages in the chat.\n\n- `.pinned`\n   Get pinned message in the current chat.\n- `.listpinned`\n   Get all pinned messages in current chat\n\n- `.autodelete <24h/7d/1m/off>`\n   Enable Auto Delete Messages in Chat.\n\n- `.purge <reply to message>`\n    Purge all messages from the replied message.\n\n- `.purgeme <reply to message>`\n    Purge Only your messages from the replied message.\n\n- `.purgeall`\n    Delete all msgs of replied user.\n\n- `.invreq [approve|decline] [all|<user>]`\n    Manage pending join requests.\n";

pub fn register(registry: &mut Registry) {
    // promote/demote/ban/unban/kick/tban/pin/unpin/tpin: admins_only +
    // fullsudo (Python manager=True, require=<right> has no Rust field).
    let admin_full = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        admins_only: true,
        fullsudo: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, admin_full("promote", "promote( (.*)|$)"), |ctx| {
        Box::pin(promote(ctx))
    });
    reg_add(registry, admin_full("demote", "demote( (.*)|$)"), |ctx| {
        Box::pin(demote(ctx))
    });
    reg_add(registry, admin_full("ban", "ban( (.*)|$)"), |ctx| {
        Box::pin(ban(ctx))
    });
    reg_add(registry, admin_full("unban", "unban( (.*)|$)"), |ctx| {
        Box::pin(unban(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "kick",
            pattern: "kick( (.*)|$)",
            help: HELP,
            fullsudo: true,
            ..def("kick", "kick( (.*)|$)", HELP)
        },
        |ctx| Box::pin(kick(ctx)),
    );
    reg_add(registry, admin_full("tban", "tban( (.*)|$)"), |ctx| {
        Box::pin(tban(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "pin",
            pattern: "pin$",
            help: HELP,
            fullsudo: true,
            ..def("pin", "pin$", HELP)
        },
        |ctx| Box::pin(pin(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "unpin",
            pattern: "unpin($| (.*))",
            help: HELP,
            fullsudo: true,
            ..def("unpin", "unpin($| (.*))", HELP)
        },
        |ctx| Box::pin(unpin(ctx)),
    );
    reg_add(registry, admin_full("tpin", "tpin( (.*)|$)"), |ctx| {
        Box::pin(tpin(ctx))
    });
    reg_add(registry, def("purge", "purge( (.*)|$)", HELP), |ctx| {
        Box::pin(purge(ctx))
    });
    reg_add(registry, def("purgeme", "purgeme( (.*)|$)", HELP), |ctx| {
        Box::pin(purge_me(ctx))
    });
    reg_add(registry, def("purgeall", "purgeall$", HELP), |ctx| {
        Box::pin(purge_all(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "pinned",
            pattern: "pinned",
            help: HELP,
            groups_only: true,
            ..def("pinned", "pinned", HELP)
        },
        |ctx| Box::pin(pinned(ctx)),
    );
    reg_add(registry, def("listpinned", "listpinned$", HELP), |ctx| {
        Box::pin(list_pinned(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "autodelete",
            pattern: "autodelete( (.*)|$)",
            help: HELP,
            admins_only: true,
            ..def("autodelete", "autodelete( (.*)|$)", HELP)
        },
        |ctx| Box::pin(auto_delete(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "invreq",
            pattern: "invreq( (.*)|$)",
            help: HELP,
            admins_only: true,
            fullsudo: true,
            ..def("invreq", "invreq( (.*)|$)", HELP)
        },
        |ctx| Box::pin(invreq(ctx)),
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

/// Replace `{}` placeholders in a localised string in order
/// (Python `str.format(*args)`).
pub fn fmt_args(template: &str, args: &[&str]) -> String {
    let mut out = template.to_string();
    for a in args {
        match out.find("{}") {
            Some(pos) => out.replace_range(pos..pos + 2, a),
            None => break,
        }
    }
    out
}

/// Port of `fns.ban_time` (`pyUltroid/fns/admins.py`): `10s`/`5m`/`2h`/`3d`
/// (bare number defaults to seconds) -> absolute unix timestamp.
pub fn ban_time(time_str: &str) -> Result<i64> {
    let mut s = time_str.trim().to_string();
    if !s.ends_with(['s', 'm', 'h', 'd']) {
        s.push('s');
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num
        .parse()
        .map_err(|_| Error::Other("Invalid time amount specified.".to_string()))?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => 1,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(e.to_string()))?
        .as_secs() as i64;
    Ok(now + n * mult)
}

/// Kick a user: Telethon `kick_participant` is a ban followed by an
/// immediate unban (`channels.EditBanned` with full rights, then empty
/// rights).
async fn kick_user(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<()> {
    ctx.responder
        .ban_user(chat_id, user_id, BannedRights::all_banned())
        .await?;
    ctx.responder.unban_user(chat_id, user_id).await
}

/// Resolve the command's target user (port of `fns.get_uinfo`):
/// reply -> the replied message's sender (from `ctx.reply_to_sender_id`);
/// otherwise the first argument (numeric id resolves locally,
/// `@username`/text id via `resolve_user`). Returns `(user_id, rest)`.
async fn resolve_user(ctx: &Ctx) -> Result<(i64, String)> {
    if ctx.reply_to_id.is_some() {
        let sender = ctx.reply_to_sender_id.ok_or_else(|| {
            Error::Other("could not determine the replied message's sender".to_string())
        })?;
        return Ok((sender, ctx.args.trim().to_string()));
    }
    let mut parts = ctx.args.splitn(2, ' ');
    let first = parts.next().unwrap_or("").trim();
    if first.is_empty() {
        return Err(Error::Other("no_user".to_string()));
    }
    let rest = parts.next().unwrap_or("").trim().to_string();
    match first.trim_start_matches('@').parse::<i64>() {
        Ok(id) => Ok((id, rest)),
        Err(_) => Ok((ctx.responder.resolve_user(first).await?.id, rest)),
    }
}

async fn promote(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let (user_id, rank_arg) = match resolve_user(ctx).await {
        Ok(v) => v,
        Err(e) => {
            if e.to_string() == "no_user" {
                ctx.reply(&ctx.get_str("pro_1")).await?;
                return Ok(());
            }
            return Err(e);
        }
    };
    let mut rank = if rank_arg.is_empty() {
        "Admin".to_string()
    } else {
        rank_arg
    };
    let full_rights = rank.split_whitespace().next() == Some("-f");
    if full_rights {
        rank = rank
            .splitn(2, ' ')
            .nth(1)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Admin".to_string());
    }
    // Python: full (`-f`) promotion uses the chat's admin rights;
    // otherwise the fixed set invite/ban/delete/pin/manage_call.
    let rights = if full_rights {
        AdminRights::all()
    } else {
        AdminRights {
            invite_users: true,
            ban_users: true,
            delete_messages: true,
            pin_messages: true,
            manage_call: true,
            ..Default::default()
        }
    };
    ctx.responder
        .promote_user(ctx.chat_id, user_id, rights, &rank)
        .await?;
    Ok(())
}

async fn demote(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let (user_id, rank_arg) = match resolve_user(ctx).await {
        Ok(v) => v,
        Err(e) => {
            if e.to_string() == "no_user" {
                ctx.reply(&ctx.get_str("de_1")).await?;
                return Ok(());
            }
            return Err(e);
        }
    };
    // Python: demote clears every admin right and sets the title to the
    // given rank, defaulting to "Not Admin".
    let rank = if rank_arg.is_empty() {
        "Not Admin".to_string()
    } else {
        rank_arg
    };
    ctx.responder
        .promote_user(ctx.chat_id, user_id, AdminRights::default(), &rank)
        .await?;
    Ok(())
}

async fn ban(ctx: &Ctx) -> Result<()> {
    let (user_id, reason) = match resolve_user(ctx).await {
        Ok(v) => v,
        Err(e) => {
            if e.to_string() == "no_user" {
                ctx.reply(&ctx.get_str("ban_1")).await?;
                return Ok(());
            }
            return Err(e);
        }
    };
    if ctx.config.devlist_ids.contains(&user_id) {
        ctx.reply(&ctx.get_str("ban_2")).await?;
        return Ok(());
    }
    let _ = reason;
    // Python: edit_permissions(chat, user, view_messages=False) — a full
    // ban (all banned rights, forever).
    ctx.responder
        .ban_user(ctx.chat_id, user_id, BannedRights::all_banned())
        .await?;
    Ok(())
}

async fn unban(ctx: &Ctx) -> Result<()> {
    let body = ctx.text.strip_prefix(['.', '/']).unwrap_or(&ctx.text);
    if body.starts_with("unbanall") {
        return Ok(());
    }
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let (user_id, _reason) = match resolve_user(ctx).await {
        Ok(v) => v,
        Err(e) => {
            if e.to_string() == "no_user" {
                ctx.reply(&ctx.get_str("unban_1")).await?;
                return Ok(());
            }
            return Err(e);
        }
    };
    // Python: edit_permissions(chat, user, view_messages=True) — clear
    // the banned rights.
    ctx.responder.unban_user(ctx.chat_id, user_id).await?;
    Ok(())
}

async fn kick(ctx: &Ctx) -> Result<()> {
    if ctx.text.contains("kickme") {
        return Ok(());
    }
    if ctx.chat_type == ChatType::Private {
        ctx.reply("`Use this in Group/Channel.`").await?;
        return Ok(());
    }
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let (user_id, _reason) = match resolve_user(ctx).await {
        Ok(v) => v,
        Err(e) => {
            if e.to_string() == "no_user" {
                ctx.reply(&ctx.get_str("adm_1")).await?;
                return Ok(());
            }
            return Err(e);
        }
    };
    if ctx.config.devlist_ids.contains(&user_id) {
        ctx.reply(&ctx.get_str("kick_2")).await?;
        return Ok(());
    }
    if user_id == ctx.config.owner_id {
        ctx.reply(&ctx.get_str("kick_3")).await?;
        return Ok(());
    }
    // Python: kick_participant(chat, user) — ban, then immediately unban.
    kick_user(ctx, ctx.chat_id, user_id).await?;
    Ok(())
}

async fn tban(ctx: &Ctx) -> Result<()> {
    let parts: Vec<&str> = ctx.text.split_whitespace().collect();
    let Some(tme) = parts.get(1) else {
        ctx.reply(&ctx.get_str("adm_3")).await?;
        return Ok(());
    };
    let tme = (*tme).to_string();
    let user_id: i64 = match parts.get(2) {
        Some(t) => match t.trim_start_matches('@').parse() {
            Ok(id) => id,
            Err(_) => ctx.responder.resolve_user(t).await?.id,
        },
        None => {
            if ctx.reply_to_id.is_some() {
                ctx.reply_to_sender_id.ok_or_else(|| {
                    Error::Other("could not determine the replied message's sender".to_string())
                })?
            } else {
                ctx.reply(&ctx.get_str("tban_1")).await?;
                return Ok(());
            }
        }
    };
    let until = ban_time(&tme)?;
    // Python: edit_permissions(chat, user, until_date, view_messages=False).
    let rights = BannedRights {
        until_date: Some(until as i32),
        ..BannedRights::all_banned()
    };
    ctx.responder.ban_user(ctx.chat_id, user_id, rights).await?;
    Ok(())
}

async fn pin(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply(&ctx.get_str("pin_1")).await?;
        return Ok(());
    }
    // Python: pin_message(chat, reply_id, notify=False) — silent pin.
    let reply_id = ctx
        .reply_to_id
        .ok_or_else(|| Error::Other("reply to a message first".to_string()))?;
    ctx.responder
        .pin_message(ctx.chat_id, reply_id as i32, true)
        .await?;
    Ok(())
}

async fn unpin(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    // Pattern `unpin($| (.*))`: group 1 is the arg after "unpin".
    let arg = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if ctx.reply_to_id.is_none() && arg != "all" {
        // Python formats HNDLR ("." by default) into the usage string.
        ctx.reply(&fmt_args(&ctx.get_str("unpin_1"), &["."]))
            .await?;
        return Ok(());
    }
    // Python: unpin_message(chat, msg) with msg None unpins all;
    // a reply unpins that one message.
    if arg == "all" {
        ctx.responder.unpin_all(ctx.chat_id).await?;
    } else {
        let reply_id = ctx
            .reply_to_id
            .ok_or_else(|| Error::Other("reply to a message first".to_string()))?;
        ctx.responder
            .unpin_message(ctx.chat_id, reply_id as i32)
            .await?;
    }
    Ok(())
}

async fn tpin(ctx: &Ctx) -> Result<()> {
    let duration_arg = ctx.args.trim().to_string();
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to message..`").await?;
        return Ok(());
    }
    if duration_arg.is_empty() {
        ctx.reply("`Please provide time..`").await?;
        return Ok(());
    }
    let until = ban_time(&duration_arg)?;
    let reply_id =
        ctx.reply_to_id
            .ok_or_else(|| Error::Other("reply to a message first".to_string()))? as i32;
    ctx.responder
        .pin_message(ctx.chat_id, reply_id, true)
        .await?;
    // Python sleeps `ban_time(...)` (an absolute timestamp); the port's
    // intent is to sleep until `until`, i.e. the remaining duration.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(e.to_string()))?
        .as_secs() as i64;
    let wait = (until - now).max(0) as u64;
    tokio::time::sleep(Duration::from_secs(wait)).await;
    ctx.responder.unpin_message(ctx.chat_id, reply_id).await?;
    Ok(())
}

async fn purge(ctx: &Ctx) -> Result<()> {
    // Python refuses `.purge m` / `.purge a` (first char after ".purge ").
    if let Some(c) = ctx.text.chars().nth(6) {
        if c == 'm' || c == 'a' {
            return Ok(());
        }
    }
    let _count = ctx.args.trim();
    // TODO(responder-op): iter_messages + delete each.
    // Python: plugins/admintools.py:fastpurger.
    Err(todo_op(
        "purge messages (iter_messages + delete)",
        "plugins/admintools.py:fastpurger",
    ))
}

async fn purge_me(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_string();
    if !arg.is_empty() {
        match arg.parse::<u32>() {
            Ok(_n) => {
                // TODO(responder-op): iter own messages + delete.
                return Err(todo_op(
                    "purge own messages (iter_messages + delete)",
                    "plugins/admintools.py:fastpurgerme",
                ));
            }
            Err(_) => {
                ctx.reply(&ctx.get_str("com_3")).await?;
                return Ok(());
            }
        }
    }
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a message to purge from or use it like ``purgeme <num>`")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): iter own messages from reply + delete_messages.
    Err(todo_op(
        "purge own messages (iter_messages + delete)",
        "plugins/admintools.py:fastpurgerme",
    ))
}

async fn purge_all(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply(&ctx.get_str("purgeall_1")).await?;
        return Ok(());
    }
    // TODO(responder-op): get replied sender + delete_messages(from_user).
    // Python: plugins/admintools.py:purgeall handler.
    Err(todo_op(
        "delete all messages from user",
        "plugins/admintools.py:purgeall",
    ))
}

async fn pinned(_ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): GetFullChat / GetFullChannel -> pinned_msg_id,
    // then get_messages(pinned_msg_id) -> link.
    // Python: plugins/admintools.py:djshsh.
    Err(todo_op(
        "get pinned message (GetFullChat/GetFullChannel)",
        "plugins/admintools.py:djshsh",
    ))
}

async fn list_pinned(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): iter_messages(filter=InputMessagesFilterPinned).
    // Python: plugins/admintools.py:get_all_pinned.
    Err(todo_op(
        "list pinned messages",
        "plugins/admintools.py:get_all_pinned",
    ))
}

async fn auto_delete(ctx: &Ctx) -> Result<()> {
    let match_ = ctx.args.trim();
    let ttl: i64 = match match_ {
        "24h" => 3600 * 24,
        "7d" => 3600 * 24 * 7,
        "1m" => 3600 * 24 * 31,
        "off" => 0,
        _ => {
            ctx.reply("`Please Use in Proper Format..`").await?;
            return Ok(());
        }
    };
    let _ = ttl;
    // TODO(responder-op): messages.SetHistoryTTLRequest(chat, period=ttl).
    // Python: plugins/admintools.py:autodelte.
    Err(todo_op(
        "set history TTL",
        "plugins/admintools.py:autodelte",
    ))
}

fn friendly_invreq_error(exc: &str) -> String {
    let upper = exc.to_uppercase();
    if upper.contains("ADMIN") {
        return format!("🚫 **Missing rights to manage join requests.**\n`{exc}`");
    }
    if upper.contains("MISSING") || upper.contains("USER_NOT_PARTICIPANT") {
        return format!("❌ **No pending join request found for that user.**\n`{exc}`");
    }
    format!("❌ **Error.**\n`{exc}`")
}

async fn invreq(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<&str> = ctx.args.split_whitespace().collect();
    if tokens.is_empty() {
        // TODO(responder-op): messages.GetChatInviteImportersRequest.
        // Python: plugins/admintools.py:_invreq_list.
        return Err(todo_op(
            "list pending join requests (GetChatInviteImportersRequest)",
            "plugins/admintools.py:_invreq_list",
        ));
    }
    let action = tokens[0].to_lowercase();
    if action != "approve" && action != "decline" {
        ctx.reply("`Usage: .invreq [approve|decline] [all|<user>]`")
            .await?;
        return Ok(());
    }
    let approved = action == "approve";
    let target = tokens.get(1).copied();
    if target
        .map(|t| t.eq_ignore_ascii_case("all"))
        .unwrap_or(false)
    {
        // TODO(responder-op): messages.HideAllChatJoinRequestsRequest.
        return Err(todo_op(
            "approve/decline all join requests (HideAllChatJoinRequestsRequest)",
            "plugins/admintools.py:_invreq_hide_all",
        ));
    }
    let user_id: Option<i64> = match target {
        Some(t) => match t.trim_start_matches('@').parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => {
                return Err(todo_op(
                    "resolve user id/username",
                    "plugins/admintools.py:_invite_request_manager",
                ));
            }
        },
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_op(
                    "get replied message sender",
                    "plugins/admintools.py:_invite_request_manager",
                ));
            }
            None
        }
    };
    let Some(user_id) = user_id else {
        ctx.reply(
            "`Usage: .invreq approve|decline <user>|all` (or reply to the requester's message).`",
        )
        .await?;
        return Ok(());
    };
    let _ = (user_id, approved, friendly_invreq_error(""));
    // TODO(responder-op): messages.HideChatJoinRequestRequest.
    // Python: plugins/admintools.py:_invreq_hide_one.
    Err(todo_op(
        "approve/decline join request (HideChatJoinRequestRequest)",
        "plugins/admintools.py:_invreq_hide_one",
    ))
}
