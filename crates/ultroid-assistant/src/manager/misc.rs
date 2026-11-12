//! Small assistant utility commands.
//!
//! Port of `assistant/manager/misc.py`.
//!
//! Ported in full: `decide`, `echo`, `joke`.
//!
//! BLOCKED — `kickme` (`doit`): the action is
//! `client.kick_participant(chat_id, sender_id)` and there is no
//! kick/leave method on [`ultroid_core::Responder`] and no client handle
//! on [`ultroid_core::Ctx`]. The developer guard (`DEVLIST`, i.e.
//! [`ultroid_core::Config::devlist_ids`]) and the two replies ("I will Not
//! Kick You, my Developer.." / "Yes, You are right, get out.") are
//! preserved as [`KICKME_DEV_REPLY`] / [`KICKME_REPLY`] for the binary to
//! use once a kick surface exists; the command is not registered.
//!
//! Notes on the ported commands:
//!
//! * `echo`'s gate is `fns.admin_check(e)`, which needs
//!   `client.get_permissions(chat_id, user.id)` plus the admin-verification
//!   callback flow — neither exists in this crate. Owners and full sudos
//!   pass (Python's first check); anyone else gets Python's exact
//!   non-admin denial, "Only Admins can use this command!", because group
//!   admin status cannot be verified here.
//! * `decide` / `joke` reply to the command message. Python's
//!   `reply_to=ri` targets the *replied-to* message when the command is a
//!   reply; [`ultroid_core::Responder::reply`] cannot target an arbitrary
//!   message id, so the reply lands on the command message instead.
//! * Timed deletions (`eor(..., time=5)`, `time=8`) are implemented with
//!   a spawned task that sleeps then deletes — the same observable
//!   behaviour as Python's `eod`/`eor` timeout.

use std::time::Duration;

use ultroid_core::Ctx;
use ultroid_core::error::{Error, Result};
use ultroid_core::router::{CommandDef, Registry};

use crate::misc_assistant::fn_handler;

/// The joke endpoint (Python `do_joke`).
pub const JOKE_URL: &str = "https://v2.jokeapi.dev/joke/Any?blacklistFlags=nsfw,religious,political,racist,sexist,explicit&type=single";

/// `decide` choices (Python `dheh`).
pub const DECIDE_CHOICES: &[&str] = &["Yes", "NoU", "Maybe", "IDK"];

/// `kickme` replies (blocked command — see module docs).
pub const KICKME_DEV_REPLY: &str = "I will Not Kick You, my Developer..";
pub const KICKME_REPLY: &str = "Yes, You are right, get out.";

/// `echo` denial for non-admins (Python `admin_check`).
pub const ECHO_NOT_ADMIN: &str = "Only Admins can use this command!";

/// `echo` prompt when there is nothing to echo (Python `e.eor("What to
/// Echo?", time=5)`).
pub const ECHO_NOTHING: &str = "What to Echo?";

/// Pseudo-random index in `0..n` from the system clock (this crate has no
/// `rand` dependency).
fn random_below(n: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u128 * 1_000_000_000 + d.subsec_nanos() as u128)
        .unwrap_or(0);
    (nanos % n as u128) as usize
}

/// Delete a message after `secs` seconds, in the background (Python's
/// `eod` / `eor(..., time=N)` timeout).
fn delete_after(ctx: &Ctx, msg_id: i64, secs: u64) {
    let responder = ctx.responder.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        let _ = responder.delete(msg_id).await;
    });
}

/// `decide` — reply with a random choice of Yes / NoU / Maybe / IDK
/// (Python `dheh`).
pub async fn decide(ctx: &Ctx) -> Result<()> {
    let choice = DECIDE_CHOICES[random_below(DECIDE_CHOICES.len())];
    ctx.reply(choice).await?;
    Ok(())
}

/// `echo` — repeat the argument, or the replied message's text, back into
/// the chat (Python `oqha`, `@asst_cmd(pattern="echo( (.*)|$)")`).
///
/// Admin-gated (see module docs). Deletes the command message
/// best-effort, as Python does (`try: await e.delete()`).
pub async fn echo(ctx: &Ctx) -> Result<()> {
    if !(ctx.is_owner || ctx.is_sudo) {
        let id = ctx.reply(ECHO_NOT_ADMIN).await?;
        delete_after(ctx, id, 8);
        return Ok(());
    }
    let text = if !ctx.args.trim().is_empty() {
        Some(ctx.args.trim().to_string())
    } else {
        ctx.reply_to_text.clone()
    };
    let Some(text) = text else {
        let id = ctx.reply(ECHO_NOTHING).await?;
        delete_after(ctx, id, 5);
        return Ok(());
    };
    let _ = ctx.delete(ctx.message_id).await;
    ctx.reply(&text).await?;
    Ok(())
}

/// `joke` — fetch a single-type joke from jokeapi.dev and reply with it
/// (Python `do_joke`).
pub async fn joke(ctx: &Ctx) -> Result<()> {
    let value: serde_json::Value = ctx.http.get(JOKE_URL).send().await?.json().await?;
    let joke = value
        .get("joke")
        .and_then(|j| j.as_str())
        .ok_or_else(|| Error::Other("jokeapi response had no `joke` field".to_string()))?;
    ctx.reply(joke).await?;
    Ok(())
}

/// Is the sender a developer who may never be kicked (Python `kickme`'s
/// `DEVLIST` check)? Ported as a predicate; the command itself is blocked
/// — see the module docs.
pub fn is_kickme_exempt(ctx: &Ctx) -> bool {
    ctx.config.is_dev(ctx.sender_id)
}

/// Register `decide`, `echo`, `joke`. `kickme` is not registered — see the
/// module docs.
pub fn register(registry: &mut Registry) {
    registry.add(
        CommandDef {
            name: "decide",
            pattern: "decide",
            help: "Choose between options.",
            ..Default::default()
        },
        fn_handler(|ctx| Box::pin(decide(ctx))),
    );
    registry.add(
        CommandDef {
            name: "echo",
            pattern: "echo( (.*)|$)",
            help: "Repeat text back into the chat.",
            ..Default::default()
        },
        fn_handler(|ctx| Box::pin(echo(ctx))),
    );
    registry.add(
        CommandDef {
            name: "joke",
            pattern: "joke",
            help: "Get a random joke.",
            ..Default::default()
        },
        fn_handler(|ctx| Box::pin(joke(ctx))),
    );
}
