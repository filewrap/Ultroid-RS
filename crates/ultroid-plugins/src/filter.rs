//! Port of `plugins/filter.py` — per-chat word filters.
//!
//! `.addfilter <word>` (reply to a message), `.remfilter <word>`,
//! `.listfilter`. When any later message in the chat contains the word
//! (word-boundary match, case-insensitive), the stored reply is sent.
//!
//! Entries live in the core `filter` feature DB as JSON
//! `{msg, media, button}` keyed `{chat_id}:{word}`. Text replies are
//! fully real. Media capture (telegraph upload / bot file id) needs the
//! replied message's media, which the `Ctx` surface does not expose —
//! media-only replies return a `TODO(responder-op)` error (see below).
//! The watcher (`check_filter`) is a plain function: there is no
//! NewMessage-listener surface on `Registry`, so the binary's update
//! loop must call it for incoming non-command messages.

use crate::util::{def, reg_add};
use regex::Regex;
use serde::{Deserialize, Serialize};
use ultroid_core::db::features::Features;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "addfilter",
            "addfilter( (.*)|$)",
            "Add a filter: `.addfilter <word>` as a reply to a message.",
        ),
        |ctx| Box::pin(addfilter(ctx)),
    );
    reg_add(
        registry,
        def(
            "remfilter",
            "remfilter( (.*)|$)",
            "Remove a filter: `.remfilter <word>`.",
        ),
        |ctx| Box::pin(remfilter(ctx)),
    );
    reg_add(
        registry,
        def(
            "listfilter",
            "listfilter$",
            "List all filters set in this chat.",
        ),
        |ctx| Box::pin(listfilter(ctx)),
    );
}

/// One stored filter/notes/snips entry (Python `dbs.{filter,notes,snips}`
/// row: msg text, media reference, serialized inline buttons).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilterEntry {
    pub msg: Option<String>,
    pub media: Option<String>,
    pub button: Option<String>,
}

/// Split `[Label](buttonurl:...)` buttons out of message text, returning
/// (clean text, buttons JSON) — mirrors `fns.get_msg_button`.
pub fn split_buttons(text: &str) -> (String, Option<String>) {
    let re = Regex::new(r"\[([^\]]+)\]\(buttonurl:([^)]+)\)").expect("static regex");
    let mut buttons: Vec<(String, String)> = Vec::new();
    for cap in re.captures_iter(text) {
        buttons.push((cap[1].to_string(), cap[2].to_string()));
    }
    let clean = re.replace_all(text, "").trim().to_string();
    let btn = if buttons.is_empty() {
        None
    } else {
        serde_json::to_string(&buttons).ok()
    };
    (clean, btn)
}

fn key(chat_id: i64, word: &str) -> String {
    format!("{chat_id}:{word}")
}

async fn addfilter(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase();
    let Some(reply_text) = ctx.reply_to_text.clone() else {
        if ctx.reply_to_id.is_some() {
            // TODO(responder-op): download the replied message's media
            // (telegraph upload for pics/gifs/videos, bot file id otherwise)
            // (Python: filter.py:af).
            return Err(Error::Other(
                "download replied media for a filter requires a binary Responder op (Python: filter.py:af)".to_string(),
            ));
        }
        ctx.reply(&ctx.get_str("flr_1")).await?;
        return Ok(());
    };
    if word.is_empty() {
        ctx.reply(&ctx.get_str("flr_1")).await?;
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
        .filter
        .set_json(&key(ctx.chat_id, &word), &entry)
        .await?;
    ctx.reply(&ctx.get_str("flr_4").replacen("{}", &word, 1))
        .await?;
    Ok(())
}

async fn remfilter(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase();
    if word.is_empty() {
        ctx.reply(&ctx.get_str("flr_3")).await?;
        return Ok(());
    }
    let features = Features::new(ctx.db.clone());
    features.filter.del(&key(ctx.chat_id, &word)).await?;
    ctx.reply(&ctx.get_str("flr_5").replacen("{}", &word, 1))
        .await?;
    Ok(())
}

async fn listfilter(ctx: &Ctx) -> Result<()> {
    let features = Features::new(ctx.db.clone());
    let keys = features.filter.keys().await?;
    let prefix = format!("filter:{}:", ctx.chat_id);
    let mut words: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
        .collect();
    words.sort();
    if words.is_empty() {
        ctx.reply(&ctx.get_str("flr_6")).await?;
        return Ok(());
    }
    let mut out = String::from("Filters Found In This Chats Are\n\n");
    for w in words {
        out.push_str(&format!("• `{w}`\n"));
    }
    ctx.reply(&out).await?;
    Ok(())
}

/// Incoming-message watcher (Python `filter_func`): return `Ok(true)` if
/// a filter fired and was answered. Call from the binary's NewMessage
/// path for non-bot senders only — the Python version ignores bot senders.
pub async fn check_filter(ctx: &Ctx) -> Result<bool> {
    let features = Features::new(ctx.db.clone());
    let keys = features.filter.keys().await?;
    let prefix = format!("filter:{}:", ctx.chat_id);
    let text = ctx.text.to_lowercase();
    for k in keys {
        let Some(word) = k.strip_prefix(&prefix) else {
            continue;
        };
        let pat = format!(r"( |^|[^\w]){}( |$|[^\w])", regex::escape(word));
        let re = Regex::new(&pat).map_err(|e| Error::Other(e.to_string()))?;
        if !re.is_match(&text) {
            continue;
        }
        let Some(entry) = features
            .filter
            .get_json::<FilterEntry>(&key(ctx.chat_id, word))
            .await?
        else {
            continue;
        };
        if entry.button.is_some() {
            // TODO(responder-op): send the stored reply with its inline
            // buttons (Python: filter.py:filter_func, `something(...)`).
            return Err(Error::Other(
                "send filter reply with inline buttons requires a binary Responder op (Python: filter.py:filter_func)".to_string(),
            ));
        }
        if entry.media.is_some() {
            // TODO(responder-op): send the stored reply with its media
            // (telegraph URL / bot file id) (Python: filter.py:filter_func).
            return Err(Error::Other(
                "send filter reply with media requires a binary Responder op (Python: filter.py:filter_func)".to_string(),
            ));
        }
        if let Some(msg) = entry.msg {
            ctx.reply(&msg).await?;
            return Ok(true);
        }
    }
    Ok(false)
}
