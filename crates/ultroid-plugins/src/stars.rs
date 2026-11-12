//! Port of `plugins/stars.py` — Telegram Stars wallet, revenue, and
//! paid messages.
//!
//! `.stars [peer]`, `.starstopup`, `.starsrev [peer]`,
//! `.starswithdraw`, `.pdon <stars> [text|reply]`, `.pdoff`,
//! `.pdrefund <charge_id>` (reply) / `.pdrefund <user> <charge_id>`
//! (all owner-only).
//!
//! `.starswithdraw` is instructions-only, exactly as in the Python
//! version — it never touches the withdrawal RPC, never asks for a 2FA
//! password, and never prints a withdrawal URL. The `.pdon <stars>
//! <text>` paid-send variant is confirm-gated (nonce re-run) like
//! `gifts.py`. All argument parsing, previews, and the confirm gate are
//! real. The `payments.*` / `account.*` RPCs (wallet, transactions,
//! top-up options, revenue stats, global privacy settings, per-user
//! exceptions, paid send, refund) are client calls the `Responder`
//! surface does not expose yet — each handler validates first, then
//! returns an honest `TODO(responder-op)` error at that exact point.

use crate::util::{def, reg_add};
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
            "stars",
            "stars(?:\\s+(\\S+))?$",
            "Stars wallet dashboard: `.stars [peer]`.",
        ),
        |ctx| Box::pin(stars_wallet(ctx)),
    );
    reg_add(
        registry,
        owner(
            "starstopup",
            "starstopup$",
            "Stars top-up options (informational only): `.starstopup`.",
        ),
        |ctx| Box::pin(stars_topup(ctx)),
    );
    reg_add(
        registry,
        owner(
            "starsrev",
            "starsrev(?:\\s+(\\S+))?$",
            "Stars revenue stats: `.starsrev [peer]`.",
        ),
        |ctx| Box::pin(stars_revenue(ctx)),
    );
    reg_add(
        registry,
        owner(
            "starswithdraw",
            "starswithdraw$",
            "Stars withdrawal instructions (never automated): `.starswithdraw`.",
        ),
        |ctx| Box::pin(stars_withdraw(ctx)),
    );
    reg_add(
        registry,
        owner(
            "pdon",
            "pdon(?:\\s+(\\S+))?(?:\\s+([\\s\\S]*))?$",
            "Pay-to-message: `.pdon <stars>` (global), `.pdon <stars>` (reply: per-user), `.pdon <stars> <text>` (send a paid message, confirm-gated).",
        ),
        |ctx| Box::pin(pd_on(ctx)),
    );
    reg_add(
        registry,
        owner(
            "pdoff",
            "pdoff$",
            "Disable pay-to-message (global, or per-user on reply): `.pdoff`.",
        ),
        |ctx| Box::pin(pd_off(ctx)),
    );
    reg_add(
        registry,
        owner(
            "pdrefund",
            "pdrefund(?:\\s+(\\S+))?(?:\\s+(\\S+))?$",
            "Refund a Stars charge: `.pdrefund <charge_id>` (reply) or `.pdrefund <user> <charge_id>`.",
        ),
        |ctx| Box::pin(pd_refund(ctx)),
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

fn stars_op(op: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: stars.py)"
    ))
}

/// Turn a raw `[peer]` token into something `get_input_entity` likes
/// (Python `stars.py:_parse_peer`).
fn parse_peer(arg: &str) -> String {
    let arg = arg.trim().trim_start_matches('@');
    if arg.is_empty() {
        return "me".to_string();
    }
    arg.to_string()
}

async fn stars_wallet(ctx: &Ctx) -> Result<()> {
    let peer = parse_peer(ctx.captures.get(1).map(|s| s.as_str()).unwrap_or(""));
    let _id = ctx.reply("`Fetching Stars wallet...`").await?;
    // TODO(responder-op): payments.GetStarsStatusRequest +
    // payments.GetStarsTransactionsRequest (Python: stars.py:stars_wallet_cmd,
    // via `pyUltroid._core.stars.wallet` / `.transactions`).
    Err(stars_op(&format!("fetch Stars wallet for {peer}")))
}

