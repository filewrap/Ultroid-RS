//! Force-subscribe — port of `plugins/forcesubscribe.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/forcesub_db.py`):
//! `FORCESUB` maps chat id -> channel/chat id users must join.
//!
//! `.fsub` / `.remfsub` / `.checkfsub` are fully ported. The live
//! enforcement watcher, the inline `fsub <user>_<channel>` query and the
//! `unm_<user>_<channel>` unmute callback are event/inline-driven: their
//! pure logic (member-count cache, payload parsing) is ported below, and
//! every client RPC (participant checks, permission edits, inline queries,
//! invite-link export) is a `TODO(responder-op)` site.

use crate::util::{def, reg_add};
use serde_json::Value;
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{BannedRights, CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}fsub <chat username><id>`\n    Enable ForceSub in Used Chat !\n\n• `{i}checkfsub`\n    Check/Get Active ForceSub Setting of Used Chat.\n\n• `{i}remfsub`\n    Remove ForceSub from Used Chat !\n\n    Note - You Need to be Admin in Both Channel/Chats\n        in order to Use ForceSubscribe.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "fsub",
            pattern: "fsub( (.*)|$)",
            help: HELP,
            admins_only: true,
            groups_only: true,
            ..def("fsub", "fsub( (.*)|$)", HELP)
        },
        |ctx| Box::pin(add_forcesub(ctx)),
    );
    reg_add(registry, def("remfsub", "remfsub$", HELP), |ctx| {
        Box::pin(remove_forcesub(ctx))
    });
    reg_add(registry, def("checkfsub", "checkfsub$", HELP), |ctx| {
        Box::pin(check_forcesub(ctx))
    });
}

/// All force-sub settings (Python `dbs.forcesub.get_chats()`).
pub async fn get_forcesub_map(ctx: &Ctx) -> Result<HashMap<String, Value>> {
    Ok(db_get_json(&*ctx.db, "FORCESUB").await?.unwrap_or_default())
}

async fn set_forcesub_map(ctx: &Ctx, map: &HashMap<String, Value>) -> Result<()> {
    db_set_json(&*ctx.db, "FORCESUB", map).await
}

/// The channel a chat forces subscription to, if any
/// (Python `dbs.forcesub.get_setting`).
pub async fn forcesub_setting(ctx: &Ctx, chat_id: i64) -> Result<Option<i64>> {
    Ok(get_forcesub_map(ctx)
        .await?
        .get(&chat_id.to_string())
        .and_then(|v| v.as_i64()))
}

async fn add_forcesub(ctx: &Ctx) -> Result<()> {
    let target = ctx.args.trim();
    if target.is_empty() {
        ctx.reply(&ctx.get_str("fsub_1")).await?;
        return Ok(());
    }
    let target_id: i64 = match target.trim_start_matches('@').parse() {
        Ok(id) => id,
        Err(_) => {
            // TODO(responder-op): parse_id / get_entity for @usernames.
            // Python: forcesubscribe.py:addfor (client.parse_id).
            return Err(Error::Other(
                "resolve channel id/username requires a binary Responder op (Python: plugins/forcesubscribe.py:addfor)".into(),
            ));
        }
    };
    let mut map = get_forcesub_map(ctx).await?;
    map.insert(ctx.chat_id.to_string(), Value::from(target_id));
    set_forcesub_map(ctx, &map).await?;
    ctx.reply("Added ForceSub in This Chat !").await?;
    Ok(())
}

async fn remove_forcesub(ctx: &Ctx) -> Result<()> {
    let mut map = get_forcesub_map(ctx).await?;
    let removed = map.remove(&ctx.chat_id.to_string()).is_some();
    if removed {
        set_forcesub_map(ctx, &map).await?;
    }
    if !removed {
        ctx.reply(&ctx.get_str("fsub_3")).await?;
        return Ok(());
    }
    ctx.reply("Removed ForceSub...").await?;
    Ok(())
}

async fn check_forcesub(ctx: &Ctx) -> Result<()> {
    match forcesub_setting(ctx, ctx.chat_id).await? {
        None => {
            ctx.reply("ForceSub is Not Active In This Chat !").await?;
        }
        Some(channel_id) => {
            let title = ctx.responder.chat_info(channel_id).await?.title;
            ctx.reply(&format!(
                "**ForceSub Status** : `Active`\n- **{title}** `({channel_id})`"
            ))
            .await?;
        }
    }
    Ok(())
}

/// Is this user a participant of the channel? The trait has no
/// `GetParticipant` op, so this pages `participants` until the user is
/// found or the list is exhausted.
async fn is_channel_member(ctx: &Ctx, channel_id: i64, user_id: i64) -> Result<bool> {
    let mut offset = 0;
    loop {
        let (page, total) = ctx.responder.participants(channel_id, 200, offset).await?;
        if page.iter().any(|p| p.user.id == user_id) {
            return Ok(true);
        }
        offset += page.len() as i32;
        if page.is_empty() || offset >= total {
            return Ok(false);
        }
    }
}

/// The mute rights Python uses (`edit_permissions(..., send_messages=
/// False)`): only the send-messages right is banned.
fn mute_rights() -> BannedRights {
    BannedRights {
        send_messages: true,
        ..Default::default()
    }
}

/// Message-count cache for the enforcement watcher (Python `CACHE`):
/// a user is only checked on their 1st message in a chat (counts 2-10 are
/// skipped, count resets at 11).
#[derive(Default)]
pub struct ForceSubCache {
    counts: HashMap<i64, HashMap<i64, u32>>,
}

impl ForceSubCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a message; returns `true` when the sender must be checked
    /// for membership this time (count == 1, or the 11-count reset).
    pub fn should_check(&mut self, chat_id: i64, user_id: i64) -> bool {
        let entry = self.counts.entry(chat_id).or_default();
        let count = entry.entry(user_id).or_insert(0);
        *count += 1;
        if *count == 11 {
            *count = 1;
            return false;
        }
        *count == 1
    }
}

