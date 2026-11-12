//! Assistant-bot custom commands — port of `plugins/asstcmd.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/asstcmd_db.py`):
//! `ASST_CMDS` maps command name (lowercased, no slash) ->
//! {msg, media, button}.
//!
//! `.addcmd` / `.remcmd` / `.listcmd` are fully ported for the text path.
//! Storing a command from a replied message with media needs the reply's
//! media descriptor + a download (+ telegraph upload for photos/videos),
//! which the `Ctx` surface does not expose — that path is a
//! `TODO(responder-op)` site. The assistant-side responder (`astcmds`:
//! replying when a user sends `/<cmd>` to the assistant bot) is an
//! assistant-bot `NewMessage` handler, not a command; its lookup is ported
//! as the pure `lookup` function for the assistant crate to call.

use crate::util::{def, reg_add};
use serde_json::{Value, json};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

pub const HELP: &str = " -\n\n-`{i}addcmd <new cmd> <reply>`\n   It will set new cmd for your assistant bot with that reply message.\n\n-`{i}remcmd <cmd name>`\n   It will remove your cmd.\n\n-`{i}listcmd`\n   To Get list of all your custom cmd.\n";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("addcmd", "addcmd( (.*)|$)", HELP), |ctx| {
        Box::pin(add_cmd(ctx))
    });
    reg_add(registry, def("remcmd", "remcmd( (.*)|$)", HELP), |ctx| {
        Box::pin(remove_cmd(ctx))
    });
    reg_add(registry, def("listcmd", "listcmd$", HELP), |ctx| {
        Box::pin(list_cmd(ctx))
    });
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
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

/// One stored assistant command (Python `AsstCmdDB` record).
#[derive(Debug, Clone)]
pub struct AsstCmd {
    pub msg: Option<String>,
    pub media: Option<String>,
    pub button: Option<String>,
}

/// All stored assistant commands (Python `dbs.asstcmd.get_stuff()`).
pub async fn get_asstcmds(ctx: &Ctx) -> Result<HashMap<String, AsstCmd>> {
    let raw: HashMap<String, Value> = db_get_json(&*ctx.db, "ASST_CMDS")
        .await?
        .unwrap_or_default();
    let mut out = HashMap::new();
    for (k, v) in raw {
        out.insert(
            k,
            AsstCmd {
                msg: v.get("msg").and_then(|x| x.as_str()).map(str::to_string),
                media: v.get("media").and_then(|x| x.as_str()).map(str::to_string),
                button: v.get("button").and_then(|x| x.as_str()).map(str::to_string),
            },
        );
    }
    Ok(out)
}

async fn set_asstcmds(ctx: &Ctx, map: &HashMap<String, AsstCmd>) -> Result<()> {
    let mut raw: HashMap<String, Value> = HashMap::new();
    for (k, v) in map {
        raw.insert(
            k.clone(),
            json!({
                "msg": v.msg,
                "media": v.media,
                "button": v.button,
            }),
        );
    }
    db_set_json(&*ctx.db, "ASST_CMDS", &raw).await
}

/// Look a command up for the assistant responder (Python `dbs.asstcmd.reply`).
pub async fn lookup(ctx: &Ctx, cmd: &str) -> Result<Option<AsstCmd>> {
    Ok(get_asstcmds(ctx).await?.get(&cmd.to_lowercase()).cloned())
}

async fn add_cmd(ctx: &Ctx) -> Result<()> {
    let mut word = ctx.args.trim().to_lowercase();
    word = word.replace('/', "");
    if ctx.reply_to_id.is_none() || word.is_empty() {
        ctx.reply(&ctx.get_str("asstcmd_1")).await?;
        return Ok(());
    }
    // The replied message's text / media / buttons are not on the `Ctx`
    // surface (only `reply_to_text` is). Text replies are ported below;
    // media replies need a download + telegraph upload.
    match ctx.reply_to_text.as_deref() {
        Some(text) if !text.is_empty() => {
            let (txt, btn) = crate::greetings::split_msg_button(text);
            let mut map = get_asstcmds(ctx).await?;
            map.insert(
                word.clone(),
                AsstCmd {
                    msg: Some(txt),
                    media: None,
                    button: btn,
                },
            );
            set_asstcmds(ctx, &map).await?;
            ctx.reply(&fmt_args(&ctx.get_str("asstcmd_4"), &[&word]))
                .await?;
            Ok(())
        }
        _ => Err(todo_op(
            "get replied message media/text for asstcmd",
            "plugins/asstcmd.py:ac",
        )),
    }
}

async fn remove_cmd(ctx: &Ctx) -> Result<()> {
    let mut word = ctx.args.trim().to_lowercase();
    if word.is_empty() {
        ctx.reply(&ctx.get_str("asstcmd_2")).await?;
        return Ok(());
    }
    word = word.replace('/', "");
    let mut map = get_asstcmds(ctx).await?;
    map.remove(&word);
    set_asstcmds(ctx, &map).await?;
    ctx.reply(&fmt_args(&ctx.get_str("asstcmd_3"), &[&word]))
        .await?;
    Ok(())
}

async fn list_cmd(ctx: &Ctx) -> Result<()> {
    let map = get_asstcmds(ctx).await?;
    if map.is_empty() {
        ctx.reply(&ctx.get_str("asstcmd_5")).await?;
        return Ok(());
    }
    let mut out = ctx.get_str("asstcmd_6");
    let mut names: Vec<&String> = map.keys().collect();
    names.sort();
    for name in names {
        out.push_str(&format!("/{name}\n"));
    }
    ctx.reply(&out).await?;
    Ok(())
}
