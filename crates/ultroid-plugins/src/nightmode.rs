//! Night mode — port of `plugins/nightmode.py`.
//!
//! DB keys (raw udB keys, same as Python `KeyManager("NIGHT_CHATS")` and
//! `udB.set_key("NIGHT_TIME", ...)`):
//! `NIGHT_CHATS` is a list of chat ids; `NIGHT_TIME` is the string form of
//! a 4-int list `[close_hour, close_min, open_hour, open_min]`
//! (default close 00:00, open 07:00).
//!
//! `.nmtime` / `.addnm` / `.remnm` / `.listnm` are fully ported (list uses
//! the id fallback, exactly as Python's `except` branch does when
//! `get_entity` fails). The cron scheduler (Python `apscheduler`) has no
//! Rust equivalent in this crate — the close/open jobs are ported as
//! callable async functions (`close_groups` / `open_groups`) for the
//! binary's scheduler to run, wired to `edit_default_rights` +
//! `send_to`. Resolving a `@username` chat argument in `.addnm` /
//! `.remnm` still has no trait op and stays a `TODO(responder-op)` site.

use crate::util::{def, reg_add};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{BannedRights, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\nAt Night it will turn off everyone permission to send message in  an all groups which you added via `{i}addnight`\nAnd Turn On auto at morning\n\n• `{i}addnm`\n   Add NightMode\n   To Add Group To Auto Night Mode.\n\n• `{i}remnm`\n   Remove NightMode\n   To remove Group From Auto Night Mode\n\n• `{i}listnm`\n   List NightMode\n   To Get All List of Groups where NightMode Active.\n\n• `{i}nmtime <close hour> <close min> <open hour> <open min>`\n   NightMode Time\n   By Default Its close 00:00 , open 07:00\n   Use 24hr format\n   Ex- `nmtime 01 00 06 30`\n";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("nmtime", "nmtime( (.*)|$)", HELP), |ctx| {
        Box::pin(set_time(ctx))
    });
    reg_add(registry, def("addnm", "addnm( (.*)|$)", HELP), |ctx| {
        Box::pin(add_group(ctx))
    });
    reg_add(registry, def("remnm", "remnm( (.*)|$)", HELP), |ctx| {
        Box::pin(remove_group(ctx))
    });
    reg_add(registry, def("listnm", "listnm$", HELP), |ctx| {
        Box::pin(list_groups(ctx))
    });
}

/// (close_hour, close_min, open_hour, open_min). Default (0, 0, 7, 0).
/// (Python stores the plain string `str([h1, m1, h2, m2])` under
/// `NIGHT_TIME`, so this reads/writes the raw key, not JSON.)
pub async fn night_time(ctx: &Ctx) -> Result<(u32, u32, u32, u32)> {
    let raw = ctx.db.get("NIGHT_TIME").await?;
    Ok(parse_night_time(raw.as_deref()))
}

/// Parse the stored `NIGHT_TIME` string (`"[1, 0, 6, 30]"`) with the
/// (0, 0, 7, 0) default on any failure (Python `eval(...)` with defaults).
pub fn parse_night_time(raw: Option<&str>) -> (u32, u32, u32, u32) {
    let default = (0, 0, 7, 0);
    let Some(raw) = raw else { return default };
    let nums: Vec<u32> = raw
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .filter_map(|p| p.trim().parse::<u32>().ok())
        .collect();
    if nums.len() == 4 {
        (nums[0], nums[1], nums[2], nums[3])
    } else {
        default
    }
}

/// Chats with night mode on (Python `KeyManager("NIGHT_CHATS")`).
pub async fn night_chats(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(db_get_json(&*ctx.db, "NIGHT_CHATS")
        .await?
        .unwrap_or_default())
}

async fn set_night_chats(ctx: &Ctx, chats: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "NIGHT_CHATS", &chats.to_vec()).await
}

