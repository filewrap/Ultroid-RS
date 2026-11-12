//! Port of `plugins/snips.py` — global `$word` snips.
//!
//! `.addsnip <word>` (reply), `.remsnip <word>`, `.listsnip`. Typing
//! `$word` in one of your own (or a sudoer's) messages sends the stored
//! snip. Entries live in the core `snips` feature DB as JSON keyed by
//! word, sharing the entry shape of `crate::filter`. Text snips are
//! fully real; media capture needs a `download_media` responder op (see
//! the `TODO(responder-op)` below). The watcher (`check_snips`) must be
//! called by the binary's NewMessage path for outgoing messages only —
//! `Registry` has no listener surface.

use crate::filter::{FilterEntry, split_buttons};
use crate::util::{def, reg_add};
use ultroid_core::db::features::Features;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "addsnip",
            "addsnip( (.*)|$)",
            "Add a snip: `.addsnip <word>` as a reply; type `$word` to fetch it.",
        ),
        |ctx| Box::pin(addsnip(ctx)),
    );
    reg_add(
        registry,
        def(
            "remsnip",
            "remsnip( (.*)|$)",
            "Remove a snip: `.remsnip <word>`.",
        ),
        |ctx| Box::pin(remsnip(ctx)),
    );
    reg_add(
        registry,
        def("listsnip", "listsnip", "List all snips."),
        |ctx| Box::pin(listsnip(ctx)),
    );
}

async fn addsnip(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase().replace('$', "");
    // The reply text is on `Ctx` when the binary filled it; otherwise
    // re-fetch the reply through `message_summary`. A media-only reply
    // still needs the media-kind branch (Telegraph upload for
    // pics/gifs/videos, bot file id otherwise), which no trait op
    // provides, so it stays an honest error.
    let reply_text: Option<String> = match ctx.reply_to_text.clone() {
        Some(text) => Some(text),
        None => match ctx.reply_to_id {
            Some(reply_id) => {
                match ctx
                    .responder
                    .message_summary(ctx.chat_id, reply_id as i32)
                    .await?
                {
                    Some(summary) if !summary.text.is_empty() => Some(summary.text),
                    Some(summary) if summary.has_media => {
                        return Err(Error::Other(
                            "download replied media for a snip requires a binary Responder op (media kind + Telegraph upload / bot file id) (Python: snips.py:an)".to_string(),
                        ));
                    }
                    _ => None,
                }
            }
            None => None,
        },
    };
    let Some(reply_text) = reply_text else {
        ctx.reply(&ctx.get_str("snip_1")).await?;
        return Ok(());
    };
    if word.is_empty() {
        ctx.reply(&ctx.get_str("snip_1")).await?;
        return Ok(());
    }
    let (txt, btn) = split_buttons(&reply_text);
    let entry = FilterEntry {
        msg: if txt.is_empty() { None } else { Some(txt) },
        media: None,
        button: btn,
    };
    let features = Features::new(ctx.db.clone());
    features.snips.set_json(&word, &entry).await?;
    ctx.reply(&format!("Done : snip `${word}` Saved.")).await?;
    Ok(())
}

async fn remsnip(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase().replace('$', "");
    if word.is_empty() {
        ctx.reply(&ctx.get_str("snip_2")).await?;
        return Ok(());
    }
    let features = Features::new(ctx.db.clone());
    features.snips.del(&word).await?;
    ctx.reply(&format!("Done : snip `${word}` Removed."))
        .await?;
    Ok(())
}

async fn listsnip(ctx: &Ctx) -> Result<()> {
    let features = Features::new(ctx.db.clone());
    let keys = features.snips.keys().await?;
    let mut words: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix("snips:").map(str::to_string))
        .collect();
    words.sort();
    if words.is_empty() {
        ctx.reply("No Snips Found Here").await?;
        return Ok(());
    }
    let mut out = String::from("SNIPS Found :\n\n");
    for w in words {
        out.push_str(&format!("• `${w}`\n"));
    }
    ctx.reply(&out).await?;
    Ok(())
}

/// Incoming-message watcher (Python `add_snips`): looks for `$word`
/// tokens in an *outgoing* (own/sudo) message and answers from the
/// stored snip. Returns `Ok(true)` when answered. The `$word` token is
/// answered in place; the Python version deletes the trigger message
/// first — that delete is a `Responder::delete` of the trigger message,
/// which the binary can do before calling this.
pub async fn check_snips(ctx: &Ctx) -> Result<bool> {
    let features = Features::new(ctx.db.clone());
    let words: Vec<String> = ctx
        .text
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.starts_with('$'))
        .map(|w| w.trim_start_matches('$').to_string())
        .collect();
    for word in words {
        let Some(entry) = features.snips.get_json::<FilterEntry>(&word).await? else {
            continue;
        };
        if entry.button.is_some() {
            // Stored snip with inline buttons — delivered directly with
            // `send_markup_to` (see filter.rs `check_filter`).
            if crate::filter::send_with_buttons(ctx, &entry).await? {
                return Ok(true);
            }
        }
        if entry.media.is_some() {
            // TODO(responder-op): send the stored snip with its media
            // (Python: snips.py:add_snips).
            return Err(Error::Other(
                "send snip with media requires a binary Responder op (Python: snips.py:add_snips)"
                    .to_string(),
            ));
        }
        if let Some(msg) = entry.msg {
            ctx.reply(&msg).await?;
            return Ok(true);
        }
    }
    Ok(false)
}
