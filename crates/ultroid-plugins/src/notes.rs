//! Port of `plugins/notes.py` — per-chat `#keyword` notes.
//!
//! `.addnote <word>` (reply), `.remnote <word>`, `.listnote`
//! (admins-only, like the Python version). Typing `#word` in the chat
//! sends the stored note. Same storage shape and media limitation as
//! `crate::filter`: entries live in the core `notes` feature DB as JSON
//! keyed `{chat_id}:{word}`; text notes are fully real, media capture
//! needs a `download_media` responder op (see the `TODO(responder-op)`
//! below). The watcher (`check_notes`) must be called by the binary's
//! NewMessage path — `Registry` has no listener surface.

use crate::filter::{FilterEntry, split_buttons};
use crate::util::{def, reg_add};
use ultroid_core::db::features::Features;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut add = def(
        "addnote",
        "addnote( (.*)|$)",
        "Add a note: `.addnote <word>` as a reply; use `#word` to fetch it.",
    );
    add.admins_only = true;
    reg_add(registry, add, |ctx| Box::pin(addnote(ctx)));

    let mut rem = def(
        "remnote",
        "remnote( (.*)|$)",
        "Remove a note: `.remnote <word>`.",
    );
    rem.admins_only = true;
    reg_add(registry, rem, |ctx| Box::pin(remnote(ctx)));

    let mut list = def("listnote", "listnote$", "List all notes in this chat.");
    list.admins_only = true;
    reg_add(registry, list, |ctx| Box::pin(listnote(ctx)));
}

fn key(chat_id: i64, word: &str) -> String {
    format!("{chat_id}:{word}")
}

async fn addnote(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase().replace('#', "");
    let Some(reply_text) = ctx.reply_to_text.clone() else {
        if ctx.reply_to_id.is_some() {
            // TODO(responder-op): download the replied message's media
            // (telegraph upload for pics/gifs/videos, bot file id otherwise)
            // (Python: notes.py:an).
            return Err(Error::Other(
                "download replied media for a note requires a binary Responder op (Python: notes.py:an)".to_string(),
            ));
        }
        ctx.reply(&ctx.get_str("notes_1")).await?;
        return Ok(());
    };
    if word.is_empty() {
        ctx.reply(&ctx.get_str("notes_1")).await?;
        return Ok(());
    }
    let (txt, btn) = split_buttons(&reply_text);
    let entry = FilterEntry {
        msg: if txt.is_empty() { None } else { Some(txt) },
        media: None,
        button: btn,
    };
    let features = Features::new(ctx.db.clone());
    features
        .notes
        .set_json(&key(ctx.chat_id, &word), &entry)
        .await?;
    ctx.reply(&ctx.get_str("notes_2").replacen("{}", &word, 1))
        .await?;
    Ok(())
}

async fn remnote(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase().replace('#', "");
    if word.is_empty() {
        ctx.reply(&ctx.get_str("notes_3")).await?;
        return Ok(());
    }
    let features = Features::new(ctx.db.clone());
    features.notes.del(&key(ctx.chat_id, &word)).await?;
    ctx.reply(&format!("Done Note: `#{word}` Removed.")).await?;
    Ok(())
}

async fn listnote(ctx: &Ctx) -> Result<()> {
    let features = Features::new(ctx.db.clone());
    let keys = features.notes.keys().await?;
    let prefix = format!("notes:{}:", ctx.chat_id);
    let mut words: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
        .collect();
    words.sort();
    if words.is_empty() {
        ctx.reply(&ctx.get_str("notes_5")).await?;
        return Ok(());
    }
    let mut out = String::from("Notes Found In This Chats Are\n\n");
    for w in words {
        out.push_str(&format!("• `{w}`\n"));
    }
    ctx.reply(&out).await?;
    Ok(())
}

/// Incoming-message watcher (Python `notes`): looks for `#word` tokens
/// and answers from the stored note. Returns `Ok(true)` when answered.
pub async fn check_notes(ctx: &Ctx) -> Result<bool> {
    let features = Features::new(ctx.db.clone());
    let words: Vec<String> = ctx
        .text
        .to_lowercase()
        .split_whitespace()
        .filter(|w| w.starts_with('#'))
        .map(|w| w.trim_start_matches('#').to_string())
        .collect();
    for word in words {
        let Some(entry) = features
            .notes
            .get_json::<FilterEntry>(&key(ctx.chat_id, &word))
            .await?
        else {
            continue;
        };
        if entry.button.is_some() {
            // TODO(responder-op): send the stored note with its inline
            // buttons (Python: notes.py:notes, `something(...)`).
            return Err(Error::Other(
                "send note with inline buttons requires a binary Responder op (Python: notes.py:notes)".to_string(),
            ));
        }
        if entry.media.is_some() {
            // TODO(responder-op): send the stored note with its media
            // (Python: notes.py:notes).
            return Err(Error::Other(
                "send note with media requires a binary Responder op (Python: notes.py:notes)"
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
