//! Port of `plugins/other.py` — miscellaneous message utilities.
//!
//! `.send`/`.dm <user/chat> <text>` — send a message to a user/chat.
//! `.fwdreply <text>` — reply to someone's message by forwarding it in
//! private. `.save`/`.fsave` — save the replied message to Saved
//! Messages (copy / forward).
//!
//! Argument parsing and validation are real. Sending, forwarding and
//! resolving a username to an id go through the `Responder`
//! (`send_to` / `forward_to` / `resolve_user` / `message_summary` /
//! `download_media` / `me`).
//!
//! Note: `.fwdreply` replies to the forwarded copy with a plain send —
//! the `Responder` surface has no reply-to-a-message send. `.save`
//! (copy) re-sends the replied message's text — or its downloaded
//! media with the text as caption — to Saved Messages, which is the
//! account's own id from `me()`.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut dm_def = def(
        "send",
        "(send|dm)( (.*)|$)",
        "Send a message to a user/chat: `.send <username/id> <text>`.",
    );
    dm_def.fullsudo = true;
    reg_add(registry, dm_def, |ctx| Box::pin(dm(ctx)));

    let mut fwd = def(
        "fwdreply",
        "fwdreply( (.*)|$)",
        "Reply to someone's message by forwarding it in private.",
    );
    fwd.fullsudo = true;
    reg_add(registry, fwd, |ctx| Box::pin(fwdreply(ctx)));

    reg_add(
        registry,
        def(
            "save",
            "(f|)save$",
            "Save the replied message to Saved Messages (`.fsave` forwards it instead).",
        ),
        |ctx| Box::pin(save(ctx)),
    );
}

async fn dm(ctx: &Ctx) -> Result<()> {
    let mut words = ctx.args.split_whitespace();
    let Some(chat_tok) = words.next() else {
        ctx.reply(&ctx.get_str("dm_1")).await?;
        return Ok(());
    };
    let chat_id = match chat_tok.trim_start_matches('@').parse::<i64>() {
        Ok(id) => id,
        Err(_) => ctx.responder.resolve_user(chat_tok).await?.id,
    };
    let msg = ctx.args.splitn(2, char::is_whitespace).nth(1);
    let Some(msg) = msg
        .filter(|m| !m.trim().is_empty())
        .or(ctx.reply_to_text.as_deref())
    else {
        ctx.reply(&ctx.get_str("dm_2")).await?;
        return Ok(());
    };
    ctx.responder.send_to(chat_id, msg).await?;
    ctx.reply(&ctx.get_str("dm_3")).await?;
    Ok(())
}

async fn fwdreply(ctx: &Ctx) -> Result<()> {
    let message = ctx.args.trim();
    let Some(reply_id) = ctx.reply_to_id else {
        ctx.reply(&ctx.get_str("ex_1")).await?;
        return Ok(());
    };
    if message.is_empty() {
        ctx.reply(&ctx.get_str("dm_6")).await?;
        return Ok(());
    }
    let Some(sender_id) = ctx.reply_to_sender_id else {
        return Err(Error::Other(
            "could not determine the sender of the replied message".to_string(),
        ));
    };
    ctx.responder
        .forward_to(ctx.chat_id, reply_id as i32, sender_id)
        .await?;
    // No reply-to-a-message send on the `Responder` surface, so the
    // reply to the forwarded copy goes as a plain message to its sender.
    ctx.responder.send_to(sender_id, message).await?;
    ctx.reply(&ctx.get_str("dm_5")).await?;
    Ok(())
}

async fn save(ctx: &Ctx) -> Result<()> {
    let Some(reply_id) = ctx.reply_to_id else {
        ctx.reply("Reply to Any Message to save it to ur saved messages")
            .await?;
        return Ok(());
    };
    let forward = ctx.captures.get(1).map(|s| s == "f").unwrap_or(false);
    // Saved Messages is the account's own dialog.
    let me_id = ctx.responder.me().await?.id;
    if forward {
        ctx.responder
            .forward_to(ctx.chat_id, reply_id as i32, me_id)
            .await?;
    } else {
        let Some(summary) = ctx
            .responder
            .message_summary(ctx.chat_id, reply_id as i32)
            .await?
        else {
            return Err(Error::Other("the replied message was deleted".to_string()));
        };
        if summary.has_media {
            let dest = std::env::temp_dir()
                .join(format!("ultroid-save-{}", uuid::Uuid::new_v4().as_simple()));
            let path = ctx
                .responder
                .download_media(ctx.chat_id, reply_id as i32, &dest)
                .await?;
            let r = ctx
                .responder
                .send_file_to(me_id, &path, &summary.text)
                .await;
            let _ = tokio::fs::remove_file(&path).await;
            r?;
        } else {
            ctx.responder.send_to(me_id, &summary.text).await?;
        }
    }
    ctx.reply("Message saved to Your Pm/Saved Messages.")
        .await?;
    Ok(())
}
