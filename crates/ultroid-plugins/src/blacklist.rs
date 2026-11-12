//! Per-chat word blacklist — port of `plugins/blacklist.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/blacklist_db.py`):
//! `BLACKLIST_DB` maps chat id -> list of lowercased words.
//!
//! `.blacklist` / `.remblacklist` / `.listblacklist` are fully ported.
//! The live watcher (`NewMessage` listener that deletes messages
//! containing a blacklisted word) is event-driven: its match predicate is
//! ported as a pure function (`contains_blacklisted`) for the binary's
//! update loop, and the delete is wired to `delete_from`.

use crate::util::{def, reg_add};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

pub const HELP: &str = " -\n\n- `{i}blacklist <word/all words with a space>`\n    blacklist the choosen word in that chat.\n\n- `{i}remblacklist <word>`\n    Remove the word from blacklist..\n\n- `{i}listblacklist`\n    list all blacklisted words.\n\n  'if a person uses blacklist Word his/her msg will be deleted'\n  'And u Must be Admin in that Chat'\n";

pub fn register(registry: &mut Registry) {
    let admin = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        admins_only: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, admin("blacklist", "blacklist( (.*)|$)"), |ctx| {
        Box::pin(add_blacklist(ctx))
    });
    reg_add(
        registry,
        admin("remblacklist", "remblacklist( (.*)|$)"),
        |ctx| Box::pin(remove_blacklist(ctx)),
    );
    reg_add(registry, admin("listblacklist", "listblacklist$"), |ctx| {
        Box::pin(list_blacklist(ctx))
    });
}

/// All blacklists (Python `dbs.blacklist.get_stuff()`).
pub async fn get_blacklists(ctx: &Ctx) -> Result<HashMap<String, Vec<String>>> {
    Ok(db_get_json(&*ctx.db, "BLACKLIST_DB")
        .await?
        .unwrap_or_default())
}

async fn set_blacklists(ctx: &Ctx, map: &HashMap<String, Vec<String>>) -> Result<()> {
    db_set_json(&*ctx.db, "BLACKLIST_DB", map).await
}

/// Blacklisted words for one chat (Python `dbs.blacklist.get`).
pub async fn words_for_chat(ctx: &Ctx, chat_id: i64) -> Result<Vec<String>> {
    Ok(get_blacklists(ctx)
        .await?
        .get(&chat_id.to_string())
        .cloned()
        .unwrap_or_default())
}

/// Word-listing text (Python `dbs.blacklist.list`): `👉`word`` per line.
pub fn list_text(words: &[String]) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    let mut txt = String::new();
    for w in words {
        txt.push_str(&format!("👉`{w}`\n"));
    }
    Some(txt)
}

async fn add_blacklist(ctx: &Ctx) -> Result<()> {
    // Python takes `e.text[11:]` (everything after ".blacklist ") verbatim.
    let word_arg = ctx.text.splitn(2, ' ').nth(1).map(str::trim).unwrap_or("");
    if word_arg.is_empty() {
        ctx.reply(&ctx.get_str("blk_1")).await?;
        return Ok(());
    }
    let mut map = get_blacklists(ctx).await?;
    let entry = map.entry(ctx.chat_id.to_string()).or_default();
    for word in word_arg.split(' ') {
        let w = word.to_lowercase();
        if !entry.contains(&w) {
            entry.push(w);
        }
    }
    set_blacklists(ctx, &map).await?;
    ctx.reply(&fmt_args(&ctx.get_str("blk_2"), &[word_arg]))
        .await?;
    Ok(())
}

async fn remove_blacklist(ctx: &Ctx) -> Result<()> {
    // Python takes `e.text[14:]` (everything after ".remblacklist ").
    let word_arg = ctx.text.splitn(2, ' ').nth(1).map(str::trim).unwrap_or("");
    if word_arg.is_empty() {
        ctx.reply(&ctx.get_str("blk_3")).await?;
        return Ok(());
    }
    let mut map = get_blacklists(ctx).await?;
    if let Some(entry) = map.get_mut(&ctx.chat_id.to_string()) {
        for word in word_arg.split(' ') {
            let w = word.to_lowercase();
            entry.retain(|x| *x != w);
        }
    }
    set_blacklists(ctx, &map).await?;
    ctx.reply(&fmt_args(&ctx.get_str("blk_4"), &[word_arg]))
        .await?;
    Ok(())
}

async fn list_blacklist(ctx: &Ctx) -> Result<()> {
    let words = words_for_chat(ctx, ctx.chat_id).await?;
    match list_text(&words) {
        Some(txt) => {
            ctx.reply(&format!("{}{}", ctx.get_str("blk_5"), txt))
                .await?;
        }
        None => {
            ctx.reply(&ctx.get_str("blk_6")).await?;
        }
    }
    Ok(())
}

/// Replace `{}` placeholders in a localised string in order
/// (Python `str.format(*args)`).
pub fn fmt_args(template: &str, args: &[&str]) -> String {
    let mut out = template.to_string();
    for a in args {
        match out.find("{}") {
            Some(pos) => out.replace_range(pos..pos + 2, a),
            None => break,
        }
    }
    out
}

/// Does this message contain a blacklisted word? (Python `blacklist`
/// watcher: lowercases the text, splits on whitespace, matches whole
/// words.)
pub fn contains_blacklisted(text: &str, words: &[String]) -> bool {
    let lowered = text.to_lowercase();
    let tokens: Vec<&str> = lowered.split_whitespace().collect();
    words.iter().any(|w| tokens.contains(&w.as_str()))
}

/// Delete a message that tripped the blacklist (Python `blacklist`).
pub async fn delete_blacklisted(ctx: &Ctx, message_id: i64) -> Result<()> {
    ctx.responder
        .delete_from(ctx.chat_id, &[message_id as i32])
        .await?;
    Ok(())
}
