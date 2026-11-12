//! Port of `plugins/status.py` — Telegram Premium emoji status.
//!
//! `.emojistatus <doc_id> [hours]` (or reply to a custom-emoji message),
//! `.emojistatus off` — set/clear your profile emoji status. Premium
//! required, enforced server-side.
//!
//! Argument parsing (doc id, optional hours, `off`/`clear`/`none`,
//! reply-to-custom-emoji) is real. The Premium probe (`get_me`) and the
//! `account.UpdateEmojiStatusRequest` RPC are client ops the `Responder`
//! surface does not expose yet — each handler validates first, then
//! returns an honest `TODO(responder-op)` error at that exact point.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub const PREMIUM_REQUIRED: &str =
    "⭐ **Telegram Premium is required to set an emoji status.**\n`This account isn't Premium.`";

pub fn register(registry: &mut Registry) {
    let mut d = def(
        "emojistatus",
        "emojistatus( (.*)|$)",
        "Set/clear your emoji status: `.emojistatus <doc_id> [hours]`, `.emojistatus off`. Premium required.",
    );
    d.fullsudo = true;
    reg_add(registry, d, |ctx| Box::pin(emoji_status(ctx)));
}

async fn emoji_status(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let tokens: Vec<&str> = raw.split_whitespace().collect();

    if let Some(first) = tokens.first() {
        if matches!(first.to_lowercase().as_str(), "off" | "clear" | "none") {
            let _id = ctx.reply(&ctx.get_str("com_1")).await?;
            // TODO(responder-op): account.UpdateEmojiStatusRequest with
            // EmojiStatusEmpty (Python: status.py:_emoji_status).
            return Err(Error::Other(
                "account.UpdateEmojiStatusRequest(EmojiStatusEmpty) requires a binary Responder op (Python: status.py:_emoji_status)".to_string(),
            ));
        }
    }

    let (doc_id, hours): (u64, Option<u64>) = if ctx.reply_to_id.is_some() {
        // The replied message's custom-emoji document id lives in its
        // entities/media — neither is visible on `Ctx`.
        // TODO(responder-op): extract the custom-emoji document id from
        // the replied message (Python: status.py:_extract_doc_id_from_message).
        return Err(Error::Other(
            "extract custom-emoji document id from replied message requires a binary Responder op (Python: status.py:_extract_doc_id_from_message)".to_string(),
        ));
    } else {
        let Some(first) = tokens.first() else {
            ctx.reply("`Usage: .emojistatus <doc_id> [hours]` (or reply to a custom emoji), or `.emojistatus off`").await?;
            return Ok(());
        };
        let Ok(doc_id) = first.parse::<u64>() else {
            ctx.reply("`Usage: .emojistatus <doc_id> [hours]` (or reply to a custom emoji), or `.emojistatus off`").await?;
            return Ok(());
        };
        let hours = match tokens.get(1) {
            Some(h) => match h.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    ctx.reply("`<hours> must be a number.`").await?;
                    return Ok(());
                }
            },
            None => None,
        };
        (doc_id, hours)
    };
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): Premium probe (`get_me` + `is_premium`) and
    // account.UpdateEmojiStatusRequest(EmojiStatus(document_id, until))
    // (Python: status.py:_emoji_status).
    let suffix = hours.map(|h| format!(" for {h}h")).unwrap_or_default();
    Err(Error::Other(format!(
        "account.UpdateEmojiStatusRequest(document_id={doc_id}{suffix}) + Premium probe requires a binary Responder op (Python: status.py:_emoji_status)"
    )))
}
