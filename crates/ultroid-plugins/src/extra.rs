//! Extra message utilities — port of `plugins/extra.py` (85 LOC).
//!
//! `.del` / `.copy` / `.edit` / `.reply`: small message-management
//! commands. Reading the replied message, editing or deleting a message
//! other than the command message itself, and iterating the account's own
//! messages are binary Responder operations (no such surface on `Ctx`
//! today); those sites return the honest `TODO(responder-op)` error.
//! Everything around them — argument handling, the no-reply branches,
//! deleting the command message itself — is implemented for real.

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
    if ctx.reply_to_id.is_none() {
        return Ok(());
    }
    // TODO(responder-op): delete the replied message (`msg_src.try_delete()`)
    // and then the command message (`delme.try_delete()`).
    Err(Error::Other(
        "delete replied message requires a binary Responder op (Python: extra.py:delete_it)"
            .to_string(),
    ))
}

/// `.copy` — `plugins/extra.py:copy`.
async fn copy_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        // Python: `await e.eor(get_string("ex_1"), time=5)`.
        ctx.reply(&ctx.get_str("ex_1")).await?;
        return Ok(());
    }
    // TODO(responder-op): fetch the replied message and re-send it
    // (`await reply.reply(reply)`), then delete the command message.
    Err(Error::Other(
        "fetch + re-send replied message requires a binary Responder op (Python: extra.py:copy)"
            .to_string(),
    ))
}

/// `.edit <text>` — `plugins/extra.py:editer`.
/// Python takes `message[6:]` (text after `.edit `), which is `ctx.args`.
async fn edit_cmd(ctx: &Ctx) -> Result<()> {
    let string = ctx.args.trim().to_string();
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): fetch the replied message and edit it
        // (`await reply.edit(string)`), then delete the command message.
        return Err(Error::Other(
            "edit replied message requires a binary Responder op (Python: extra.py:editer)"
                .to_string(),
        ));
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
