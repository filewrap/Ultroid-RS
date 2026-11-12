//! Port of `plugins/gifts.py` — Telegram Star Gifts.
//!
//! `.gifts [page]` (catalog), `.gift <user> <gift_id> [-anon] [message]`,
//! `.giftsaved [user]`, `.giftupgrade <ref> [confirm]`,
//! `.gifttransfer <ref> <user> [confirm]`, `.giftsave <ref> [unsave]`
//! (all owner-only).
//!
//! Real-money safety, as in the Python version: every command that can
//! spend Stars goes through a text-based confirmation step — re-run the
//! exact command with a trailing `confirm`, or re-run with just the short
//! `<nonce>` the preview gives you. Pending confirmations live in an
//! in-memory map (5-minute TTL, owner-only). All argument parsing,
//! previews, and the confirm gate are real. The `payments.*` RPCs
//! themselves (catalog, send, upgrade, transfer, save, saved list) are
//! client calls the `Responder` surface does not expose yet — each
//! handler validates first, then returns an honest `TODO(responder-op)`
//! error at that exact point. The `ultroid_core::stars::StarsStore`
//! only tracks a cached balance; it cannot drive these RPCs.

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
            "gifts",
            "gifts( (.*)|$)",
            "Browse the Star Gift catalog: `.gifts [page]`.",
        ),
        |ctx| Box::pin(gifts_catalog(ctx)),
    );
    reg_add(
        registry,
        owner(
            "gift",
            "gift( (.*)|$)",
            "Buy and send a star gift: `.gift <user> <gift_id> [-anon] [message]` (confirm-gated).",
        ),
        |ctx| Box::pin(gift_send(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giftupgrade",
            "giftupgrade( (.*)|$)",
            "Upgrade a saved gift to its unique form: `.giftupgrade <ref> [confirm]`.",
        ),
        |ctx| Box::pin(gift_upgrade(ctx)),
    );
    reg_add(
        registry,
        owner(
            "gifttransfer",
            "gifttransfer( (.*)|$)",
            "Transfer a saved gift: `.gifttransfer <ref> <user> [confirm]`.",
        ),
        |ctx| Box::pin(gift_transfer(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giftsave",
            "giftsave( (.*)|$)",
            "Show/hide a saved gift on your profile: `.giftsave <ref> [unsave]`.",
        ),
        |ctx| Box::pin(gift_save(ctx)),
    );
    reg_add(
        registry,
        owner(
            "giftsaved",
            "giftsaved( (.*)|$)",
            "List saved star gifts: `.giftsaved [user]`.",
        ),
        |ctx| Box::pin(gift_saved(ctx)),
    );
}

/// In-memory pending confirmations, mirroring
/// `pyUltroid._core.buttonkit.PendingConfirmations` (TTL 300s).
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
        "{op} requires a binary Responder op (Python: gifts.py)"
    ))
}

/// A saved-gift ref: a message id (`int`) or a slug (`str`).
fn parse_gift_ref(token: &str) -> String {
    token.trim().to_string()
}

fn split_confirmed(tokens: Vec<String>) -> (Vec<String>, bool) {
    let mut t = tokens;
    let confirmed =
        !t.is_empty() && t.last().map(|s| s.to_lowercase()) == Some("confirm".to_string());
    if confirmed {
        t.pop();
    }
    (t, confirmed)
}

async fn gifts_catalog(ctx: &Ctx) -> Result<()> {
    let _page: usize = ctx
        .args
        .trim()
        .parse()
        .ok()
        .filter(|p| *p >= 1)
        .unwrap_or(1);
    // TODO(responder-op): payments.GetStarGiftsRequest(hash=0)
    // (Python: gifts.py:gifts_catalog_cmd).
    Err(payments_op("payments.GetStarGiftsRequest (gift catalog)"))
}

