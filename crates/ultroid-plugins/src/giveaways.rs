//! Port of `plugins/giveaways.py` — boost giveaways + gift codes.
//!
//! `.giveawayinfo [t.me link]` (or reply), `.giftcode <slug>`,
//! `.giftcoderedeem <slug> [confirm]`, `.premiumopts [boost_channel]`,
//! `.giveaway <channel> <winners> stars=<N> [days=<D>]` (all owner-only).
//!
//! Argument parsing, the `t.me` link resolver, the real-currency
//! guidance text, and the Stars-giveaway confirm gate (nonce re-run)
//! are real. Every `payments.*` RPC (giveaway info, gift-code check /
//! redeem, premium options, Stars-giveaway options, the
//! GetPaymentForm → SendStarsForm launch) is a client call the
//! `Responder` surface does not expose yet — each handler validates
//! first, then returns an honest `TODO(responder-op)` error at that
//! exact point.
//!
//! The real-currency path (`.giveaway <channel> <winners> <months>`)
//! is deliberately guidance-only, exactly as in the Python version:
//! there is no MTProto request that originates an App Store / Play
//! Store Premium purchase, so nothing is faked.

use crate::util::{def, reg_add};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::Mutex;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    let owner = |name: &'static str, pattern: &'static str, help: &'static str| {
        let mut d = def(name, pattern, help);
        d.owner_only = true;
        d
    };
    reg_add(
        registry,
        owner(
            "giveawayinfo",
            "giveawayinfo( (.*)|$)",
            "Giveaway status: `.giveawayinfo [t.me link]` (or reply to a giveaway message).",
        ),
        |ctx| Box::pin(giveawayinfo(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giftcode",
            "giftcode( (.*)|$)",
            "Check a gift/Premium code without redeeming it: `.giftcode <slug>`.",
        ),
        |ctx| Box::pin(giftcode_check(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giftcoderedeem",
            "giftcoderedeem( (.*)|$)",
            "Redeem a gift code (confirm-gated, irreversible): `.giftcoderedeem <slug> [confirm]`.",
        ),
        |ctx| Box::pin(giftcode_redeem(ctx)),
    );
    reg_add(
        registry,
        owner(
            "premiumopts",
            "premiumopts( (.*)|$)",
            "Premium gift-code price tiers: `.premiumopts [boost_channel]`.",
        ),
        |ctx| Box::pin(premiumopts(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giveaway",
            "giveaway( (.*)|$)",
            "Stars-funded boost giveaway: `.giveaway <channel> <winners> stars=<N> [days=<D>]` (confirm-gated).",
        ),
        |ctx| Box::pin(giveaway(ctx)),
    );
}

struct Pending {
    kind: &'static str,
    data: Value,
    by: i64,
    at: Instant,
}

static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();

fn pending_store() -> &'static Mutex<HashMap<String, Pending>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn stash(by: i64, kind: &'static str, data: Value) -> String {
    let nonce = format!("{:x}", rand::random::<u64>());
    pending_store().lock().await.insert(
        nonce.clone(),
        Pending {
            kind,
            data,
            by,
            at: Instant::now(),
        },
    );
    nonce
}

async fn pop(nonce: &str, by: i64, kind: &str) -> Option<Value> {
    let mut m = pending_store().lock().await;
    let p = m.remove(nonce)?;
    if p.by != by || p.kind != kind || p.at.elapsed().as_secs() > 300 {
        return None;
    }
    Some(p.data)
}

fn payments_op(op: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: giveaways.py)"
    ))
}

fn tme_regex() -> Regex {
    Regex::new(r"(?:https?://)?t\.me/(c/)?([\w\d_]+)/(\d+)").expect("static regex")
}

/// Resolve `(peer, msg_id)` from a `t.me` link, or the replied message
/// (Python `giveaways.py:_resolve_peer_msg`).
fn resolve_peer_msg(ctx: &Ctx, arg: &str) -> std::result::Result<(String, i64), String> {
    if let Some(caps) = tme_regex().captures(arg) {
        let is_internal = caps.get(1).is_some();
        let ident = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let msg_id: i64 = caps
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let peer = if is_internal {
            format!("-100{ident}")
        } else {
            ident.to_string()
        };
        return Ok((peer, msg_id));
    }
    if let Some(reply_id) = ctx.reply_to_id {
        return Ok((ctx.chat_id.to_string(), reply_id));
    }
    Err("Give a t.me link (`t.me/channel/123`) or reply to the giveaway message.".to_string())
}

