//! Port of `plugins/boosts.py` — Telegram Premium channel boosts.
//!
//! `.boosts [channel]` — boost status card (level, count, next level,
//! your slots). `.boost <channel> [slot]` — apply one of your Premium
//! boost slots (full-sudo). Premium required.
//!
//! Argument parsing and the slot-picking rules (explicit slot, else
//! first free unused non-cooldown slot) are real. The status and apply
//! calls are wired to the `Responder` boost ops (`boost_status`,
//! `apply_boost`) with a Premium probe via `me()`. Two gaps remain:
//! `premium.GetMyBoostsRequest` (own slots) has no trait op, so the
//! status card cannot show your slots and an explicit `[slot]` cannot
//! be honoured — the apply call lets the server pick — and channel
//! usernames cannot be resolved (only numeric ids; `resolve_user` is
//! users-only), so they still return an honest error.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub const PREMIUM_REQUIRED: &str =
    "⭐ **Telegram Premium is required to boost a channel.**\n`This account isn't Premium.`";

pub fn register(registry: &mut Registry) {
    let mut status = def(
        "boosts",
        "boosts( (.*)|$)",
        "Boost status card: `.boosts [channel]`. Premium required.",
    );
    status.fullsudo = true;
    reg_add(registry, status, |ctx| Box::pin(boosts_status(ctx)));

    let mut apply = def(
        "boost",
        "boost( (.*)|$)",
        "Apply a boost slot: `.boost <channel> [slot]`. Premium required.",
    );
    apply.fullsudo = true;
    reg_add(registry, apply, |ctx| Box::pin(boost_apply(ctx)));
}

fn boosts_op(op: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: boosts.py)"
    ))
}

/// Resolve a channel token to a numeric id; usernames need `get_entity`.
fn resolve_channel(token: &str) -> std::result::Result<i64, Error> {
    token
        .trim()
        .trim_start_matches('@')
        .parse::<i64>()
        .map_err(|_| boosts_op(&format!("resolve channel '{token}' to an entity")))
}

async fn boosts_status(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let target: i64 = if raw.is_empty() {
        ctx.chat_id
    } else {
        resolve_channel(raw)?
    };
    let id = ctx.reply("`Fetching boost status...`").await?;
    let me = ctx.responder.me().await?;
    if !me.premium {
        ctx.edit(id, PREMIUM_REQUIRED).await?;
        return Ok(());
    }
    // premium.GetBoostsStatusRequest(peer) (Python: boosts.py:_boosts_status).
    // premium.GetMyBoostsRequest (own slots) has no trait op, so the
    // card shows level / count / next level only.
    let status = ctx.responder.boost_status(target).await?;
    let mut lines = vec![
        format!("**Boost status** for `{target}`:"),
        format!("Level: **{}**", status.level),
        format!("Boosts: **{}**", status.boosts),
    ];
    if let Some(gift) = status.gift_boosts {
        lines.push(format!("Boosts from you: **{gift}**"));
    }
    match status.next_level_boosts {
        Some(next) => lines.push(format!("Next level at: **{next}** boosts")),
        None => lines.push("Next level: **max level reached**".to_string()),
    }
    ctx.edit(id, &lines.join("\n")).await
}

async fn boost_apply(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.is_empty() {
        ctx.reply("`Usage: .boost <channel> [slot]`").await?;
        return Ok(());
    }
    let channel_token = tokens[0];
    let slot_token = tokens.get(1).copied();
    if let Some(slot) = slot_token {
        if !slot.chars().all(|c| c.is_ascii_digit()) {
            ctx.reply("`[slot] must be a number.`").await?;
            return Ok(());
        }
    }
    let target = resolve_channel(channel_token)?;
    let me = ctx.responder.me().await?;
    if !me.premium {
        ctx.reply(PREMIUM_REQUIRED).await?;
        return Ok(());
    }
    if slot_token.is_some() {
        // An explicit slot needs premium.GetMyBoostsRequest to validate
        // and pick it, which has no trait op; `apply_boost` lets the
        // server pick a slot.
        return Err(boosts_op(
            "premium.GetMyBoostsRequest (explicit boost slot selection)",
        ));
    }
    // premium.ApplyBoostRequest(peer) (Python: boosts.py:_boost_apply).
    ctx.responder.apply_boost(target).await?;
    ctx.reply(&format!("`Boosted {channel_token}.`")).await?;
    Ok(())
}
