//! Auto-kick ("do not disturb") — port of `plugins/autoban.py`.
//!
//! DB key (raw udB key, same as Python `KeyManager("DND_CHATS")`):
//! `DND_CHATS` is a list of chat ids in do-not-disturb mode.
//!
//! `.autokick on|off` is fully ported. The join handler (`dnd_func`,
//! a `ChatAction` listener that kicks every new joiner) is event-driven:
//! its decision predicate and kick loop are ported as callable helpers
//! for the binary's update loop, with the kick RPC as a
//! `TODO(responder-op)` site.

use crate::util::{def, reg_add};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "\n\n- `{i}autokick <on/off>`\n    on - To enable.\n    off - To disable.\n    Automatically kick new joined users from the group.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "autokick",
            pattern: "autokick (on|off)$",
            help: HELP,
            admins_only: true,
            fullsudo: true,
            ..def("autokick", "autokick (on|off)$", HELP)
        },
        |ctx| Box::pin(autokick(ctx)),
    );
}

/// Chats in do-not-disturb mode (Python `KeyManager("DND_CHATS")`).
pub async fn dnd_chats(ctx: &Ctx) -> Result<Vec<i64>> {
    let v: Option<Vec<i64>> = db_get_json(&*ctx.db, "DND_CHATS").await?;
    Ok(v.unwrap_or_default())
}

async fn set_dnd_chats(ctx: &Ctx, chats: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "DND_CHATS", &chats.to_vec()).await
}

pub async fn is_dnd(ctx: &Ctx, chat_id: i64) -> Result<bool> {
    Ok(dnd_chats(ctx).await?.contains(&chat_id))
}

async fn autokick(ctx: &Ctx) -> Result<()> {
    // Pattern `autokick (on|off)$` — group 1 is the toggle.
    let mode = ctx.captures.get(1).map(|s| s.as_str()).unwrap_or("");
    let mut chats = dnd_chats(ctx).await?;
    match mode {
        "on" => {
            if chats.contains(&ctx.chat_id) {
                ctx.reply("`Chat already in do not disturb mode.`").await?;
                return Ok(());
            }
            chats.push(ctx.chat_id);
            set_dnd_chats(ctx, &chats).await?;
            ctx.reply("`Do not disturb mode activated for this chat.`")
                .await?;
        }
        "off" => {
            if !chats.contains(&ctx.chat_id) {
                ctx.reply("`Chat is not in do not disturb mode.`").await?;
                return Ok(());
            }
            chats.retain(|c| *c != ctx.chat_id);
            set_dnd_chats(ctx, &chats).await?;
            ctx.reply("`Do not disturb mode deactivated for this chat.`")
                .await?;
        }
        _ => {}
    }
    Ok(())
}

/// Should a join event in this chat trigger DND kicks?
/// (Python `join_func`: the update is a user join AND the chat is in
/// `DND_CHATS`.)
pub fn dnd_applies(user_joined: bool, dnd_chats: &[i64], chat_id: i64) -> bool {
    user_joined && dnd_chats.contains(&chat_id)
}

/// Kick every newly-joined user in a DND chat (Python `dnd_func`).
/// The kick RPC + deleting the join service message are client ops.
pub async fn dnd_kick_joins(_ctx: &Ctx, _chat_id: i64, _user_ids: &[i64]) -> Result<()> {
    // TODO(responder-op): kick_participant(chat, user) per joiner, then
    // delete the service message. Python: plugins/autoban.py:dnd_func.
    Err(Error::Other(
        "kick participant requires a binary Responder op (Python: plugins/autoban.py:dnd_func)"
            .into(),
    ))
}