async fn stars_topup(ctx: &Ctx) -> Result<()> {
    let _id = ctx.reply("`Fetching top-up options...`").await?;
    // TODO(responder-op): payments.GetStarsTopupOptionsRequest
    // (Python: stars.py:stars_topup_cmd, via `pyUltroid._core.stars.topup_options`).
    // Buying Stars needs Telegram's own in-app purchase flow; this
    // command is informational only.
    Err(stars_op("fetch Stars top-up options"))
}

async fn stars_revenue(ctx: &Ctx) -> Result<()> {
    let peer = if let Some(arg) = ctx.captures.get(1).filter(|s| !s.is_empty()) {
        parse_peer(arg)
    } else if ctx.reply_to_id.is_some() {
        // The replied message's sender id is not visible on `Ctx`.
        // TODO(responder-op): resolve the replied message's sender
        // (Python: stars.py:stars_revenue_cmd, `reply.sender_id`).
        return Err(stars_op("resolve replied message sender for .starsrev"));
    } else {
        "me".to_string()
    };
    let _id = ctx.reply("`Fetching Stars revenue stats...`").await?;
    // TODO(responder-op): payments.GetStarsRevenueStatsRequest
    // (Python: stars.py:stars_revenue_cmd, via
    // `pyUltroid._core.stars.revenue_stats`).
    Err(stars_op(&format!("fetch Stars revenue stats for {peer}")))
}

const WITHDRAW_TEXT: &str = "**⭐ Stars Withdrawal**\n\nThis command does **not** automate withdrawals — real money and your 2FA password are involved, and this bot will never ask for either in a chat message.\n\nTo withdraw Stars revenue, use the **official Telegram app**:\n`Settings → Telegram Stars → your channel/bot → Withdraw`\n\nCheck current eligibility/balance with `.starsrev`.";

async fn stars_withdraw(ctx: &Ctx) -> Result<()> {
    // Instructions only — never the withdrawal RPC, never a 2FA prompt,
    // never a withdrawal URL anywhere (Python: stars.py:stars_withdraw_cmd).
    if ctx.chat_type == ultroid_core::ChatType::Private {
        ctx.reply(WITHDRAW_TEXT).await?;
        return Ok(());
    }
    // TODO(responder-op): send the instructions to Saved Messages
    // ("me") and just point at them here — never place
    // withdrawal-adjacent replies in a group
    // (Python: stars.py:stars_withdraw_cmd).
    Err(stars_op("send withdrawal instructions to Saved Messages"))
}

