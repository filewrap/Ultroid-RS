//! User warnings — port of `plugins/warn.py`.
//!
//! DB keys (raw udB keys, same as Python `pyUltroid/dB/warn_db.py`):
//! `WARNS` maps chat id -> {user id: [count, "reason|$|reason"]} and
//! `SETWARN` holds `"<count> <ban|mute|kick>"` (default `3 kick`).
//!
//! Warn counting, reason accumulation and threshold evaluation are fully
//! ported, and the threshold actions (ban / kick / mute RPCs) and target
//! resolution are wired to the `Responder` ops. Name lookups still fall
//! back to the `[id](tg://user?id=…)` mention form where Python would use
//! the fetched display name.
//!
//! Patterns are kept exactly as written in Python, including the literal
//! `dbs.warn.warns` prefix on the `warns` command.

use crate::util::{def, reg_add};
use serde_json::{Value, json};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{BannedRights, CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available\n\n•`{i}warn <reply to user> <reason>`\n    Gives Warn.\n\n•`{i}resetwarn <reply to user>`\n    To reset All Warns.\n\n•`{i}warns <reply to user>`\n   To Get List of Warnings of a user.\n\n•`{i}setwarn <warn count> | <ban/mute/kick>`\n   Set Number in warn count for warnings\n   After putting \" | \" mark put action like ban/mute/kick\n   Its Default 3 kick\n   Example : `setwarn 5 | mute`\n\n";

pub fn register(registry: &mut Registry) {
    let admin_group = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        groups_only: true,
        admins_only: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, admin_group("warn", "warn( (.*)|$)"), |ctx| {
        Box::pin(warn(ctx))
    });
    reg_add(
        registry,
        admin_group("resetwarn", "resetwarn( (.*)|$)"),
        |ctx| Box::pin(reset_warn(ctx)),
    );
    reg_add(
        registry,
        admin_group("warns", "dbs.warn.warns( (.*)|$)"),
        |ctx| Box::pin(list_warns(ctx)),
    );
    reg_add(registry, def("setwarn", "setwarn( (.*)|$)", HELP), |ctx| {
        Box::pin(set_warn(ctx))
    });
}

/// Markdown mention of a user id (Python `fns.inline_mention` with only
/// the id known).
pub fn mention_id(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
}

/// All warns (Python `dbs.warn.get_stuff()`): chat -> user -> [count, reasons].
pub async fn get_warns_map(ctx: &Ctx) -> Result<HashMap<String, HashMap<String, Value>>> {
    Ok(db_get_json(&*ctx.db, "WARNS").await?.unwrap_or_default())
}

async fn set_warns_map(ctx: &Ctx, map: &HashMap<String, HashMap<String, Value>>) -> Result<()> {
    db_set_json(&*ctx.db, "WARNS", map).await
}

