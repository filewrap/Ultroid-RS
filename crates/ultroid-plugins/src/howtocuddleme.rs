//! Port of `plugins/howtocuddleme.py` — developer template / feature
//! showcase. Ported faithfully as-is: each section below mirrors one
//! section of the Python file.
//!
//! 1. `cuddle_attr` — show client attributes (handler prefix, DB, client
//!    user). The handler prefix comes from config; the client user's
//!    name comes from the `Responder` `me()` op. The DB connection name
//!    has no accessor on `Ctx`, so that line is omitted.
//! 2. `cuddle_buttons` — styled inline buttons, sent with the
//!    `Responder` `send_markup_to` op.
//! 3. `cuddle_guest_allowed` / `cuddle_guest_restricted` — guest-mode
//!    gating via `allow_guest` on the `CommandDef` (real).
//! 4. `cuddle_ask` — inline `listen()` for the next message. The core
//!    `WaiterHub` is not carried on `Ctx`, so there is no way to await a
//!    reply from a plugin — `TODO(responder-op)`; the prompt itself is
//!    sent first, faithfully.
//! 5. `cuddle_part` — isolated `PluginPart` registration, ported as a
//!    plain command registration (Rust has no runtime plugin parts).

use crate::util::{def, reg_add};
use ultroid_core::tg::{ButtonStyle, InlineButton};
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "cuddle_attr",
            "cuddle_attr$",
            "Show modernized client attributes (template showcase).",
        ),
        |ctx| Box::pin(show_attributes(ctx)),
    );
    reg_add(
        registry,
        def(
            "cuddle_buttons",
            "cuddle_buttons$",
            "Show custom styled buttons (template showcase).",
        ),
        |ctx| Box::pin(show_buttons(ctx)),
    );
    let mut allowed = def(
        "cuddle_guest_allowed",
        "cuddle_guest_allowed$",
        "Guest-allowed demo command.",
    );
    allowed.allow_guest = true;
    reg_add(registry, allowed, |ctx| Box::pin(guest_allowed(ctx)));

    let mut restricted = def(
        "cuddle_guest_restricted",
        "cuddle_guest_restricted$",
        "Guest-restricted demo command.",
    );
    restricted.allow_guest = false;
    reg_add(registry, restricted, |ctx| Box::pin(guest_restricted(ctx)));

    reg_add(
        registry,
        def(
            "cuddle_ask",
            "cuddle_ask$",
            "Ask a question and listen for the next message (template showcase).",
        ),
        |ctx| Box::pin(ask_question(ctx)),
    );
    reg_add(
        registry,
        def(
            "cuddle_part",
            "cuddle_part$",
            "Isolated plugin-part demo command.",
        ),
        |ctx| Box::pin(custom_part(ctx)),
    );
}

async fn show_attributes(ctx: &Ctx) -> Result<()> {
    // NOTE: the DB connection name has no accessor on `Ctx`, so that
    // line from the Python version is omitted here.
    let me = ctx.responder.me().await?;
    ctx.reply(&format!(
        "✨ **Modernized Client Attributes**:\n• Handler: `{}`\n• Client User: `{}`",
        ctx.config.hndlr, me.first_name
    ))
    .await?;
    Ok(())
}

async fn show_buttons(ctx: &Ctx) -> Result<()> {
    // The Python version builds a styled ReplyInlineMarkup (danger /
    // success / primary rows + a URL button) via `ikm(...)`; the style
    // is carried on each `InlineButton` here.
    let buttons = vec![
        vec![
            InlineButton::data("Danger Action", b"cuddle_click_danger".to_vec())
                .styled(ButtonStyle::Danger),
        ],
        vec![
            InlineButton::data("Success Action", b"cuddle_click_success".to_vec())
                .styled(ButtonStyle::Success),
        ],
        vec![InlineButton::url("Info link", "https://google.com").styled(ButtonStyle::Primary)],
    ];
    ctx.responder
        .send_markup_to(ctx.chat_id, "💖 Choose a custom styled action:", buttons)
        .await?;
    Ok(())
}

async fn guest_allowed(ctx: &Ctx) -> Result<()> {
    ctx.reply("✅ Welcome! This command is allowed for guest sessions.")
        .await?;
    Ok(())
}

async fn guest_restricted(ctx: &Ctx) -> Result<()> {
    ctx.reply("🔒 Secret information for the owner only!")
        .await?;
    Ok(())
}

async fn ask_question(ctx: &Ctx) -> Result<()> {
    ctx.reply("🤔 What is your favorite programming language? (Reply in 15 seconds):")
        .await?;
    // TODO(responder-op): await the next incoming message in this chat
    // with a 15s timeout (Python: howtocuddleme.py:ask_question_inline,
    // `client.listen(chat_id, NewMessage(incoming=True), timeout=15)`).
    // `ultroid_core::listen::WaiterHub` exists, but no hub is carried on
    // `Ctx`, so a plugin cannot wait on it yet.
    Err(Error::Other(
        "listen for next message requires a binary Responder op (Python: howtocuddleme.py:ask_question_inline)".to_string(),
    ))
}

async fn custom_part(ctx: &Ctx) -> Result<()> {
    ctx.reply("🛠️ Registered safely through isolated `PluginPart` registry!")
        .await?;
    Ok(())
}
