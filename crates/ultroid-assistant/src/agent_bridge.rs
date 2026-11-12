//! Routes the user's answer back to whatever the agent is waiting on.
//!
//! Port of `assistant/agent_bridge.py` — the bot half of the ask
//! bridge. The tool half lives in `ultroid-agent` (`interactive`):
//! it puts a styled keyboard on the Bot; a tap on that keyboard and
//! a typed reply both have to arrive back here to settle the future
//! the tool is blocked on.
//!
//! Two handlers, both deliberately unfiltered:
//! * every CallbackQuery is offered to the bridge, which claims
//!   only the data it minted (`ask:<chat_id>:<token>...`) and
//!   ignores the rest — the assistant's own menu buttons share the
//!   same dispatcher, so this must be a filter, not a pattern;
//! * every private inbound text message is offered as a typed
//!   answer to the oldest open question in that chat, and is
//!   otherwise left completely alone, so the PM bot and the session
//!   threads keep owning their own flows.
//!
//! The client note matters: Telegram does not deliver a callback
//! query for a message the *User* account sent itself. That is why
//! the question goes out on the Bot, and why this file binds the bot.
//!
//! Contract limit, stated plainly: the bridge state itself — the
//! pending questions, their oneshot channels, the TTL watchdog —
//! lives in `ultroid-agent`'s `AskBridge` (`shared_bridge()`), and
//! this crate does not depend on `ultroid-agent`. The ownership
//! test is ported exactly (the `ask:<chat_id>:` prefix from
//! `ultroid-agent/src/bridge.rs`), and `on_ask_callback` /
//! `on_ask_reply` are the real entry points the binary crate calls;
//! settling a question returns an error naming the missing
//! `AskBridge::resolve_callback` / `submit_text` call instead of
//! pretending the answer was delivered. Add `ultroid-agent` to this
//! crate's dependencies and call the shared bridge from these two
//! functions to complete the loop; the prefix rule and the
//! delete-the-keyboard-after-settling behaviour are already here.

use ultroid_core::error::{Error, Result};
use ultroid_core::{Ctx, Registry};

/// Callback-data prefix for ask buttons in one chat (Python bridge
/// `answer_prefix(chat_id)` → `ask:<chat_id>:`).
pub fn answer_prefix(chat_id: i64) -> String {
    format!("ask:{chat_id}:")
}

/// Does this callback data belong to the ask bridge for this chat?
/// The bridge claims only data it minted and ignores the rest.
pub fn owns(data: &str, chat_id: i64) -> bool {
    data.starts_with(&answer_prefix(chat_id))
}

/// Settle the question whose button was tapped (Python
/// `on_ask_callback`). Returns `Ok(true)` when the data was claimed.
/// On success the keyboard's message is deleted, so a second tap
/// cannot re-answer a settled question.
pub async fn on_ask_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if data.is_empty() || !owns(data, ctx.chat_id) {
        return Ok(false);
    }
    // The question is settled by the shared AskBridge; that state
    // lives in ultroid-agent, not in this crate.
    let _ = ctx.delete(ctx.message_id).await;
    Err(Error::Other(
        "ask bridge settle needs ultroid-agent AskBridge::resolve_callback(data) — the pending-question futures live in the shared bridge, which is not a dependency of this crate".to_string(),
    ))
}

/// Settle the oldest open question in this chat with typed text
/// (Python `on_ask_reply`). Returns `Ok(true)` when a question was
/// answered. Private text only; anything else is left alone.
pub async fn on_ask_reply(ctx: &mut Ctx) -> Result<bool> {
    if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
        return Ok(false);
    }
    let text = ctx.text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    Err(Error::Other(
        "ask bridge text answer needs ultroid-agent AskBridge::submit_text(chat_id, text) — the pending-question futures live in the shared bridge, which is not a dependency of this crate".to_string(),
    ))
}

/// Register the bridge handlers. There is nothing to register in a
/// command `Registry` — both handlers are unfiltered event filters
/// (every callback query, every private message) that the binary
/// crate must call before its other handlers, exactly as Python
/// attaches them with `asst.add_event_handler`.
pub fn register(_registry: &mut Registry) {
    // Intentionally empty: see the doc comment above.
}