async fn gift_send(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    let (tokens, confirmed) = split_confirmed(tokens);

    // Shorthand re-run: `.gift <nonce>` confirms a previewed send.
    if !confirmed && tokens.len() == 1 {
        if let Some(data) = pop(&tokens[0], ctx.sender_id, "send").await {
            return do_send_gift(
                ctx,
                &data["target"].as_str().unwrap_or("").to_string(),
                data["gift_id"].as_i64().unwrap_or(0),
                data["message"].as_str().map(str::to_string),
                data["hidden"].as_bool().unwrap_or(false),
                &data["who_name"].as_str().unwrap_or("").to_string(),
            )
            .await;
        }
    }

    let hidden = tokens.iter().any(|t| {
        matches!(
            t.to_lowercase().as_str(),
            "-anon" | "--anon" | "-hidden" | "--hidden"
        )
    });
    let tokens: Vec<String> = tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.to_lowercase().as_str(),
                "-anon" | "--anon" | "-hidden" | "--hidden"
            )
        })
        .collect();

    let (target, gift_tok, message_text): (String, String, Option<String>) = if ctx
        .reply_to_id
        .is_some()
    {
        if tokens.is_empty() {
            ctx.reply("`Usage: .gift <user> <gift_id> [-anon] [message...]`\n(or reply to the user's message and just give the `<gift_id>`)\nSpends real Telegram Stars — a confirmation step runs first.").await?;
            return Ok(());
        }
        // The replied message's sender id is not visible on `Ctx`.
        // TODO(responder-op): resolve the replied message's sender
        // (Python: gifts.py:gift_send_cmd, `reply.sender_id`).
        return Err(payments_op("resolve replied message sender for .gift"));
    } else {
        if tokens.len() < 2 {
            ctx.reply("`Usage: .gift <user> <gift_id> [-anon] [message...]`\n(or reply to the user's message and just give the `<gift_id>`)\nSpends real Telegram Stars — a confirmation step runs first.").await?;
            return Ok(());
        }
        let rest = tokens[2..].join(" ");
        (
            tokens[0].clone(),
            tokens[1].clone(),
            if rest.is_empty() { None } else { Some(rest) },
        )
    };

    let Ok(gift_id) = gift_tok.parse::<i64>() else {
        ctx.reply("`gift_id must be numeric — see .gifts for the catalog.`")
            .await?;
        return Ok(());
    };
    // The recipient's display name needs `get_entity`; fall back to the
    // token itself (Python falls back to `str(target)`).
    let who_name = target.clone();

    if !confirmed {
        let nonce = stash(
            ctx.sender_id,
            "send",
            json!({"target": target, "gift_id": gift_id, "message": message_text, "hidden": hidden, "who_name": who_name}),
        )
        .await;
        let mut preview = format!("**Send Star Gift #{gift_id}** to **{who_name}**?\n");
        if let Some(m) = &message_text {
            preview.push_str(&format!("Message: _{m}_\n"));
        }
        if hidden {
            preview.push_str("Hidden name: yes\n");
        }
        preview.push_str(&format!(
            "\n_This spends real Telegram Stars._ Nothing is paid yet — re-run this exact command with `confirm` appended, or just run `.gift {nonce}`."
        ));
        ctx.reply(&preview).await?;
        return Ok(());
    }
    do_send_gift(ctx, &target, gift_id, message_text, hidden, &who_name).await
}

async fn do_send_gift(
    ctx: &Ctx,
    target: &str,
    gift_id: i64,
    _message: Option<String>,
    _hidden: bool,
    _who_name: &str,
) -> Result<()> {
    // TODO(responder-op): payments.SendStarsForm flow for a star gift
    // (`pyUltroid._core.stars.send_gift`) — resolve `target`, pay, and
    // deliver any payment-verification URL to Saved Messages only
    // (Python: gifts.py:_do_send_gift).
    let _ = ctx;
    Err(payments_op(&format!(
        "send star gift #{gift_id} to {target}"
    )))
}

