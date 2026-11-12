//! AFK — port of `plugins/afk.py` (`.afk`).
//!
//! State shape is kept identical to Python so the key stays compatible:
//! udB key `AFK_DB` holds JSON `[message, media_type, media, time]` where
//! `time` is `"%b %d %Y %I:%M:%S%p"` (chrono formats/parses the same
//! pattern). `is_afk()` returns `(text, media_type, media, afk_since)` with
//! `afk_since` as a Python-`timedelta`-style string.
//!
//! What cannot be expressed through `Ctx` in this wave:
//! * Replied media: `Ctx` has no replied-message media handle, so a
//!   media AFK (Catbox upload of a replied photo/gif, sticker file id)
//!   cannot be set — text-only AFKs are stored, and a reply with media
//!   is noted in the log instead of silently pretending it worked.
//! * The `remove_afk` / `on_afk` auto-reply handlers are `NewMessage`
//!   event handlers, not commands; there is no event-handler registration
//!   surface in the plugin crate, so they cannot be installed here. (In
//!   Python `.afk` installs them and they are also installed at import
//!   time when `AFK_DB` is already set.)
//! * The log-channel copy (`asst.send_message(LOG_CHANNEL, …)`) goes out
//!   through the assistant responder when connected, and is skipped with
//!   a debug log when it is not.

use crate::util::reg_add;
use chrono::{NaiveDateTime, Utc};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

pub const AFK_TIME_FMT: &str = "%b %d %Y %I:%M:%S%p";

/// The stored AFK row: `[message, media_type, media, time]` (Python
/// `afk_db.AfkDB.add`).
pub type AfkRow = (Option<String>, Option<String>, Option<String>, String);

/// Format a `chrono::Duration` the way Python's `str(timedelta)` does:
/// `"1 day, 2:03:04"` or `"2:03:04"`.
fn fmt_timedelta(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    if days > 0 {
        format!(
            "{days} day{}, {h}:{m:02}:{s:02}",
            if days == 1 { "" } else { "s" }
        )
    } else {
        format!("{h}:{m:02}:{s:02}")
    }
}

/// Port of `afk_db.AfkDB.is_afk()`: `(text, media_type, media, afk_since)`.
pub async fn is_afk(
    ctx: &Ctx,
) -> Result<Option<(Option<String>, Option<String>, Option<String>, String)>> {
    let row: Option<AfkRow> = db_get_json(&*ctx.db, "AFK_DB").await?;
    let Some((text, media_type, media, time)) = row else {
        return Ok(None);
    };
    let since = match NaiveDateTime::parse_from_str(&time, AFK_TIME_FMT) {
        Ok(t) => {
            let now = Utc::now().naive_utc();
            fmt_timedelta(now - t)
        }
        Err(_) => time.clone(),
    };
    Ok(Some((text, media_type, media, since)))
}

/// `.afk [reason]` — port of `afk.py::set_afk`.
async fn set_afk(ctx: &Ctx) -> Result<()> {
    if is_afk(ctx).await?.is_some() {
        return Ok(());
    }
    let mut text = ctx.args.trim().to_string();
    if text.is_empty() {
        text = ctx.reply_to_text.clone().unwrap_or_default();
    }
    if ctx.reply_to_id.is_some() && ctx.reply_to_text.is_none() {
        // A reply whose message is media-only (or otherwise has no text):
        // Python would download the media and Catbox-upload it. That needs
        // a replied-media download op.
        tracing::debug!("afk: replied message has no text; media AFK needs a Responder media op");
    }
    let time = Utc::now().format(AFK_TIME_FMT).to_string();
    let row: AfkRow = (
        if text.is_empty() {
            None
        } else {
            Some(text.clone())
        },
        None,
        None,
        time,
    );
    db_set_json(&*ctx.db, "AFK_DB", &row).await?;

    // Remove-afk / on-afk event handlers cannot be installed from here
    // (no NewMessage-handler surface in this wave); the state above is
    // what the dispatcher-side AFK hook must read.

    let msg = if text.is_empty() {
        ctx.get_str("afk_6")
    } else {
        ctx.get_str("afk_5").replace("{}", &text)
    };
    let id = ctx.reply(&msg).await?;
    if ctx.config.log_channel != 0 {
        // Log-channel copy via the assistant bot (Python:
        // afk.py:set_afk -> asst.send_message(LOG_CHANNEL, …)).
        match &ctx.asst {
            Some(asst) => {
                asst.send_to(ctx.config.log_channel, &msg).await?;
            }
            None => {
                tracing::debug!(
                    log_channel = ctx.config.log_channel,
                    afk_msg_id = id,
                    "afk: assistant not connected; log-channel copy skipped"
                );
            }
        }
    }
    Ok(())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "afk",
            pattern: "afk( (.*)|$)",
            help: "Set yourself AFK; people who tag or message you get an automated reply.",
            owner_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(set_afk(ctx)),
    );
}