async fn pd_on(ctx: &Ctx) -> Result<()> {
    // captures[1] = stars token, captures[2] = trailing text (or empty).
    let stars_raw = ctx.captures.get(1).cloned().unwrap_or_default();
    let text_raw = ctx.captures.get(2).cloned().unwrap_or_default();
    if stars_raw.trim().is_empty() {
        ctx.reply("`Usage: .pdon <stars>` (global) · `.pdon <stars>` as a reply (per-user) · `.pdon <stars> <text>` (send a paid message).").await?;
        return Ok(());
    }
    // Shorthand re-run: `.pdon <nonce>` alone confirms a previewed
    // paid-message send.
    if text_raw.trim().is_empty() {
        if let Some(data) = pop(stars_raw.trim(), ctx.sender_id, "pdon_send").await {
            return finish_pdon_send(
                ctx,
                data["stars"].as_i64().unwrap_or(0),
                &data["text"].as_str().unwrap_or("").to_string(),
            )
            .await;
        }
    }
    let Ok(stars) = stars_raw.trim().parse::<i64>() else {
        ctx.reply("`<stars> must be a non-negative whole number.`")
            .await?;
        return Ok(());
    };
    if stars < 0 {
        ctx.reply("`<stars> must be a non-negative whole number.`")
            .await?;
        return Ok(());
    }
    let mut tokens: Vec<String> = text_raw.split_whitespace().map(str::to_string).collect();
    let confirmed = !tokens.is_empty()
        && tokens.last().map(|s| s.to_lowercase()) == Some("confirm".to_string());
    if confirmed {
        tokens.pop();
    }
    let text = tokens.join(" ");
    if !text.is_empty() {
        // Real-money branch — spends this account's own Stars. Gated.
        if !confirmed {
            let nonce = stash(
                ctx.sender_id,
                "pdon_send",
                json!({"stars": stars, "text": text}),
            )
            .await;
            let preview = format!(
                "**Send a paid message to chat `{}`?**\nMessage: _{text}_\nMax charge: up to **{stars}** ⭐\n\n_This spends real Telegram Stars._ Nothing is paid yet — re-run this exact command with `confirm` appended, or just run `.pdon {nonce}`.",
                ctx.chat_id
            );
            ctx.reply(&preview).await?;
            return Ok(());
        }
        return finish_pdon_send(ctx, stars, &text).await;
    }
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): resolve the replied message's sender, then
        // account.ToggleNoPaidMessagesExceptionRequest(user, require_payment=true)
        // (Python: stars.py:pd_on_cmd).
        return Err(stars_op(
            "resolve replied sender + ToggleNoPaidMessagesExceptionRequest(require_payment=true)",
        ));
    }
    // TODO(responder-op): read-modify-write
    // account.GetGlobalPrivacySettingsRequest +
    // SetGlobalPrivacySettingsRequest(noncontact_peers_paid_stars=stars)
    // (Python: stars.py:pd_on_cmd, `_set_noncontact_price`).
    Err(stars_op(&format!(
        "set noncontact_peers_paid_stars={stars}"
    )))
}

async fn finish_pdon_send(ctx: &Ctx, stars: i64, text: &str) -> Result<()> {
    let _ = (ctx, stars, text);
    // TODO(responder-op): messages.SendMessageRequest with
    // allow_paid_stars=stars (Python: stars.py:_send_allow_paid_stars).
    Err(stars_op(&format!(
        "send paid message (allow_paid_stars={stars})"
    )))
}

async fn pd_off(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): resolve the replied message's sender, then
        // account.ToggleNoPaidMessagesExceptionRequest(user, require_payment=false)
        // (Python: stars.py:pd_off_cmd).
        return Err(stars_op(
            "resolve replied sender + ToggleNoPaidMessagesExceptionRequest(require_payment=false)",
        ));
    }
    // TODO(responder-op): SetGlobalPrivacySettingsRequest with
    // noncontact_peers_paid_stars=0 (Python: stars.py:pd_off_cmd).
    Err(stars_op("set noncontact_peers_paid_stars=0"))
}

async fn pd_refund(ctx: &Ctx) -> Result<()> {
    let first = ctx.captures.get(1).cloned().unwrap_or_default();
    let second = ctx.captures.get(2).cloned().unwrap_or_default();
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): resolve the replied message's sender
        // (the payer), then payments.RefundStarsChargeRequest(user,
        // charge_id=first) (Python: stars.py:pd_refund_cmd).
        return Err(stars_op(
            "resolve payer from reply + payments.RefundStarsChargeRequest",
        ));
    }
    if first.trim().is_empty() || second.trim().is_empty() {
        ctx.reply("`Usage: .pdrefund <charge_id>` (reply to the payer) or `.pdrefund <user> <charge_id>`.").await?;
        return Ok(());
    }
    let user = parse_peer(&first);
    let charge_id = second.trim().to_string();
    // TODO(responder-op): payments.RefundStarsChargeRequest(user,
    // charge_id) (Python: stars.py:pd_refund_cmd).
    Err(stars_op(&format!(
        "payments.RefundStarsChargeRequest(user={user}, charge_id={charge_id})"
    )))
}
