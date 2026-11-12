//! Group admin commands and the admin-verification callback.
//!
//! Port of `assistant/manager/admins.py`.
//!
//! BLOCKED — both behaviours in this file need Telegram calls that the
//! core surface does not expose, and per the porting rules they are NOT
//! registered as stubs:
//!
//! * `d(kick|ban)` (`@ultroid_cmd(pattern="d(kick|ban)", manager=True,
//!   require="ban_users")`, handler `dowj`): needs
//!   `client.kick_participant(chat_id, user)` for kick and
//!   `client.edit_permissions(chat_id, user, view_messages=False)` for
//!   ban, plus deleting the replied message. [`ultroid_core::Responder`]
//!   has `delete(msg_id)` but no kick / ban / edit-permissions method, and
//!   [`ultroid_core::Ctx`] carries no client handle. The reply-target
//!   validation ("Reply to a message...") and the kick-vs-ban wording
//!   ("Kicked Successfully!" / "Banned Successfully!") are preserved below
//!   as [`AdminAction`] and [`action_result_text`] for the binary to use
//!   once an admin-action surface exists.
//! * The `assistant:admins:admin_verify` callback (`callback_`): needs a
//!   CallbackQuery dispatch surface (none exists in `Registry`), plus
//!   `client.get_permissions(chat_id, sender_id)` and the process-wide
//!   `_ult_cache["admin_callback"]` registrations (no equivalent in core).
//!   The portable halves — parsing the callback data and checking whether
//!   a verification is still pending — are ported as
//!   [`parse_verify_data`] and [`is_verification_pending`]; the denial
//!   strings ("This verification request is invalid.",
//!   "This verification request has expired.", "Join the Group First!",
//!   "You are not an Admin!", "Verification Done!") are preserved as
//!   constants.

use std::collections::HashSet;
use ultroid_core::router::Registry;

/// Kick or ban, selected by group 1 of the `d(kick|ban)` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    Kick,
    Ban,
}

/// Parse group 1 of the `d(kick|ban)` pattern (`"kick"` / `"ban"`).
pub fn parse_admin_action(group1: &str) -> Option<AdminAction> {
    match group1.trim() {
        "kick" => Some(AdminAction::Kick),
        "ban" => Some(AdminAction::Ban),
        _ => None,
    }
}

/// The success reply for the action (Python: `f"{te} Successfully!"`,
/// where `te` is `"Kicked"` / `"Banned"`).
pub fn action_result_text(action: AdminAction) -> &'static str {
    match action {
        AdminAction::Kick => "Kicked Successfully!",
        AdminAction::Ban => "Banned Successfully!",
    }
}

/// The reply when the command is not used on a replied message (Python:
/// `e.eor("Reply to a message...")`).
pub const NEED_REPLY: &str = "Reply to a message...";

/// Callback-data prefix for the admin verification button (Python:
/// `cb_pattern("assistant", "admins", "admin_verify")`).
pub const ADMIN_VERIFY_PREFIX: &str = "assistant:admins:admin_verify:";

/// Callback answers from the verification handler, verbatim.
pub const VERIFY_INVALID: &str = "This verification request is invalid.";
pub const VERIFY_EXPIRED: &str = "This verification request has expired.";
pub const VERIFY_JOIN_FIRST: &str = "Join the Group First!";
pub const VERIFY_NOT_ADMIN: &str = "You are not an Admin!";
pub const VERIFY_DONE: &str = "Verification Done!";

/// Parse the verification token out of the callback data (Python:
/// `event.data_match.group(1).decode("utf-8")`). Returns `None` when the
/// data is not under the verify prefix or is not valid UTF-8.
pub fn parse_verify_data(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let token = text.strip_prefix(ADMIN_VERIFY_PREFIX)?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// Whether a verification token is still pending (Python:
/// `_verification_pending`: the registrations map is a dict and the token
/// is a key in it).
pub fn is_verification_pending(token: &str, registrations: &HashSet<String>) -> bool {
    registrations.contains(token)
}

/// Nothing to register: the command needs kick/ban client calls and the
/// callback needs CallbackQuery dispatch + `get_permissions` — none of
/// which exist on the core surface. See the module docs.
pub fn register(_registry: &mut Registry) {}
