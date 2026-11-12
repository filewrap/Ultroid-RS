//! Fact-checks — port of `plugins/factcheck.py`
//! (`.factcheck` / `.factclear`).
//!
//! The commands bottom out in raw MTProto fact-check requests
//! (`messages.EditFactCheckRequest`, `messages.DeleteFactCheckRequest`,
//! `messages.GetFactCheckRequest`, plus reading `Message.factcheck` off
//! the replied message). None of those exists on the frozen
//! `Ctx`/`Responder` surface, so the reply validation and the friendly
//! error mapping are ported faithfully and the Telegram call itself is an
//! honest `TODO(responder-op)` error at the exact call site.
//!
//! Note from the Python docstring, preserved: adding/removing a
//! fact-check is gated server-side behind Telegram's independent
//! fact-checker program, not a normal chat-admin right — a permission
//! error there is Telegram's fact-checker program, not a bug here.

use crate::util::reg_add;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

/// Port of `_friendly_error` — turn any fact-check failure into a short,
/// chat-friendly message, never a traceback.
pub fn friendly_error(exc: &str) -> String {
    let message = exc.to_uppercase();
    if message.contains("FACTCHECK") && message.contains("ALLOWED") {
        format!(
            "🚫 **This account isn't an independent fact-checker.**\n\
             Adding/removing fact-checks is gated behind Telegram's \
             fact-checker program, not a normal chat-admin right.\n`{exc}`"
        )
    } else if message.contains("ADMIN") {
        format!("🚫 **Missing rights in this chat.**\n`{exc}`")
    } else if message.contains("TOO_LONG") {
        format!("❌ **Fact-check text is too long.**\n`{exc}`")
    } else if message.contains("PRIVATE") {
        format!("❌ **Can't reach this chat.**\n`{exc}`")
    } else {
        format!("❌ **Error.**\n`{exc}`")
    }
}

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

/// `.factcheck [text]` (reply to a post) — port of `factcheck.py::_factcheck`.
async fn factcheck(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        return ctx
            .reply("`Reply to the post you want to manage the fact-check of.`")
            .await
            .map(|_| ());
    }
    let raw = ctx.args.trim();
    if raw.is_empty() {
        // Bare `.factcheck` — show the fact-check already attached to the
        // replied message (reads `Message.factcheck`, refetching full text
        // via messages.GetFactCheckRequest when only a hash is cached).
        // TODO(responder-op): read replied Message.factcheck +
        // messages.GetFactCheckRequest (Python: factcheck.py:_factcheck,
        // _fetch_full_factcheck).
        return Err(todo_op(
            "read replied Message.factcheck + messages.GetFactCheckRequest",
            "factcheck.py:_factcheck",
        ));
    }
    let _ok = ctx.reply("`Updating fact-check...`").await?;
    // TODO(responder-op): messages.EditFactCheckRequest(peer, msg_id,
    // TextWithEntities(md-v3 text, entities))
    // (Python: factcheck.py:_factcheck). The MarkdownV3 -> entities
    // conversion (`custom.entities.mdv3_to_entities`) has no equivalent in
    // ultroid-core either; the raw text would need that conversion first.
    Err(todo_op(
        "messages.EditFactCheckRequest with TextWithEntities (mdv3 -> entities)",
        "factcheck.py:_factcheck",
    ))
}

/// `.factclear` (reply to a post) — port of `factcheck.py::_factclear`.
async fn factclear(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        return ctx
            .reply("`Reply to the post you want to manage the fact-check of.`")
            .await
            .map(|_| ());
    }
    let _ok = ctx.reply("`Removing fact-check...`").await?;
    // TODO(responder-op): messages.DeleteFactCheckRequest(peer, msg_id)
    // (Python: factcheck.py:_factclear).
    Err(todo_op(
        "messages.DeleteFactCheckRequest",
        "factcheck.py:_factclear",
    ))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "factcheck",
            pattern: "factcheck( (.*)|$)",
            help: "Add/update (or show) the fact-check on the replied post.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(factcheck(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "factclear",
            pattern: "factclear$",
            help: "Remove the fact-check from the replied post.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(factclear(ctx)),
    );
}
