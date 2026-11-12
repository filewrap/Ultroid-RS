//! Anti-flood — port of `plugins/antiflood.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/antiflood_db.py`):
//! `ANTIFLOOD` maps chat id -> flood limit (messages in a row from one
//! user before muting).
//!
//! `.setflood` / `.remflood` / `.getflood` are fully ported. The live
//! flood watcher (`NewMessage` listener) and the `anti_<user>_<chat>`
//! unmute callback are event-driven: the counting + decision logic is
//! ported as pure helpers (`FloodCounter`, `should_mute`) for the
//! binary's update loop, and the unmute RPC is wired to `unban_user`.
//! (The flood-mute half lives in the binary's update loop, not here.)

use crate::util::{def, reg_add};
use serde_json::Value;
use std::collections::HashMap;
use ultroid_core::CommandDef;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Registry, Result};

pub const HELP: &str = " -\n\n- `{i}setflood <integer>`\n    Set flood limit in a chat.\n\n- `{i}remflood`\n    Remove flood limit from a chat.\n\n- `{i}getflood`\n    Get flood limit of a chat.\n";

pub fn register(registry: &mut Registry) {
    let admin = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        admins_only: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, admin("setflood", "setflood ?(\\d+)"), |ctx| {
        Box::pin(set_flood(ctx))
    });
    reg_add(registry, admin("remflood", "remflood$"), |ctx| {
        Box::pin(remove_flood(ctx))
    });
    reg_add(registry, admin("getflood", "getflood$"), |ctx| {
        Box::pin(get_flood(ctx))
    });
}

/// All flood limits (Python `dbs.antiflood.get_flood()`).
pub async fn get_flood_map(ctx: &Ctx) -> Result<HashMap<String, Value>> {
    Ok(db_get_json(&*ctx.db, "ANTIFLOOD")
        .await?
        .unwrap_or_default())
}

async fn set_flood_map(ctx: &Ctx, map: &HashMap<String, Value>) -> Result<()> {
    db_set_json(&*ctx.db, "ANTIFLOOD", map).await
}

/// Flood limit for one chat (Python `dbs.antiflood.get_flood_limit`).
pub async fn flood_limit(ctx: &Ctx, chat_id: i64) -> Result<Option<i64>> {
    Ok(get_flood_map(ctx)
        .await?
        .get(&chat_id.to_string())
        .and_then(|v| v.as_i64()))
}

async fn set_flood(ctx: &Ctx) -> Result<()> {
    // Pattern `setflood ?(\d+)` — group 1 is the limit.
    let input = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if input.is_empty() {
        ctx.reply("`What?`").await?;
        return Ok(());
    }
    if !input.chars().all(|c| c.is_ascii_digit()) {
        ctx.reply(&ctx.get_str("com_3")).await?;
        return Ok(());
    }
    let mut map = get_flood_map(ctx).await?;
    map.insert(
        ctx.chat_id.to_string(),
        Value::from(input.parse::<i64>().unwrap_or(0)),
    );
    set_flood_map(ctx, &map).await?;
    ctx.reply(&fmt_args(&ctx.get_str("antiflood_4"), &[&input]))
        .await?;
    Ok(())
}

async fn remove_flood(ctx: &Ctx) -> Result<()> {
    let mut map = get_flood_map(ctx).await?;
    let had = map.remove(&ctx.chat_id.to_string()).is_some();
    if had {
        set_flood_map(ctx, &map).await?;
        ctx.reply(&ctx.get_str("antiflood_1")).await?;
    } else {
        ctx.reply(&ctx.get_str("antiflood_2")).await?;
    }
    Ok(())
}

async fn get_flood(ctx: &Ctx) -> Result<()> {
    match flood_limit(ctx, ctx.chat_id).await? {
        Some(limit) => {
            ctx.reply(&fmt_args(
                &ctx.get_str("antiflood_5"),
                &[&limit.to_string()],
            ))
            .await?;
        }
        None => {
            ctx.reply(&ctx.get_str("antiflood_2")).await?;
        }
    }
    Ok(())
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

/// In-memory per-chat flood counter (Python `_check_flood`).
/// Keyed by chat id -> (sender id, consecutive count).
#[derive(Default)]
pub struct FloodCounter {
    state: HashMap<i64, (i64, u32)>,
}

impl FloodCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one message; returns the sender's consecutive count
    /// (Python `flood_checm` counting: resets to 1 when the sender changes).
    pub fn record(&mut self, chat_id: i64, sender_id: i64) -> u32 {
        match self.state.get(&chat_id) {
            Some((last_sender, count)) if *last_sender == sender_id => {
                let next = *count + 1;
                self.state.insert(chat_id, (sender_id, next));
                next
            }
            _ => {
                self.state.insert(chat_id, (sender_id, 1));
                1
            }
        }
    }

    pub fn clear_chat(&mut self, chat_id: i64) {
        self.state.remove(&chat_id);
    }
}

/// Should this sender be muted for flooding? (Python `flood_checm`:
/// count >= the chat's limit, sender is not an admin/bot/dev.)
pub fn should_mute(
    count: u32,
    limit: Option<i64>,
    is_admin: bool,
    is_bot: bool,
    is_dev: bool,
) -> bool {
    if is_admin || is_bot || is_dev {
        return false;
    }
    match limit {
        Some(l) => count >= l as u32,
        None => false,
    }
}

/// Parse the `anti_<user>_<chat>` unmute-button payload
/// (Python `unmuting` callback).
pub fn parse_anti_payload(payload: &str) -> Option<(i64, i64)> {
    let rest = payload.strip_prefix("anti_")?;
    let (user, chat) = rest.split_once('_')?;
    Some((user.parse::<i64>().ok()?, chat.parse::<i64>().ok()?))
}

/// Unmute a flood-muted user (Python `unmuting`).
/// Python lifts the send ban with `edit_permissions(send_messages=True)`;
/// `unban_user` clears the banned rights, which lifts it.
pub async fn unmute_flooded(ctx: &Ctx, user_id: i64, chat_id: i64) -> Result<()> {
    ctx.responder.unban_user(chat_id, user_id).await?;
    Ok(())
}
