//! Welcome / goodbye notes + thank-members — port of `plugins/greetings.py`.
//!
//! DB keys (raw udB keys, same as Python `pyUltroid/dB/greetings_db.py`):
//! `WELCOME` / `GOODBYE` map chat id -> {welcome|goodbye, media, button};
//! `THANK_MEMBERS` maps chat id -> true.
//!
//! Media handling: setting a note from a replied message's text is
//! resolved through `message_summary`; a replied message with media
//! still needs the reply's media kind + a download, which the trait does
//! not expose — that path returns a `TODO(responder-op)` error (see
//! `set_note`). Notes with inline buttons are delivered with
//! `send_markup_to`; a note with stored media cannot be sent (no trait
//! op sends a stored media reference). The plain-text path is fully
//! ported. Telegraph upload (`graph.org`) is done over `reqwest` when a
//! local file path is available; without a download there is nothing to
//! upload.

use crate::util::{def, reg_add};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::tg::InlineButton;
use ultroid_core::{Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n---- Welcomes ----\n• `{i}setwelcome <message/reply to message>`\n    Set welcome message in the current chat.\n\n• `{i}clearwelcome`\n    Delete the welcome in the current chat.\n\n• `{i}getwelcome`\n    Get the welcome message in the current chat.\n\n---- GoodByes ----\n• `{i}setgoodbye <message/reply to message>`\n    Set goodbye message in the current chat.\n\n• `{i}cleargoodbye`\n    Delete the goodbye in the current chat.\n\n• `{i}getgoodbye`\n    Get the goodbye message in the current chat.\n\n• `{i}thankmembers on/off`\n    Send a thank you sticker on hitting a members count of 100*x in your groups.\n\nNote: `{mention}`, `{group}`, `{count}`, `{name}`, `{fullname}`, `{username}`, `{userid}` can be used as formatting parameters.\n";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("setwelcome", "setwelcome", HELP), |ctx| {
        Box::pin(set_welcome(ctx))
    });
    reg_add(
        registry,
        def("clearwelcome", "clearwelcome$", HELP),
        |ctx| Box::pin(clear_welcome(ctx)),
    );
    reg_add(registry, def("getwelcome", "getwelcome$", HELP), |ctx| {
        Box::pin(get_welcome(ctx))
    });
    reg_add(registry, def("setgoodbye", "setgoodbye", HELP), |ctx| {
        Box::pin(set_goodbye(ctx))
    });
    reg_add(
        registry,
        def("cleargoodbye", "cleargoodbye$", HELP),
        |ctx| Box::pin(clear_goodbye(ctx)),
    );
    reg_add(registry, def("getgoodbye", "getgoodbye$", HELP), |ctx| {
        Box::pin(get_goodbye(ctx))
    });
    reg_add(
        registry,
        def("thankmembers", "thankmembers (on|off)", HELP),
        |ctx| Box::pin(thank_members(ctx)),
    );
}

/// Fill the legacy greeting placeholders (port of
/// `pyUltroid/_core/events.py` `render_welcome` / `render_farewell`).
/// `mention` is the `[name](tg://user?id=…)` markdown mention of the user.
pub fn render_note(
    template: &str,
    mention: &str,
    name: &str,
    fullname: &str,
    username: &str,
    userid: i64,
    group: &str,
    count: i64,
) -> String {
    template
        .replace("{mention}", mention)
        .replace("{group}", group)
        .replace("{count}", &count.to_string())
        .replace("{fullname}", fullname)
        .replace("{name}", name)
        .replace("{username}", username)
        .replace("{userid}", &userid.to_string())
}

/// `render_welcome` alias (same placeholder set as the farewell renderer).
pub fn render_welcome(
    template: &str,
    mention: &str,
    name: &str,
    fullname: &str,
    username: &str,
    userid: i64,
    group: &str,
    count: i64,
) -> String {
    render_note(
        template, mention, name, fullname, username, userid, group, count,
    )
}

/// `render_farewell` alias (same placeholder set as the welcome renderer).
pub fn render_farewell(
    template: &str,
    mention: &str,
    name: &str,
    fullname: &str,
    username: &str,
    userid: i64,
    group: &str,
    count: i64,
) -> String {
    render_note(
        template, mention, name, fullname, username, userid, group, count,
    )
}

