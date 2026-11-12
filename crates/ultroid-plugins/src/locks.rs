//! Chat default-rights locks — port of `plugins/locks.py`.
//!
//! `.lock` / `.unlock` share one handler whose pattern is
//! `(un|)lock( (.*)|$)` (group 1 is `""` for lock, `"un"` for unlock;
//! group 2 is the setting name).
//!
//! The setting -> `ChatBannedRights` mapping is ported faithfully from
//! `fns.lock_unlock` (`pyUltroid/fns/admins.py`), including its quirks
//! (`msgs` sets `send_messages`, `invite_users` and the misspelled
//! `pin_messageschange_info` attribute — the last of which is a no-op
//! field name in Python and is therefore dropped here with a note).
//! The `EditChatDefaultBannedRightsRequest` RPC itself is a
//! `TODO(responder-op)` site.

use crate::util::{def, reg_add};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}lock <msgs/media/sticker/gif/games/inline/polls/invites/pin/changeinfo>`\n    Lock the Used Setting in Used Group.\n\n• `{i}unlock <msgs/media/sticker/gif/games/inline/polls/invites/pin/changeinfo>`\n    UNLOCK the Used Setting in Used Group.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "lock",
            pattern: "(un|)lock( (.*)|$)",
            help: HELP,
            admins_only: true,
            ..def("lock", "(un|)lock( (.*)|$)", HELP)
        },
        |ctx| Box::pin(un_lock(ctx)),
    );
}

/// The banned-rights fields a lock query touches (port of
/// `fns.lock_unlock`). `None` = unknown query (Python returns `None`).
/// Fields are the TL `ChatBannedRights` names.
pub fn lock_unlock_fields(query: &str) -> Option<Vec<&'static str>> {
    match query {
        // Python also sets the misspelled attribute `pin_messageschange_info`
        // on the rights object; Telethon ignores unknown attributes, so it
        // has no effect — only the two real fields are listed here.
        "msgs" => Some(vec!["send_messages", "invite_users"]),
        "media" => Some(vec!["send_media"]),
        "sticker" => Some(vec!["send_stickers"]),
        "gif" => Some(vec!["send_gifs"]),
        "games" => Some(vec!["send_games"]),
        "inline" => Some(vec!["send_inline"]),
        "polls" => Some(vec!["send_polls"]),
        "invites" => Some(vec!["invite_users"]),
        "pin" => Some(vec!["pin_messages"]),
        "changeinfo" => Some(vec!["change_info"]),
        _ => None,
    }
}

async fn un_lock(ctx: &Ctx) -> Result<()> {
    // Pattern groups: 1 = "" | "un", 2 = " <setting>" (with leading space).
    let is_unlock = ctx.captures.get(1).map(|s| s.as_str()) == Some("un");
    let setting = ctx
        .captures
        .get(2)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if setting.is_empty() {
        ctx.reply("`Give some Proper Input..`").await?;
        return Ok(());
    }
    let lock = !is_unlock;
    let Some(fields) = lock_unlock_fields(&setting) else {
        ctx.reply("`Incorrect Input`").await?;
        return Ok(());
    };
    let _ = fields;
    let verb = if lock { "Locked" } else { "Unlocked" };
    // TODO(responder-op): messages.EditChatDefaultBannedRightsRequest(
    // chat, banned_rights=ChatBannedRights(...fields = lock...)).
    // Python: plugins/locks.py:un_lock.
    let _ = verb;
    Err(Error::Other(
        "edit chat default banned rights requires a binary Responder op (Python: plugins/locks.py:un_lock)".into(),
    ))
}
