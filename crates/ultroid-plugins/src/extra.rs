//! Extra message utilities — port of `plugins/extra.py` (85 LOC).
//!
//! `.del` / `.copy` / `.edit` / `.reply`: small message-management
//! commands. The replied message is read, edited, deleted and re-sent
//! through the Responder (`message_summary`, `edit`, `delete`); iterating
//! the account's own messages is not on the trait, so `.edit` with no
//! reply and `.reply` with a reply keep their honest
//! `TODO(responder-op)` error. Everything around them — argument
//! handling, the no-reply branches, deleting the command message
//! itself — is implemented for real.

use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

const HELP: &str = "• `.del` — delete the replied message (and the command).\n\
• `.copy` — re-send the replied message as your own.\n\
• `.edit <text>` — edit the replied message, or your last message.\n\
• `.reply` — re-send your last message as a reply to the replied message.";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("del", "del$", HELP), |ctx| {
        Box::pin(delete_it(ctx))
    });
    reg_add(registry, def("copy", "copy$", HELP), |ctx| {
        Box::pin(copy_cmd(ctx))
    });
    reg_add(registry, def("edit", "edit", HELP), |ctx| {
        Box::pin(edit_cmd(ctx))
    });
    reg_add(registry, def("reply", "reply$", HELP), |ctx| {
        Box::pin(reply_cmd(ctx))
    });
}

/// `.del` — `plugins/extra.py:delete_it` (`manager=True`: also registered
/// on the manager client in Python; the Rust registry already serves both
/// `.` and `/` prefixes, so no extra flag is needed).
async fn delete_it(ctx: &Ctx) -> Result<()> {
    // Python: `msg_src = await delme.get_reply_message()`; no reply → return.
    let Some(reply_id) = ctx.reply_to_id else {
        return Ok(());
    };
    ctx.delete(reply_id).await?;
    ctx.delete(ctx.message_id).await?;
    Ok(())
}

/// `.copy` — `plugins/extra.py:copy`. Text replies are re-sent as the
/// account's own message; a media reply cannot be re-sent (no trait op
/// re-sends stored media), so it is an honest error.
async fn copy_cmd(ctx: &Ctx) -> Result<()> {
    let Some(reply_id) = ctx.reply_to_id else {
        // Python: `await e.eor(get_string("ex_1"), time=5)`.
        ctx.reply(&ctx.get_str("ex_1")).await?;
        return Ok(());
    };
    let Some(summary) = ctx
        .responder
        .message_summary(ctx.chat_id, reply_id as i32)
        .await?
    else {
        return Err(Error::Other(
            "the replied message is no longer available".into(),
        ));
    };
    if summary.has_media {
        return Err(Error::Other(
            "re-sending a media message requires a binary Responder op (Python: extra.py:copy)"
                .to_string(),
        ));
    }
    if summary.text.is_empty() {
        return Err(Error::Other(
            "the replied message has no text to copy".into(),
        ));
    }
    ctx.reply(&summary.text).await?;
    ctx.delete(ctx.message_id).await?;
    Ok(())
}

/// `.edit <text>` — `plugins/extra.py:editer`.
/// Python takes `message[6:]` (text after `.edit `), which is `ctx.args`.
async fn edit_cmd(ctx: &Ctx) -> Result<()> {
    let string = ctx.args.trim().to_string();
    if let Some(reply_id) = ctx.reply_to_id {
        // Python edits the replied message (`await reply.edit(string)`),
        // then deletes the command message.
        ctx.edit(reply_id, &string).await?;
        ctx.delete(ctx.message_id).await?;
        return Ok(());
    }
    // No reply: Python edits the account's second-most-recent own message
    // (`iter_messages(chat, from_user="me", limit=2)`, index 2).
    // TODO(responder-op): iterate own messages in this chat.
    let _ = string;
    Err(Error::Other(
        "iterate own messages requires a binary Responder op (Python: extra.py:editer)".to_string(),
    ))
}

/// `.reply` — `plugins/extra.py:_` (anonymous handler).
async fn reply_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): fetch the account's latest message in this
        // chat (`get_messages(chat, limit=1, max_id=e.id)`), delete the
        // command + that message, and re-send it as a reply to
        // `reply_to_msg_id`.
        return Err(Error::Other(
            "fetch latest own message + re-send as reply requires a binary Responder op (Python: extra.py:_)"
                .to_string(),
        ));
    }
    // No reply: Python just deletes the command message (`try_delete`).
    ctx.delete(ctx.message_id).await?;
    Ok(())
}
