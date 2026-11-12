//! Sticker pack manager (`kang` / `listpack`).
//!
//! Port of `assistant/manager/stickermanager.py`.
//!
//! BLOCKED — the commands themselves need raw Telegram media/sticker
//! calls that the core surface does not expose, and per the porting rules
//! they are NOT registered as stubs. What `kang` needs and this crate
//! cannot do:
//!
//! * `reply.download_media()` — no media-download surface on
//!   [`ultroid_core::Ctx`] / [`ultroid_core::Responder`].
//! * `TgConverter.resize_photo_sticker` and `Quotly().create_quotly` — no
//!   image/quote helpers in this crate.
//! * `client.upload_file` + `UploadMediaRequest` — no upload surface.
//! * `GetStickerSetRequest`, `CreateStickerSetRequest`,
//!   `AddStickerToSetRequest` (+ `get_input_document`) — raw TL sticker
//!   calls; grammers types are forbidden in this crate.
//! * `asst.me.username` for the `_by_{username}` pack suffix and
//!   `sender.username` for the pack prefix — no usernames on
//!   [`ultroid_core::Ctx`] / [`ultroid_core::Config`].
//! * `packs_inline` (`@in_pattern("packs", owner=True)`) needs the
//!   InlineQuery surface and `InlineResultBuilder` — neither exists in
//!   core (`Registry` dispatches message commands only).
//!
//! Ported for real: the `STICKERS` DB bookkeeping
//! ([`owned_pack_names`], [`stored_packs`]) and the pack short-name /
//! title construction ([`pack_short_name`], [`overflow_pack_short_name`],
//! [`pack_title`]), which the binary can use once the sticker calls
//! exist.

use std::collections::HashMap;

use ultroid_core::db::{Database, db_get_json};
use ultroid_core::error::Result;
use ultroid_core::router::Registry;

/// The DB key holding every user's kanged packs (Python `udB.get_key(
/// "STICKERS")`): sender id -> kind (`"static"` / `"anim"` / `"vid"`) ->
/// pack short-names, newest last.
pub const STICKERS_KEY: &str = "STICKERS";

/// Sticker pack kinds (Python `type_`: `"static"` / `"anim"` / `"vid"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    Static,
    Anim,
    Vid,
}

impl PackKind {
    /// DB key for the kind.
    pub fn key(self) -> &'static str {
        match self {
            PackKind::Static => "static",
            PackKind::Anim => "anim",
            PackKind::Vid => "vid",
        }
    }

    /// Short-name suffix for the kind (Python `sn += "_anim"` / `"_vid"`).
    pub fn short_name_suffix(self) -> &'static str {
        match self {
            PackKind::Static => "",
            PackKind::Anim => "_anim",
            PackKind::Vid => "_vid",
        }
    }

    /// Title suffix for the kind (Python `title += " (Animated)"` /
    /// `"(Video)"` — note the video branch has no space before the paren).
    pub fn title_suffix(self) -> &'static str {
        match self {
            PackKind::Static => "",
            PackKind::Anim => " (Animated)",
            PackKind::Vid => "(Video)",
        }
    }
}

/// All stored packs for every user (Python `udB.get_key("STICKERS") or
/// {}`).
pub async fn stored_packs(
    db: &dyn Database,
) -> Result<HashMap<String, HashMap<String, Vec<String>>>> {
    Ok(db_get_json(db, STICKERS_KEY).await?.unwrap_or_default())
}

/// Flat list of every pack short-name one user has kanged so far (Python
/// `_owned_pack_names`).
pub async fn owned_pack_names(db: &dyn Database, sender_id: i64) -> Result<Vec<String>> {
    let packs = stored_packs(db).await?;
    let mut names = Vec::new();
    if let Some(owned) = packs.get(&sender_id.to_string()) {
        for kind in owned.values() {
            names.extend(kind.iter().cloned());
        }
    }
    Ok(names)
}

/// Short name for a freshly created pack (Python `kang_cmd`):
/// `{pre}_{sender_id}[_anim|_vid]_by_{bot_username}`.
pub fn pack_short_name(pre: &str, sender_id: i64, kind: PackKind, bot_username: &str) -> String {
    format!(
        "{pre}_{sender_id}{}_by_{bot_username}",
        kind.short_name_suffix()
    )
}

/// Short name for the overflow pack created when a pack is full (Python:
/// `sn = f"{pre}{sender_id}_{ult.id}"` — note: no underscore between the
/// prefix and the sender id, unlike first creation — then the kind suffix
/// and `_by_{bot_username}`).
pub fn overflow_pack_short_name(
    pre: &str,
    sender_id: i64,
    message_id: i64,
    kind: PackKind,
    bot_username: &str,
) -> String {
    format!(
        "{pre}{sender_id}_{message_id}{}_by_{bot_username}",
        kind.short_name_suffix()
    )
}

/// Title for a freshly created pack (Python: `f"{display_name}'s Kang
/// Pack"` plus the kind suffix).
pub fn pack_title(display_name: &str, kind: PackKind) -> String {
    format!("{display_name}'s Kang Pack{}", kind.title_suffix())
}

/// The `t.me/addstickers/{short_name}` link for a pack.
pub fn pack_link(short_name: &str) -> String {
    format!("https://t.me/addstickers/{short_name}")
}

/// Nothing to register: `kang`, `listpack` and the `packs` inline handler
/// all need the raw sticker/media/inline calls listed in the module docs.
pub fn register(_registry: &mut Registry) {}
