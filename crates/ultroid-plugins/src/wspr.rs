//! Whisper (secret message) — port of `plugins/_wspr.py` (`.wspr`).
//!
//! The command itself is registered with its exact Python pattern. Its
//! delivery is an assistant-bot inline query (`msg <user>`) + click,
//! wired via `Responder::inline_query_click`.
//!
//! The inline-query handlers (`wspr`, `msg`) and the `dd_` / `del_`
//! callbacks are assistant-bot inline/callback handlers, not commands;
//! there is no inline or callback registration surface in the plugin
//! crate, so they are not portable in this wave. Their shared state — the
//! `buddhhu` map of `inline_id -> [target_user_id, sender_user_id, text]`
//! — is documented here so the follow-up pass can wire it:
//! `buddhhu: HashMap<i64, (i64, i64, String)>`.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

/// `.wspr [user]` — port of `_wspr.py` command handler.
///
/// With a reply, Python targets the replied-to sender (`@username` when
/// it has one, else the numeric id); the sender id comes from
/// `ctx.reply_to_sender_id` and the username from `user_full`.
async fn wspr(ctx: &Ctx) -> Result<()> {
    let put: String = if ctx.reply_to_id.is_some() {
        let Some(sender_id) = ctx.reply_to_sender_id else {
            return Err(Error::Other(
                "could not determine the replied message's sender".into(),
            ));
        };
        match ctx.responder.user_full(sender_id).await {
            Ok(user) => match user.username {
                Some(name) if !name.is_empty() => format!("@{name}"),
                _ => sender_id.to_string(),
            },
            Err(_) => sender_id.to_string(),
        }
    } else {
        let put = ctx.args.trim().to_string();
        if put.is_empty() {
            return ctx.reply(&ctx.get_str("wspr_3")).await.map(|_| ());
        }
        put
    };
    let Some(asst_id) = ctx.asst_dialog_id else {
        return Err(Error::Other(
            "sending a whisper requires the assistant bot to be connected".into(),
        ));
    };
    ctx.responder
        .inline_query_click(
            asst_id,
            &format!("msg {put}"),
            ctx.chat_id,
            ctx.reply_to_id.map(|id| id as i32),
        )
        .await?;
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "wspr",
            "wspr( (.*)|$)",
            "Send a secret (whisper) message to a user.",
        ),
        |ctx| Box::pin(wspr(ctx)),
    );
}