async fn set_time(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply(&ctx.get_str("nightm_1")).await?;
        return Ok(());
    }
    let parts: Vec<&str> = arg.split_whitespace().collect();
    if parts.len() != 4 {
        ctx.reply(&ctx.get_str("nightm_1")).await?;
        return Ok(());
    }
    let mut nums = Vec::with_capacity(4);
    for p in &parts {
        match p.parse::<i64>() {
            Ok(n) => nums.push(n),
            Err(_) => {
                ctx.reply(&ctx.get_str("nightm_1")).await?;
                return Ok(());
            }
        }
    }
    ctx.db.set("NIGHT_TIME", &format!("{nums:?}")).await?;
    ctx.reply(&ctx.get_str("nightm_2")).await?;
    Ok(())
}

async fn add_group(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let mut chats = night_chats(ctx).await?;
    if !arg.is_empty() {
        match arg.trim_start_matches('@').parse::<i64>() {
            Ok(id) => {
                if !chats.contains(&id) {
                    chats.push(id);
                }
                set_night_chats(ctx, &chats).await?;
                ctx.reply(&format!("Done, Added {arg} To Night Mode."))
                    .await?;
                return Ok(());
            }
            Err(_) => {
                // TODO(responder-op): get_entity(arg) -> id.
                // Python: nightmode.py:add_grp.
                return Err(Error::Other(
                    "resolve chat id/username requires a binary Responder op (Python: plugins/nightmode.py:add_grp)".into(),
                ));
            }
        }
    }
    if !chats.contains(&ctx.chat_id) {
        chats.push(ctx.chat_id);
    }
    set_night_chats(ctx, &chats).await?;
    ctx.reply(&ctx.get_str("nightm_3")).await?;
    Ok(())
}

async fn remove_group(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let mut chats = night_chats(ctx).await?;
    if !arg.is_empty() {
        match arg.trim_start_matches('@').parse::<i64>() {
            Ok(id) => {
                chats.retain(|c| *c != id);
                set_night_chats(ctx, &chats).await?;
                ctx.reply(&format!("Done, Removed {arg} To Night Mode."))
                    .await?;
                return Ok(());
            }
            Err(_) => {
                return Err(Error::Other(
                    "resolve chat id/username requires a binary Responder op (Python: plugins/nightmode.py:r_em_grp)".into(),
                ));
            }
        }
    }
    chats.retain(|c| *c != ctx.chat_id);
    set_night_chats(ctx, &chats).await?;
    ctx.reply(&ctx.get_str("nightm_4")).await?;
    Ok(())
}

async fn list_groups(ctx: &Ctx) -> Result<()> {
    let chats = night_chats(ctx).await?;
    // Python resolves each id to @username/title and falls back to the raw
    // id on failure; without a client fetch the port lists the ids.
    let mut name = "NightMode Groups Are-:\n\n".to_string();
    for x in chats {
        name.push_str(&x.to_string());
    }
    ctx.reply(&name).await?;
    Ok(())
}

/// Close every night-mode group (Python `close_grp`): ban `send_messages`
/// for everyone, then announce the open time (from `NIGHT_TIME`).
/// Per-chat failures are swallowed, exactly like Python's per-chat
/// `try/except: LOGS.info(er)`.
pub async fn close_groups(ctx: &Ctx) -> Result<()> {
    let (_, _, open_h, open_m) = night_time(ctx).await?;
    let chats = night_chats(ctx).await?;
    for chat in chats {
        let rights = BannedRights {
            send_messages: true,
            ..Default::default()
        };
        if ctx
            .responder
            .edit_default_rights(chat, rights)
            .await
            .is_ok()
        {
            let _ = ctx
                .responder
                .send_to(
                    chat,
                    &format!(
                        "**NightMode : Group Closed**\n\nGroup Will Open At `{open_h}:{open_m}`"
                    ),
                )
                .await;
        }
    }
    Ok(())
}

/// Open every night-mode group again (Python `open_grp`): lift the
/// default bans, then announce.
pub async fn open_groups(ctx: &Ctx) -> Result<()> {
    let chats = night_chats(ctx).await?;
    for chat in chats {
        if ctx
            .responder
            .edit_default_rights(chat, BannedRights::default())
            .await
            .is_ok()
        {
            let _ = ctx
                .responder
                .send_to(chat, "**NightMode Off**\n\nGroup Opened 🥳.")
                .await;
        }
    }
    Ok(())
}
