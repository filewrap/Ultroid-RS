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
use ultroid_core::tg::InlineButton;
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

/// Parse stored filter/notes/snips button JSON — `split_buttons` stores
/// a flat `[[label, url], …]` list — into markup rows (one button per
/// row), for `Responder::send_markup_to`.
pub fn buttons_from_json(button_json: &str) -> Vec<Vec<InlineButton>> {
    let pairs: Vec<(String, String)> = serde_json::from_str(button_json).unwrap_or_default();
    pairs
        .into_iter()
        .map(|(label, url)| vec![InlineButton::url(label, url)])
        .collect()
}

/// Send a stored entry's text with its inline buttons, when it has any.
/// Returns `Ok(true)` when a buttoned send happened.
pub async fn send_with_buttons(ctx: &Ctx, entry: &FilterEntry) -> Result<bool> {
    let Some(button_json) = entry.button.as_deref() else {
        return Ok(false);
    };
    let text = entry.msg.clone().unwrap_or_default();
    let rows = buttons_from_json(button_json);
    ctx.responder
        .send_markup_to(ctx.chat_id, &text, rows)
        .await?;
    Ok(true)
}

fn key(chat_id: i64, word: &str) -> String {
    format!("{chat_id}:{word}")
}

async fn addfilter(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_lowercase();
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
                            "download replied media for a filter requires a binary Responder op (media kind + Telegraph upload / bot file id) (Python: filter.py:af)".to_string(),
                        ));
                    }
                    _ => None,
                }
            }
            None => None,
        },
    };
    let Some(reply_text) = reply_text else {
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
            // Stored reply with inline buttons — delivered directly with
            // `send_markup_to` (Python `something(...)` goes through the
            // assistant inline flow, whose `STUFF` entry plugins cannot
            // register; the visible result is the same).
            if send_with_buttons(ctx, &entry).await? {
                return Ok(true);
            }
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
