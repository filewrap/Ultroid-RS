//! Whisper (secret message) — port of `plugins/_wspr.py` (`.wspr`).
//!
//! The command itself is registered with its exact Python pattern. Its
//! delivery is an assistant-bot inline query (`msg <user>`) + click, which
//! needs an assistant inline-query op that does not exist on `Responder`.
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

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

/// `.wspr [user]` — port of `_wspr.py` command handler.
///
/// With a reply, Python targets the replied-to sender; `Ctx` carries only
/// the replied message id, not its sender, so that branch also needs a
/// Responder op (fetch the replied message's sender).
async fn wspr(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): fetch replied message -> sender username/id
        return Err(todo_op(
            "get replied message sender for .wspr",
            "_wspr.py:<command>",
        ));
    }
    let put = ctx.args.trim();
    if put.is_empty() {
        return ctx.reply(&ctx.get_str("wspr_3")).await.map(|_| ());
    }
    // TODO(responder-op): assistant-bot inline query "msg <put>" + click
    Err(todo_op(
        "assistant inline query 'msg <user>' + click result",
        "_wspr.py:<command>",
    ))
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
