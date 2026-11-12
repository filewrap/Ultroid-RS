//! Private-message bot (chat with people through the assistant bot).
//!
//! Port of `assistant/pmbot.py` (based on TeleBot's pmbot incoming).
//!
//! Flows:
//! * Incoming private message (not a `/` command, not the owner,
//!   sender not in the `BOTBLS` ban list) — force-subscribe check
//!   against udB `PMBOT_FSUB` (both the native list form and the
//!   legacy stringified-Python-list form, parsed safely), then the
//!   message is forwarded to the owner and the forwarded id is
//!   recorded in `botchat` (`botchat:<forwarded_id> -> sender_id`)
//!   so the owner's reply finds its way back. A forwarded message
//!   gets a `From <user> [<id>]` note on the forwarded copy.
//! * Outgoing owner reply in private (replying to a forwarded
//!   message) — `/who` answers with the user's info; any other `/`
//!   command is ignored; anything else is sent to the recorded user.
//! * `/ban` / `/unban` (owner-only, private, reply-to-a-forwarded
//!   message) — manage the `BOTBLS` ban list; banned users get the
//!   `pmbot_4` string, unbanned the `pmbot_7` string.
//!
//! Contract limits, stated plainly:
//! * Forwarding a message to the owner, sending a message to an
//!   arbitrary user, fetching a user's entity / profile photo, and
//!   checking force-sub membership (`get_permissions`) all need
//!   Telegram calls for peers other than the current chat. `Ctx` /
//!   `Responder` target one chat only, so those operations are NOT
//!   attempted: the affected handlers record what they can in the DB
//!   and return an error naming the exact missing call, instead of
//!   pretending the message was delivered. The pure parts —
//!   force-sub parsing, ban-list state, `botchat` bookkeeping — are
//!   fully implemented and unit-testable through the DB.
//! * The incoming handler is exposed as `on_new_message` /
//!   `on_owner_reply` for the binary crate to call on incoming
//!   private messages (they are not commands, so they are not
//!   `Registry` entries); `/ban` and `/unban` are registered.

use ultroid_core::db::features::Features;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::{CommandDef, Ctx, Registry};

use crate::cbs::{def, fmtmsg, reg_add};

