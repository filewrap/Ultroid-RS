//! Greeting sent when the assistant bot is added to a group.
//!
//! Port of `assistant/manager/_on_adds.py` (`dueha`).
//!
//! The decision logic is ported in full as [`on_add_message`] /
//! [`on_add_rich_doc`]: read the `ON_MNGR_ADD` DB key; disabled when the
//! key is `false` or a string reading `off` / `false` / `0`; the default
//! text is `"Thanks for adding me."`; anything else is sent verbatim.
//!
//! BLOCKED — no event hook: Python registers this on
//! `events.ChatAction(func=lambda x: x.user_added)` and replies only when
//! the added user `is_self`. There is no ChatAction / NewMessage watcher
//! registry in `ultroid-core` (`Registry` dispatches message commands
//! only), so nothing in this crate can fire the greeting. The binary's
//! update loop must call [`on_add_rich_doc`] when it sees the assistant
//! bot itself added to a chat and send the returned document.

use ultroid_core::db::Database;
use ultroid_core::error::Result;
use ultroid_core::rich::RichDoc;
use ultroid_core::router::Registry;

/// Default greeting when `ON_MNGR_ADD` is unset (Python: `sm =
/// "Thanks for adding me."`).
pub const DEFAULT_ON_ADD_MESSAGE: &str = "Thanks for adding me.";

/// Resolve the greeting text from the `ON_MNGR_ADD` DB key.
/// Returns `None` when the greeting is disabled (`false`, or a string
/// reading `off` / `false` / `0`, case-insensitive, ignoring surrounding
/// quotes/whitespace), `Some(default)` when the key is unset or empty,
/// and `Some(custom)` otherwise.
pub async fn on_add_message(db: &dyn Database) -> Result<Option<String>> {
    match db.get("ON_MNGR_ADD").await? {
        None => Ok(Some(DEFAULT_ON_ADD_MESSAGE.to_string())),
        Some(raw) => {
            let value = raw.trim().trim_matches('"').trim();
            if value.is_empty() {
                return Ok(Some(DEFAULT_ON_ADD_MESSAGE.to_string()));
            }
            if value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("off")
                || value == "0"
            {
                return Ok(None);
            }
            Ok(Some(value.to_string()))
        }
    }
}

/// The greeting as a rich document (Python: `RichBlocks().paragraph(sm)`),
/// or `None` when disabled.
pub async fn on_add_rich_doc(db: &dyn Database) -> Result<Option<RichDoc>> {
    match on_add_message(db).await? {
        Some(text) => Ok(Some(RichDoc::markdown(text)?)),
        None => Ok(None),
    }
}

/// No command to register: the greeting fires on a ChatAction event, not
/// on a command — see the module docs.
pub fn register(_registry: &mut Registry) {}