async fn giveawayinfo(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let (peer, msg_id) = match resolve_peer_msg(ctx, arg) {
        Ok(v) => v,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    let _ = (peer, msg_id);
    // TODO(responder-op): payments.GetGiveawayInfoRequest(peer, msg_id)
    // (Python: giveaways.py:giveawayinfo_cmd).
    Err(payments_op(
        "payments.GetGiveawayInfoRequest (giveaway info)",
    ))
}

async fn giftcode_check(ctx: &Ctx) -> Result<()> {
    let slug = ctx.args.split_whitespace().next().unwrap_or("").to_string();
    if slug.is_empty() {
        ctx.reply("`Usage: .giftcode <slug>`").await?;
        return Ok(());
    }
    // TODO(responder-op): payments.CheckGiftCodeRequest(slug)
    // (Python: giveaways.py:giftcode_check_cmd).
    Err(payments_op(&format!(
        "payments.CheckGiftCodeRequest(slug={slug})"
    )))
}

async fn giftcode_redeem(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    let mut tokens = tokens;
    let confirmed = !tokens.is_empty()
        && tokens.last().map(|s| s.to_lowercase()) == Some("confirm".to_string());
    if confirmed {
        tokens.pop();
    }
    if tokens.is_empty() {
        ctx.reply("`Usage: .giftcoderedeem <slug> [confirm]`")
            .await?;
        return Ok(());
    }
    let slug = tokens[0].clone();
    if !confirmed {
        ctx.reply(&format!(
            "**Redeem gift code** `{slug}` **onto this account?**\n_This is irreversible._ Re-run: `.giftcoderedeem {slug} confirm`"
        ))
        .await?;
        return Ok(());
    }
    // TODO(responder-op): payments.ApplyGiftCodeRequest(slug)
    // (Python: giveaways.py:giftcode_redeem_cmd).
    Err(payments_op(&format!(
        "payments.ApplyGiftCodeRequest(slug={slug})"
    )))
}

async fn premiumopts(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_string();
    let _ = arg;
    // TODO(responder-op): payments.GetPremiumGiftCodeOptionsRequest
    // (Python: giveaways.py:premiumopts_cmd).
    Err(payments_op(
        "payments.GetPremiumGiftCodeOptionsRequest (Premium options)",
    ))
}

fn months_guidance(months: &str) -> String {
    format!(
        "`Real-currency Premium giveaways ({months} month(s)) need a Play Store/App Store billing payment method already on file with Telegram — this bot has no MTProto path to originate that purchase for you.`\n\n**What is automatable here:**\n• `.giveaway <channel> <winners> stars=<N>` — a **Stars-funded** giveaway, paid straight from your Stars balance.\n• `.premiumopts` — the real-currency pricing table, if you'd rather buy a Premium giveaway slot yourself in the Telegram app.\n• If you already hold a **prepaid** giveaway slot (bought through an app store), its `giveaway_id` can be launched with `payments.LaunchPrepaidGiveawayRequest` — not wired up here since nothing at this fork layer can hand this bot that id on its own."
    )
}

const GIVEAWAY_USAGE: &str = "`Usage: .giveaway <channel> <winners> stars=<N> [days=<D>]`\nor `.giveaway <channel> <winners> <months>` for guidance on the real-currency path (not automatable here).";

async fn giveaway(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    let mut tokens = tokens;
    let confirmed = !tokens.is_empty()
        && tokens.last().map(|s| s.to_lowercase()) == Some("confirm".to_string());
    if confirmed {
        tokens.pop();
    }

    // Shorthand re-run: `.giveaway <nonce>` confirms a previewed launch.
    if !confirmed && tokens.len() == 1 {
        if let Some(data) = pop(&tokens[0], ctx.sender_id, "giveaway").await {
            return do_launch_giveaway(
                ctx,
                &data["channel"].as_str().unwrap_or("").to_string(),
                data["stars"].as_i64().unwrap_or(0),
                data["winners"].as_i64().unwrap_or(0),
                data["days"].as_i64().unwrap_or(3),
            )
            .await;
        }
    }

    if tokens.len() < 3 {
        ctx.reply(GIVEAWAY_USAGE).await?;
        return Ok(());
    }
    let channel_tok = tokens[0].clone();
    let winners: i64 = match tokens[1].parse() {
        Ok(n) => n,
        Err(_) => {
            ctx.reply("`<winners> must be a whole number.`").await?;
            return Ok(());
        }
    };
    let plan_tok = tokens[2].to_lowercase();
    let mut days: i64 = 3;
    for tok in &tokens[3..] {
        if let Some(v) = tok.to_lowercase().strip_prefix("days=") {
            if let Ok(n) = v.parse::<i64>() {
                days = n.max(1);
            }
        }
    }
    if !plan_tok.starts_with("stars=") {
        ctx.reply(&months_guidance(&tokens[2])).await?;
        return Ok(());
    }
    let target_stars: i64 = match plan_tok.split('=').nth(1).and_then(|v| v.parse().ok()) {
        Some(n) => n,
        None => {
            ctx.reply("`stars=<N> must be a whole number.`").await?;
            return Ok(());
        }
    };

    if !confirmed {
        let nonce = stash(
            ctx.sender_id,
            "giveaway",
            json!({"channel": channel_tok, "stars": target_stars, "winners": winners, "days": days}),
        )
        .await;
        let preview = format!(
            "**Launch a Stars giveaway on** `{channel_tok}`**?**\n• Winners: **{winners}**\n• Stars pool: **{target_stars} ⭐**\n• Ends in: `{days}` day(s)\n\n_This spends real Telegram Stars from your balance._ Nothing is paid yet — re-run this exact command with `confirm` appended, or just run `.giveaway {nonce}`."
        );
        ctx.reply(&preview).await?;
        return Ok(());
    }
    do_launch_giveaway(ctx, &channel_tok, target_stars, winners, days).await
}

async fn do_launch_giveaway(
    ctx: &Ctx,
    channel: &str,
    stars: i64,
    winners: i64,
    days: i64,
) -> Result<()> {
    let _ = ctx;
    let _ = (stars, winners, days);
    // TODO(responder-op): payments.GetStarsGiveawayOptionsRequest to pick
    // the tier, then the GetPaymentFormRequest → SendStarsFormRequest
    // two-step with an InputInvoiceStars / InputStorePaymentStarsGiveaway
    // purpose (Python: giveaways.py:_do_launch_giveaway). Any payment
    // verification URL goes to Saved Messages only, never the chat.
    Err(payments_op(&format!("launch Stars giveaway on {channel}")))
}