/// A stored welcome/goodbye note: (text, media, button).
pub struct GreetingNote {
    pub text: Option<String>,
    pub media: Option<String>,
    pub button: Option<String>,
}

fn note_from_value(v: &Value, text_key: &str) -> Option<GreetingNote> {
    let obj = v.as_object()?;
    Some(GreetingNote {
        text: obj
            .get(text_key)
            .and_then(|t| t.as_str())
            .map(str::to_string),
        media: obj
            .get("media")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        button: obj
            .get("button")
            .and_then(|t| t.as_str())
            .map(str::to_string),
    })
}

async fn get_note_map(ctx: &Ctx, key: &str) -> Result<HashMap<String, Value>> {
    Ok(db_get_json(&*ctx.db, key).await?.unwrap_or_default())
}

async fn set_note_map(ctx: &Ctx, key: &str, map: &HashMap<String, Value>) -> Result<()> {
    db_set_json(&*ctx.db, key, map).await
}

pub async fn get_welcome_note(ctx: &Ctx, chat_id: i64) -> Result<Option<GreetingNote>> {
    let map = get_note_map(ctx, "WELCOME").await?;
    Ok(map
        .get(&chat_id.to_string())
        .and_then(|v| note_from_value(v, "welcome")))
}

pub async fn get_goodbye_note(ctx: &Ctx, chat_id: i64) -> Result<Option<GreetingNote>> {
    let map = get_note_map(ctx, "GOODBYE").await?;
    Ok(map
        .get(&chat_id.to_string())
        .and_then(|v| note_from_value(v, "goodbye")))
}

async fn put_note(
    ctx: &Ctx,
    key: &str,
    text_key: &str,
    chat_id: i64,
    note: GreetingNote,
) -> Result<()> {
    let mut map = get_note_map(ctx, key).await?;
    let mut obj = Map::new();
    obj.insert(
        text_key.to_string(),
        note.text.map(Value::String).unwrap_or(Value::Null),
    );
    obj.insert(
        "media".to_string(),
        note.media.map(Value::String).unwrap_or(Value::Null),
    );
    obj.insert(
        "button".to_string(),
        note.button.map(Value::String).unwrap_or(Value::Null),
    );
    map.insert(chat_id.to_string(), Value::Object(obj));
    set_note_map(ctx, key, &map).await
}

async fn del_note(ctx: &Ctx, key: &str, chat_id: i64) -> Result<()> {
    let mut map = get_note_map(ctx, key).await?;
    map.remove(&chat_id.to_string());
    set_note_map(ctx, key, &map).await
}

/// Parse `[label | url]` button markup out of a note's text, mirroring
/// Python `fns.get_msg_button`: returns (clean_text, button_json_or_none).
/// Button markup lines look like `[Text | https://t.me/x]` (optionally with
/// `| same` to share a row). Lines that are not button markup stay in the text.
pub fn split_msg_button(text: &str) -> (String, Option<String>) {
    let mut body_lines: Vec<&str> = Vec::new();
    let mut buttons: Vec<Vec<(String, String)>> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') && t.contains('|') {
            let inner = &t[1..t.len() - 1];
            let parts: Vec<&str> = inner.split('|').collect();
            if parts.len() >= 2 {
                let label = parts[0].trim().to_string();
                let url = parts[1].trim().to_string();
                let same = parts
                    .get(2)
                    .map(|s| s.trim().eq_ignore_ascii_case("same"))
                    .unwrap_or(false);
                if same {
                    if let Some(row) = buttons.last_mut() {
                        row.push((label, url));
                    } else {
                        buttons.push(vec![(label, url)]);
                    }
                } else {
                    buttons.push(vec![(label, url)]);
                }
                continue;
            }
        }
        body_lines.push(line);
    }
    let body = body_lines.join("\n").trim().to_string();
    let btn = if buttons.is_empty() {
        None
    } else {
        Some(json!(buttons).to_string())
    };
    (body, btn)
}