/// Parse both native and legacy stringified `PMBOT_FSUB` values
/// (Python `_force_sub_chats`): `None`/`False`/`""` → empty; a
/// string is tried as a Python literal (`[1, 2]` / `('a',)`), then
/// as comma-separated values; a single str/int becomes one element.
pub fn force_sub_chats(value: Option<&str>) -> Vec<String> {
    let Some(v) = value else {
        return Vec::new();
    };
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("false") || v == "None" {
        return Vec::new();
    }
    if let Some(inner) = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .or_else(|| v.strip_prefix('(').and_then(|s| s.strip_suffix(')')))
    {
        return inner
            .split(',')
            .map(|p| p.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
            .filter(|p| !p.is_empty())
            .collect();
    }
    v.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The `BOTBLS` ban list (Python `KeyManager("BOTBLS", cast=list)`),
/// stored as a JSON list under the `BOTBLS` key.
pub async fn ban_list(ctx: &Ctx) -> Vec<i64> {
    match ctx.db.get("BOTBLS").await {
        Ok(Some(v)) => serde_json::from_str::<Vec<i64>>(&v).unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn ban_list_set(ctx: &Ctx, list: &[i64]) -> Result<()> {
    ctx.db
        .set("BOTBLS", &serde_json::to_string(list).map_err(Error::from)?)
        .await
}

pub async fn is_banned(ctx: &Ctx, user_id: i64) -> bool {
    ban_list(ctx).await.contains(&user_id)
}

/// `botchat` mapping (Python `db.botchat`): forwarded-message id in
/// the owner's DM → original sender id.
pub async fn botchat_get(ctx: &Ctx, forwarded_id: i64) -> Option<i64> {
    let features = Features::new(ctx.db.clone());
    let raw = features
        .botchat
        .get(&forwarded_id.to_string())
        .await
        .ok()
        .flatten()?;
    raw.parse::<i64>().ok()
}

pub async fn botchat_add(ctx: &Ctx, forwarded_id: i64, who: i64) -> Result<()> {
    let features = Features::new(ctx.db.clone());
    features
        .botchat
        .set(&forwarded_id.to_string(), &who.to_string())
        .await
}

/// Incoming private message from a non-owner, non-banned user.
/// Force-sub is evaluated from the stored value; the forward to the
/// owner is the blocked step (see module docs).
pub async fn on_new_message(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private)
        || ctx.sender_id == ctx.config.owner_id
    {
        return Ok(());
    }
    if ctx.text.trim_start().starts_with('/') {
        return Ok(());
    }
    if is_banned(ctx, ctx.sender_id).await {
        return Ok(());
    }
    let fsub_raw = ctx.db.get("PMBOT_FSUB").await.ok().flatten();
    let chats = force_sub_chats(fsub_raw.as_deref());
    if !chats.is_empty() {
        // Membership (`get_permissions`) needs a Telegram call for the
        // sender in each chat; without it the requirement cannot be
        // checked, so the check is reported rather than skipped.
        return Err(Error::Other(format!(
            "pmbot force-subscribe check for chats {chats:?} needs get_permissions(entity, sender) — no cross-peer Telegram call exists in the Ctx contract"
        )));
    }
    Err(Error::Other(
        "pmbot forward to the owner needs event.forward_to(OWNER_ID) — no cross-peer send/forward exists in the Ctx contract; record the message with botchat_add() once the binary delivers it".to_string(),
    ))
}

/// Owner's reply in private to a forwarded message. `/who` answers
/// with the recorded user's info (entity lookup is the blocked
/// step); other `/` commands are ignored; anything else is relayed
/// to the recorded user (the blocked step).
pub async fn on_owner_reply(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private)
        || ctx.sender_id != ctx.config.owner_id
        || ctx.reply_to_id.is_none()
    {
        return Ok(());
    }
    let reply_to = ctx.reply_to_id.unwrap();
    let to_user = botchat_get(ctx, reply_to).await;
    let text = ctx.text.trim();
    if text.starts_with("/who") {
        let Some(user) = to_user else {
            ctx.reply("Could not identify the user for this message.")
                .await?;
            return Ok(());
        };
        return Err(Error::Other(format!(
            "pmbot /who for user {user} needs get_entity + download_profile_photo — no entity/profile-photo call exists in the Ctx contract"
        )));
    }
    if text.starts_with('/') {
        return Ok(());
    }
    let Some(user) = to_user else {
        return Ok(());
    };
    Err(Error::Other(format!(
        "pmbot relay to user {user} needs asst.send_message(to_user, message) — no cross-peer send exists in the Ctx contract"
    )))
}

/// `/ban` — owner-only, private, reply to a forwarded message.
pub async fn ban(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
        return Ok(());
    }
    let Some(reply_to) = ctx.reply_to_id else {
        ctx.reply(&fmtmsg(
            "Ban target not found",
            &[],
            &ctx.get_str("pmbot_2"),
            "",
        ))
        .await?;
        return Ok(());
    };
    let Some(target) = botchat_get(ctx, reply_to).await else {
        ctx.reply("Could not identify the user for this message.")
            .await?;
        return Ok(());
    };
    if is_banned(ctx, target).await {
        ctx.reply(&fmtmsg("Already Banned", &[], &ctx.get_str("pmbot_3"), ""))
            .await?;
        return Ok(());
    }
    let mut list = ban_list(ctx).await;
    list.push(target);
    ban_list_set(ctx, &list).await?;
    ctx.reply(&fmtmsg(
        "User Banned",
        &[(
            "User ID",
            format!("<code>{}</code>", escape_html(&target.to_string())),
        )],
        "",
        "",
    ))
    .await?;
    Err(Error::Other(format!(
        "banned user {target} must be told with asst.send_message(target, pmbot_4) — no cross-peer send exists in the Ctx contract"
    )))
}

/// `/unban` — owner-only, private, reply to a forwarded message.
pub async fn unban(ctx: &Ctx) -> Result<()> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
        return Ok(());
    }
    let Some(reply_to) = ctx.reply_to_id else {
        ctx.reply(&fmtmsg(
            "Unban target not found",
            &[],
            &ctx.get_str("pmbot_5"),
            "",
        ))
        .await?;
        return Ok(());
    };
    let Some(target) = botchat_get(ctx, reply_to).await else {
        ctx.reply("Could not identify the user for this message.")
            .await?;
        return Ok(());
    };
    if !is_banned(ctx, target).await {
        ctx.reply(&fmtmsg("Not Banned", &[], &ctx.get_str("pmbot_6"), ""))
            .await?;
        return Ok(());
    }
    let mut list = ban_list(ctx).await;
    list.retain(|u| *u != target);
    ban_list_set(ctx, &list).await?;
    ctx.reply(&fmtmsg(
        "User Unbanned",
        &[(
            "User ID",
            format!("<code>{}</code>", escape_html(&target.to_string())),
        )],
        "",
        "",
    ))
    .await?;
    Err(Error::Other(format!(
        "unbanned user {target} must be told with asst.send_message(target, pmbot_7) — no cross-peer send exists in the Ctx contract"
    )))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def(
                "ban",
                "ban",
                "Ban a PM bot user (reply to their forwarded message).",
            )
        },
        |ctx| Box::pin(ban(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def(
                "unban",
                "unban",
                "Unban a PM bot user (reply to their forwarded message).",
            )
        },
        |ctx| Box::pin(unban(ctx)),
    );
}