/// (count, reasons-string) for one user (Python `dbs.warn.warns`).
pub async fn warns_of(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<(i64, Option<String>)> {
    let map = get_warns_map(ctx).await?;
    if let Some(entry) = map
        .get(&chat_id.to_string())
        .and_then(|u| u.get(&user_id.to_string()))
        .and_then(|v| v.as_array())
    {
        let count = entry.first().and_then(|v| v.as_i64()).unwrap_or(0);
        let reasons = entry.get(1).and_then(|v| v.as_str()).map(str::to_string);
        return Ok((count, reasons));
    }
    Ok((0, None))
}

pub async fn add_warn(
    ctx: &Ctx,
    chat_id: i64,
    user_id: i64,
    count: i64,
    reasons: &str,
) -> Result<()> {
    let mut map = get_warns_map(ctx).await?;
    map.entry(chat_id.to_string())
        .or_default()
        .insert(user_id.to_string(), json!([count, reasons]));
    set_warns_map(ctx, &map).await
}

pub async fn reset_warns(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<()> {
    let mut map = get_warns_map(ctx).await?;
    if let Some(users) = map.get_mut(&chat_id.to_string()) {
        users.remove(&user_id.to_string());
    }
    set_warns_map(ctx, &map).await
}

/// The configured warn threshold (Python `udB.get_key("SETWARN")`):
/// `(count, action)` with default `(3, "kick")`. Stored as a plain string
/// `"<count> <action>"` in Python, so this reads the raw key (not JSON).
/// Note Python's `if ("ban" or "kick" or "mute") not in action` is
/// always-false (`"ban"` is truthy), so the fallback never fires — the
/// port keeps the same effective behaviour (any action string is
/// accepted as-is).
pub async fn warn_threshold(ctx: &Ctx) -> Result<(i64, String)> {
    let raw = ctx.db.get("SETWARN").await?;
    Ok(parse_setwarn(raw.as_deref()))
}

pub fn parse_setwarn(raw: Option<&str>) -> (i64, String) {
    let default = (3, "kick".to_string());
    let Some(raw) = raw else { return default };
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() < 2 {
        return default;
    }
    let Ok(n) = parts[0].parse::<i64>() else {
        return default;
    };
    (n, parts[1].to_string())
}

/// Kick a user: Telethon `kick_participant` is a ban followed by an
/// immediate unban.
async fn kick_user(ctx: &Ctx, user_id: i64) -> Result<()> {
    ctx.responder
        .ban_user(ctx.chat_id, user_id, BannedRights::all_banned())
        .await?;
    ctx.responder.unban_user(ctx.chat_id, user_id).await
}

/// Resolve the warned user from a reply or an argument (port of the
/// pattern repeated across `warn.py`): reply -> the replied message's
/// sender (from `ctx.reply_to_sender_id`); `@username` -> `resolve_user`;
/// numeric id -> ok.
async fn resolve_target(ctx: &Ctx, py_fn: &str) -> Result<i64> {
    let _ = py_fn;
    if ctx.reply_to_id.is_some() {
        return ctx.reply_to_sender_id.ok_or_else(|| {
            Error::Other("could not determine the replied message's sender".to_string())
        });
    }
    let tok = ctx.text.split_whitespace().nth(1).unwrap_or("");
    if tok.is_empty() {
        return Err(Error::Other("missing_target".to_string()));
    }
    match tok.trim_start_matches('@').parse::<i64>() {
        Ok(id) => Ok(id),
        Err(_) => Ok(ctx.responder.resolve_user(tok).await?.id),
    }
}

async fn warn(ctx: &Ctx) -> Result<()> {
    // Python ignores `.warnXYZ` (no space after ".warn" when len > 5).
    if ctx.text.len() > 5 && !ctx.text[5..].starts_with(' ') {
        return Ok(());
    }
    let (user_id, reason) = if ctx.reply_to_id.is_some() {
        let reason = if ctx.args.trim().is_empty() {
            "unknown".to_string()
        } else {
            ctx.text[5..].trim().to_string()
        };
        let user_id = resolve_target(ctx, "plugins/warn.py:warn").await?;
        (user_id, reason)
    } else {
        let user_id = match resolve_target(ctx, "plugins/warn.py:warn").await {
            Ok(id) => id,
            Err(e) if e.to_string() == "missing_target" => {
                ctx.reply("Reply To A User").await?;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let reason = ctx
            .text
            .splitn(3, ' ')
            .nth(2)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        (user_id, reason)
    };

    let (count, prev_reasons) = warns_of(ctx, ctx.chat_id, user_id).await?;
    let reasons = match prev_reasons {
        Some(r) => format!("{r}|$|{reason}"),
        None => reason.clone(),
    };
    let (threshold, action) = warn_threshold(ctx).await?;
    if count + 1 >= threshold {
        add_warn(ctx, ctx.chat_id, user_id, count + 1, &reasons).await?;
        let (final_count, final_reasons) = warns_of(ctx, ctx.chat_id, user_id).await?;
        let (action_taken, _action_err) = match action.as_str() {
            _ if action.contains("ban") => {
                ("ban", "edit_permissions(chat, user, view_messages=False)")
            }
            _ if action.contains("mute") => {
                ("mute", "edit_permissions(chat, user, send_messages=False)")
            }
            _ => ("kick", "kick_participant(chat, user)"),
        };
        // Threshold action, then report (Python: plugins/warn.py:warn).
        match action_taken {
            "ban" => {
                ctx.responder
                    .ban_user(ctx.chat_id, user_id, BannedRights::all_banned())
                    .await?;
            }
            "mute" => {
                ctx.responder
                    .ban_user(
                        ctx.chat_id,
                        user_id,
                        BannedRights {
                            send_messages: true,
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            _ => {
                kick_user(ctx, user_id).await?;
            }
        }
        let reason_list: Vec<&str> = final_reasons
            .as_deref()
            .unwrap_or("")
            .split("|$|")
            .collect();
        let mut text = format!(
            "User {} Got {action_taken} Due to {} Warns.\n\n",
            mention_id(user_id),
            count + 1
        );
        for (i, r) in reason_list.iter().enumerate().take(final_count as usize) {
            text.push_str(&format!("•**{}.** {r}\n", i + 1));
        }
        reset_warns(ctx, ctx.chat_id, user_id).await?;
        ctx.reply(&text).await?;
        return Ok(());
    }
    add_warn(ctx, ctx.chat_id, user_id, count + 1, &reasons).await?;
    ctx.reply(&format!(
        "**WARNING :** {}/{threshold}\n**To :**{}\n**Be Careful !!!**\n\n**Reason** : {reason}",
        count + 1,
        mention_id(user_id)
    ))
    .await?;
    Ok(())
}

async fn reset_warn(ctx: &Ctx) -> Result<()> {
    let user_id = match resolve_target(ctx, "plugins/warn.py:rwarn").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("Reply To user").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    reset_warns(ctx, ctx.chat_id, user_id).await?;
    ctx.reply(&format!("Cleared All Warns of {}.", mention_id(user_id)))
        .await?;
    Ok(())
}

async fn list_warns(ctx: &Ctx) -> Result<()> {
    let user_id = match resolve_target(ctx, "plugins/warn.py:twarns").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("Reply To A User").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let (count, reasons) = warns_of(ctx, ctx.chat_id, user_id).await?;
    match (count, reasons) {
        (c, Some(r)) if c > 0 => {
            let parts: Vec<&str> = r.split("|$|").collect();
            let mut text = format!("User {} Got {c} Warns.\n\n", mention_id(user_id));
            for (i, reason) in parts.iter().enumerate().take(c as usize) {
                text.push_str(&format!("•**{}.** {reason}\n", i + 1));
            }
            ctx.reply(&text).await?;
        }
        _ => {
            ctx.reply("`No Warnings`").await?;
        }
    }
    Ok(())
}

async fn set_warn(ctx: &Ctx) -> Result<()> {
    let ok = ctx.args.trim().to_string();
    if ok.is_empty() {
        ctx.reply("stuff").await?;
        return Ok(());
    }
    if ok.contains('|') {
        // Python: number = int(ok.split()[0]); action = ok.split()[1].
        let parts: Vec<&str> = ok.split_whitespace().collect();
        let parsed = match (parts.first(), parts.get(1)) {
            (Some(n), Some(a)) => n.parse::<i64>().ok().map(|num| (num, (*a).to_string())),
            _ => None,
        };
        let Some((number, action)) = parsed else {
            ctx.reply(&ctx.get_str("schdl_2")).await?;
            return Ok(());
        };
        if !["ban", "kick", "mute"].iter().any(|a| action.contains(a)) {
            ctx.reply("`Only mute / ban / kick option suported`")
                .await?;
            return Ok(());
        }
        ctx.db.set("SETWARN", &format!("{number} {action}")).await?;
        ctx.reply(&format!(
            "Done Your Warn Count is now {number} and Action is {action}"
        ))
        .await?;
    } else {
        ctx.reply(&ctx.get_str("schdl_2")).await?;
    }
    Ok(())
}