/// Upload a local file to telegra.ph and return the `https://graph.org/...`
/// URL (Python `telegraph.upload_file`). Real HTTP via `ctx.http`.
/// The multipart body is built by hand: the workspace `reqwest` build has
/// no `multipart` feature enabled, so the `multipart` module is unavailable.
pub async fn upload_to_telegraph(ctx: &Ctx, path: &std::path::Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await.map_err(Error::Io)?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let boundary = format!("----ultroid-{}", uuid::Uuid::new_v4().simple());
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = ctx
        .http
        .post("https://telegra.ph/upload")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await?;
    let body: Value = resp.json().await.map_err(Error::Http)?;
    let first = body
        .as_array()
        .and_then(|a| a.first())
        .and_then(|o| o.get("src"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| Error::Other(format!("telegra.ph upload failed: {body}")))?;
    Ok(format!("https://telegra.ph{first}"))
}

/// Parse stored greeting button JSON — `split_msg_button` stores a
/// nested `[[[label, url], …], …]` row list — into markup rows, for
/// `Responder::send_markup_to`.
fn buttons_from_json(button_json: &str) -> Vec<Vec<InlineButton>> {
    let rows: Vec<Vec<(String, String)>> = serde_json::from_str(button_json).unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(label, url)| InlineButton::url(label, url))
                .collect()
        })
        .collect()
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

async fn set_note(
    ctx: &Ctx,
    key: &str,
    text_key: &str,
    saved_msg: &str,
    empty_msg_key: &str,
) -> Result<()> {
    // Text after the command (Python: event.text.split(maxsplit=1)[1]).
    let text_arg = ctx
        .text
        .splitn(2, ' ')
        .nth(1)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(reply_id) = ctx.reply_to_id {
        // Resolve the replied message through `message_summary`. A text
        // reply is stored like an inline-text note; a media reply still
        // needs the media-kind branch (download + Telegraph upload for
        // pics/gifs/videos, bot file id otherwise), which no trait op
        // provides, so it stays an honest error. Python:
        // greetings.py:setwel / setgb.
        let Some(summary) = ctx
            .responder
            .message_summary(ctx.chat_id, reply_id as i32)
            .await?
        else {
            return Err(Error::Other(
                "the replied message is no longer available".into(),
            ));
        };
        if summary.has_media {
            return Err(todo_op(
                "get replied message media (download + Telegraph upload / bot file id)",
                "plugins/greetings.py:setwel",
            ));
        }
        if summary.text.is_empty() {
            ctx.reply(&ctx.get_str(empty_msg_key)).await?;
            return Ok(());
        }
        let (txt, btn) = split_msg_button(&summary.text);
        put_note(
            ctx,
            key,
            text_key,
            ctx.chat_id,
            GreetingNote {
                text: Some(txt),
                media: None,
                button: btn,
            },
        )
        .await?;
        ctx.reply(saved_msg).await?;
        return Ok(());
    }
    match text_arg {
        Some(text) => {
            let (txt, btn) = split_msg_button(text);
            put_note(
                ctx,
                key,
                text_key,
                ctx.chat_id,
                GreetingNote {
                    text: Some(txt),
                    media: None,
                    button: btn,
                },
            )
            .await?;
            ctx.reply(saved_msg).await?;
            Ok(())
        }
        None => {
            ctx.reply(&ctx.get_str(empty_msg_key)).await?;
            Ok(())
        }
    }
}

async fn set_welcome(ctx: &Ctx) -> Result<()> {
    set_note(ctx, "WELCOME", "welcome", &ctx.get_str("grt_1"), "grt_3").await
}

async fn set_goodbye(ctx: &Ctx) -> Result<()> {
    set_note(ctx, "GOODBYE", "goodbye", "`Goodbye note saved`", "grt_7").await
}

async fn clear_welcome(ctx: &Ctx) -> Result<()> {
    if get_welcome_note(ctx, ctx.chat_id).await?.is_none() {
        ctx.reply(&ctx.get_str("grt_4")).await?;
        return Ok(());
    }
    del_note(ctx, "WELCOME", ctx.chat_id).await?;
    ctx.reply(&ctx.get_str("grt_5")).await?;
    Ok(())
}