/// Parse the `unm_<user>_<channel>` unmute payload
/// (Python `diesoon` callback) and the `fsub <user>_<channel>` inline
/// query argument (Python `fcall`).
pub fn parse_unmute_payload(payload: &str) -> Option<(i64, i64)> {
    let rest = payload.strip_prefix("unm_")?;
    let (user, channel) = rest.split_once('_')?;
    Some((user.parse::<i64>().ok()?, channel.parse::<i64>().ok()?))
}

/// The force-sub prompt text (Python `fcall` inline article).
pub fn forcesub_prompt(user_mention: &str, channel_title: &str) -> String {
    format!("Hi {user_mention}, You Need to Join {channel_title} in order to Chat in this Group.")
}

/// Unmute a user after they joined the required channel
/// (Python `diesoon`): checks membership, then unmutes.
pub async fn unmute_after_join(ctx: &Ctx, user_id: i64, channel_id: i64) -> Result<()> {
    if !is_channel_member(ctx, channel_id, user_id).await? {
        return Err(Error::Other(
            "Please Join That Channel !\nThen Click This Button !".to_string(),
        ));
    }
    // Python: edit_permissions(chat, user, send_messages=True,
    // until_date=None) — `unban_user` clears the banned rights, which
    // lifts the send ban.
    ctx.responder.unban_user(ctx.chat_id, user_id).await?;
    Ok(())
}

/// Enforce force-sub on one incoming message (Python `force_sub`):
/// mute the non-member, then prompt them via the assistant inline query.
pub async fn enforce_forcesub(
    ctx: &Ctx,
    chat_id: i64,
    user_id: i64,
    channel_id: i64,
    sender_is_bot: bool,
) -> Result<()> {
    // Python: get_permissions(channel, user) succeeds for members.
    if is_channel_member(ctx, channel_id, user_id).await? {
        return Ok(());
    }
    if sender_is_bot {
        // Channel senders get banned (view_messages=False) and no prompt.
        ctx.responder
            .ban_user(chat_id, user_id, BannedRights::all_banned())
            .await?;
        return Ok(());
    }
    ctx.responder
        .ban_user(chat_id, user_id, mute_rights())
        .await?;
    let Some(asst_id) = ctx.asst_dialog_id else {
        return Err(Error::Other(
            "assistant bot is not connected, cannot send the force-sub prompt".to_string(),
        ));
    };
    ctx.responder
        .inline_query_click(
            asst_id,
            &format!("fsub {user_id}_{channel_id}"),
            chat_id,
            None,
        )
        .await?;
    Ok(())
}