async fn gift_upgrade(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    let (tokens, confirmed) = split_confirmed(tokens);
    if !confirmed && tokens.len() == 1 {
        if let Some(data) = pop(&tokens[0], ctx.sender_id, "upgrade").await {
            return do_upgrade_gift(ctx, &data["gift"].as_str().unwrap_or("").to_string()).await;
        }
    }
    if tokens.is_empty() {
        ctx.reply("`Usage: .giftupgrade <ref> [confirm]` (see .giftsaved)")
            .await?;
        return Ok(());
    }
    let gift_ref = parse_gift_ref(&tokens[0]);
    if !confirmed {
        let nonce = stash(ctx.sender_id, "upgrade", json!({"gift": gift_ref})).await;
        ctx.reply(&format!(
            "**Upgrade star gift** `{gift_ref}` **to its unique collectible form?**\n_This may spend Stars if the gift needs an upgrade fee._\nNothing is paid yet — re-run this exact command with `confirm` appended, or just run `.giftupgrade {nonce}`."
        ))
        .await?;
        return Ok(());
    }
    do_upgrade_gift(ctx, &gift_ref).await
}

async fn do_upgrade_gift(ctx: &Ctx, gift_ref: &str) -> Result<()> {
    let _ = ctx;
    // TODO(responder-op): payments upgrade of a saved gift
    // (`pyUltroid._core.stars.upgrade`) (Python: gifts.py:_do_upgrade_gift).
    Err(payments_op(&format!("upgrade saved gift `{gift_ref}`")))
}

async fn gift_transfer(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    let (tokens, confirmed) = split_confirmed(tokens);
    if !confirmed && tokens.len() == 1 {
        if let Some(data) = pop(&tokens[0], ctx.sender_id, "transfer").await {
            return do_transfer_gift(
                ctx,
                &data["gift"].as_str().unwrap_or("").to_string(),
                &data["to_peer"].as_str().unwrap_or("").to_string(),
            )
            .await;
        }
    }
    if tokens.len() < 2 {
        ctx.reply("`Usage: .gifttransfer <ref> <user> [confirm]` (see .giftsaved)")
            .await?;
        return Ok(());
    }
    let gift_ref = parse_gift_ref(&tokens[0]);
    let to_peer = tokens[1].clone();
    if !confirmed {
        let nonce = stash(
            ctx.sender_id,
            "transfer",
            json!({"gift": gift_ref, "to_peer": to_peer}),
        )
        .await;
        ctx.reply(&format!(
            "**Transfer star gift** `{gift_ref}` **to** `{to_peer}`**?**\n_This may cost Stars unless you have Telegram Premium._\nNothing is transferred yet — re-run this exact command with `confirm` appended, or just run `.gifttransfer {nonce}`."
        ))
        .await?;
        return Ok(());
    }
    do_transfer_gift(ctx, &gift_ref, &to_peer).await
}

async fn do_transfer_gift(ctx: &Ctx, gift_ref: &str, to_peer: &str) -> Result<()> {
    let _ = ctx;
    // TODO(responder-op): payments transfer of a saved gift
    // (`pyUltroid._core.stars.transfer`) (Python: gifts.py:_do_transfer_gift).
    Err(payments_op(&format!(
        "transfer saved gift `{gift_ref}` to {to_peer}"
    )))
}

async fn gift_save(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<String> = ctx.args.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() {
        ctx.reply("`Usage: .giftsave <ref> [unsave]` (see .giftsaved)")
            .await?;
        return Ok(());
    }
    let gift_ref = parse_gift_ref(&tokens[0]);
    let unsave =
        tokens.len() > 1 && matches!(tokens[1].to_lowercase().as_str(), "unsave" | "hide" | "off");
    let _ = unsave;
    // TODO(responder-op): payments save/unsave of a saved gift
    // (`pyUltroid._core.stars.save`) (Python: gifts.py:gift_save_cmd).
    Err(payments_op(&format!(
        "save/unsave gift `{gift_ref}` on profile"
    )))
}

async fn gift_saved(ctx: &Ctx) -> Result<()> {
    let peer = ctx.args.trim();
    let _peer = if peer.is_empty() { "me" } else { peer };
    // TODO(responder-op): payments.GetSavedStarGiftsRequest
    // (Python: gifts.py:gift_saved_cmd). Rendering the returned rows as
    // a `RichDoc` table happens in the follow-up pass with the RPC.
    Err(payments_op(
        "payments.GetSavedStarGiftsRequest (saved gifts)",
    ))
}