async fn clear_goodbye(ctx: &Ctx) -> Result<()> {
    if get_goodbye_note(ctx, ctx.chat_id).await?.is_none() {
        ctx.reply(&ctx.get_str("grt_6")).await?;
        return Ok(());
    }
    del_note(ctx, "GOODBYE", ctx.chat_id).await?;
    ctx.reply("`Goodbye Note Deleted`").await?;
    Ok(())
}

async fn get_welcome(ctx: &Ctx) -> Result<()> {
    let Some(note) = get_welcome_note(ctx, ctx.chat_id).await? else {
        ctx.reply(&ctx.get_str("grt_4")).await?;
        return Ok(());
    };
    if let Some(button_json) = note.button.as_deref() {
        // Note with inline buttons — delivered directly with
        // `send_markup_to` (Python goes through the assistant inline flow
        // `something()`; the visible result is the same). The stored
        // media, if any, cannot be attached — no trait op sends a stored
        // media reference — so a media note still errors below.
        if note.media.is_some() {
            return Err(todo_op(
                "send note with stored media",
                "plugins/greetings.py:listwel",
            ));
        }
        let text = format!(
            "**Welcome Note in this chat**\n\n`{}`",
            note.text.clone().unwrap_or_default()
        );
        let rows = buttons_from_json(button_json);
        ctx.responder
            .send_markup_to(ctx.chat_id, &text, rows)
            .await?;
        return Ok(());
    }
    let text = format!(
        "**Welcome Note in this chat**\n\n`{}`",
        note.text.clone().unwrap_or_default()
    );
    ctx.reply(&text).await?;
    Ok(())
}

async fn get_goodbye(ctx: &Ctx) -> Result<()> {
    let Some(note) = get_goodbye_note(ctx, ctx.chat_id).await? else {
        ctx.reply(&ctx.get_str("grt_6")).await?;
        return Ok(());
    };
    if let Some(button_json) = note.button.as_deref() {
        // Same button-delivery path as getwelcome.
        if note.media.is_some() {
            return Err(todo_op(
                "send note with stored media",
                "plugins/greetings.py:listgd",
            ));
        }
        let text = format!(
            "**Goodbye Note in this chat**\n\n`{}`",
            note.text.clone().unwrap_or_default()
        );
        let rows = buttons_from_json(button_json);
        ctx.responder
            .send_markup_to(ctx.chat_id, &text, rows)
            .await?;
        return Ok(());
    }
    let text = format!(
        "**Goodbye Note in this chat**\n\n`{}`",
        note.text.clone().unwrap_or_default()
    );
    ctx.reply(&text).await?;
    Ok(())
}

async fn thank_members(ctx: &Ctx) -> Result<()> {
    // Pattern `thankmembers (on|off)` — group 1 is the toggle.
    let mode = ctx.captures.get(1).map(|s| s.as_str()).unwrap_or("");
    let mut map: HashMap<String, Value> = db_get_json(&*ctx.db, "THANK_MEMBERS")
        .await?
        .unwrap_or_default();
    match mode.to_lowercase().as_str() {
        "on" => {
            map.insert(ctx.chat_id.to_string(), Value::Bool(true));
            db_set_json(&*ctx.db, "THANK_MEMBERS", &map).await?;
        }
        "off" => {
            map.remove(&ctx.chat_id.to_string());
            db_set_json(&*ctx.db, "THANK_MEMBERS", &map).await?;
        }
        _ => {}
    }
    ctx.reply(&format!(
        "**Done! Thank you members has been turned** `{mode}` **for this chat**!"
    ))
    .await?;
    Ok(())
}

/// Is the thank-members sticker enabled for this chat?
/// (Python `dbs.greetings.must_thank`.)
pub async fn must_thank(ctx: &Ctx, chat_id: i64) -> Result<bool> {
    let map: HashMap<String, Value> = db_get_json(&*ctx.db, "THANK_MEMBERS")
        .await?
        .unwrap_or_default();
    Ok(map
        .get(&chat_id.to_string())
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
