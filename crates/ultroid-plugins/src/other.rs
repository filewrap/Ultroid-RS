//! Port of `plugins/other.py` — miscellaneous message utilities.
//!
//! `.send`/`.dm <user/chat> <text>` — send a message to a user/chat.
//! `.fwdreply <text>` — reply to someone's message by forwarding it in
//! private. `.save`/`.fsave` — save the replied message to Saved
//! Messages (copy / forward).
//!
//! Argument parsing and validation are real. Resolving a username to an
//! id, sending to an arbitrary peer, and forwarding/copying a message
//! all need client calls the `Responder` surface does not expose yet
//! (see the `TODO(responder-op)` sites).

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

fn responder_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Resolve a numeric id; usernames need a client-side `parse_id`.
fn resolve_peer(token: &str) -> std::result::Result<i64, Error> {
    token
        .trim()
        .trim_start_matches('@')
        .parse::<i64>()
        .map_err(|_| responder_op(&format!("resolve peer '{token}' to an id"), "other.py:dm"))
}

async fn dm(ctx: &Ctx) -> Result<()> {
    let mut words = ctx.args.split_whitespace();
    let Some(chat_tok) = words.next() else {
        ctx.reply(&ctx.get_str("dm_1")).await?;
        return Ok(());
    };
    let chat_id = resolve_peer(chat_tok)?;
    let msg = ctx.args.splitn(2, char::is_whitespace).nth(1);
    let Some(msg) = msg
        .filter(|m| !m.trim().is_empty())
        .or(ctx.reply_to_text.as_deref())
    else {
        ctx.reply(&ctx.get_str("dm_2")).await?;
        return Ok(());
    };
    let _ = msg;
    let _ = chat_id;
    // TODO(responder-op): send `msg` to `chat_id` (Python: other.py:dm,
    // `e.client.send_message(chat_id, msg)`).
    Err(responder_op(
        &format!("send_message to peer {chat_id}"),
        "other.py:dm",
    ))
}

async fn fwdreply(ctx: &Ctx) -> Result<()> {
    let message = ctx.args.trim();
    let Some(_reply_id) = ctx.reply_to_id else {
        ctx.reply(&ctx.get_str("ex_1")).await?;
        return Ok(());
    };
    if message.is_empty() {
        ctx.reply(&ctx.get_str("dm_6")).await?;
        return Ok(());
    }
    // TODO(responder-op): forward the replied message to its sender, then
    // reply `message` to the forwarded copy (Python: other.py:_,
    // `msg.forward_to(msg.sender_id)` + `fwd.reply(message)`).
    Err(responder_op(
        "forward replied message to its sender",
        "other.py:_",
    ))
}

async fn save(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("Reply to Any Message to save it to ur saved messages")
            .await?;
        return Ok(());
    }
    let forward = ctx.captures.get(1).map(|s| s == "f").unwrap_or(false);
    // TODO(responder-op): copy (or, for `.fsave`, forward) the replied
    // message to Saved Messages (Python: other.py:saf,
    // `x.forward_to(e.sender_id)` / `e.client.send_message(e.sender_id, x)`).
    Err(responder_op(
        if forward {
            "forward replied message to Saved Messages"
        } else {
            "copy replied message to Saved Messages"
        },
        "other.py:saf",
    ))
}
