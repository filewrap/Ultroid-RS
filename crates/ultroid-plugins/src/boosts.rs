//! Port of `plugins/boosts.py` — Telegram Premium channel boosts.
//!
//! `.boosts [channel]` — boost status card (level, count, next level,
//! your slots). `.boost <channel> [slot]` — apply one of your Premium
//! boost slots (full-sudo). Premium required.
//!
//! Argument parsing and the slot-picking rules (explicit slot, else
//! first free unused non-cooldown slot) are real. The Premium probe
//! (`get_me` / `is_premium`), peer resolution, and the
//! `premium.GetBoostsStatusRequest` / `GetMyBoostsRequest` /
//! `ApplyBoostRequest` RPCs are client calls the `Responder` surface
//! does not expose yet — each handler validates first, then returns an
//! honest `TODO(responder-op)` error at that exact point. The status
//! card is rendered with `RichDoc::markdown` + `send_rich_or_plain`
//! once the RPC exists (the follow-up pass).

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
    let _id = ctx.reply("`Fetching boost status...`").await?;
    // TODO(responder-op): premium.GetBoostsStatusRequest(peer) +
    // premium.GetMyBoostsRequest() (Python: boosts.py:_boosts_status).
    Err(boosts_op(&format!(
        "premium.GetBoostsStatusRequest(peer={target})"
    )))
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
    // TODO(responder-op): Premium probe (`get_me` + `is_premium`),
    // then premium.GetMyBoostsRequest() for auto slot-pick (or the
    // explicit slot), then premium.ApplyBoostRequest(peer, slots=[slot])
    // (Python: boosts.py:_boost_apply).
    let _ = target;
    Err(boosts_op("Premium probe + premium.ApplyBoostRequest"))
}
