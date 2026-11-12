//! Broadcast — port of `plugins/broadcast.py`
//! (`.addch` / `.remch` / `.listchannels` / `.forward` / `.broadcast`).
//!
//! Channel storage is the `BROADCAST` udB key holding a JSON list of chat
//! ids (Python `KeyManager("BROADCAST", cast=list)`), so the key stays
//! compatible with the Python database.
//!
//! What cannot be expressed through `Ctx` in this wave:
//! * `.addch all` enumerates dialogs (`client.get_dialogs()` filtered to
//!   broadcast channels the account admins) — no dialog-listing op.
//! * `.listchannels` resolves each id to a display name via
//!   `get_entity` — no entity-fetch op, so ids are listed as-is.
//! * `.forward` / `.broadcast` forward or re-send the replied message into
//!   every stored channel — no send/forward-to-arbitrary-chat op and no
//!   replied-message fetch. The validation and counting logic around the
//!   send loop is implemented; the send itself is the `TODO(responder-op)`
//!   site.

use crate::util::{def, reg_add};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

/// The stored broadcast list (Python `KeyM.get()`).
async fn channels(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(db_get_json(&*ctx.db, "BROADCAST")
        .await?
        .unwrap_or_default())
}

async fn save_channels(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "BROADCAST", &list.to_vec()).await
}

/// `.addch [all|<reply to id list>|<none>]` — port of
/// `broadcast.py::broadcast_adder`.
async fn addch(ctx: &Ctx) -> Result<()> {
    let msgg = ctx.args.trim();
    let x = ctx.reply(&ctx.get_str("bd_1")).await?;
    if msgg == "all" {
        ctx.edit(x, &ctx.get_str("bd_2")).await?;
        // TODO(responder-op): list dialogs, keep broadcast channels where
        // the account is creator/admin, add the missing ones
        // (Python: broadcast.py:broadcast_adder, `get_dialogs` branch).
        return Err(todo_op(
            "list dialogs (broadcast channels the account admins)",
            "broadcast.py:broadcast_adder",
        ));
    }
    if ctx.reply_to_id.is_some() {
        // Python parses the replied message's lines: lines[1..len-2], and
        // takes `line[4:-1]` as the channel id. `reply_to_text` is the
        // replied message's text.
        let raw = ctx.reply_to_text.clone().unwrap_or_default();
        let lines: Vec<&str> = raw.split('\n').collect();
        let mut list = channels(ctx).await?;
        let mut added = false;
        if lines.len() > 3 {
            for line in &lines[1..lines.len() - 2] {
                let chars: Vec<char> = line.chars().collect();
                if chars.len() > 5 {
                    let id_str: String = chars[4..chars.len() - 1].iter().collect();
                    if let Ok(id) = id_str.trim().parse::<i64>() {
                        if !list.contains(&id) {
                            list.push(id);
                            added = true;
                        }
                    }
                }
            }
        }
        if added {
            save_channels(ctx, &list).await?;
        }
        ctx.edit(x, &ctx.get_str("bd_4")).await?;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let _ = ctx.delete(x).await;
        return Ok(());
    }
    let chat_id = ctx.chat_id;
    if chat_id == ctx.config.log_channel && ctx.config.log_channel != 0 {
        return Ok(());
    }
    let mut list = channels(ctx).await?;
    if list.contains(&chat_id) {
        ctx.edit(x, &ctx.get_str("bd_6")).await?;
    } else {
        list.push(chat_id);
        save_channels(ctx, &list).await?;
        ctx.edit(x, &ctx.get_str("bd_5")).await?;
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let _ = ctx.delete(x).await;
    Ok(())
}

/// `.remch [all|id]` — port of `broadcast.py::broadcast_remover`.
async fn remch(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let chat_id: i64 = if arg.is_empty() {
        ctx.chat_id
    } else {
        arg.parse().unwrap_or(ctx.chat_id)
    };
    let x = ctx.reply(&ctx.get_str("com_1")).await?;
    if arg == "all" {
        ctx.edit(x, &ctx.get_str("bd_8")).await?;
        ctx.db.del("BROADCAST").await?;
        ctx.edit(x, "Database cleared.").await?;
        return Ok(());
    }
    let mut list = channels(ctx).await?;
    if list.contains(&chat_id) {
        list.retain(|c| *c != chat_id);
        save_channels(ctx, &list).await?;
        ctx.edit(x, &ctx.get_str("bd_7")).await?;
    } else {
        ctx.edit(x, &ctx.get_str("bd_9")).await?;
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let _ = ctx.delete(x).await;
    Ok(())
}

/// `.listchannels` — port of `broadcast.py::list_all`. Names are
/// unavailable without an entity-fetch op, so ids are listed as-is.
async fn listchannels(ctx: &Ctx) -> Result<()> {
    let x = ctx.reply(&ctx.get_str("com_1")).await?;
    let list = channels(ctx).await?;
    if list.is_empty() {
        ctx.edit(x, "No chats were added.").await?;
        return Ok(());
    }
    let mut msg = String::from("Channels in database:\n");
    for channel in &list {
        // TODO(responder-op): get_entity(channel) for the display name
        // (Python: broadcast.py:list_all).
        msg.push_str(&format!("=> **{channel}** [`{channel}`]\n"));
    }
    msg.push_str(&format!("\nTotal {} channels.", list.len()));
    if msg.len() > 4096 {
        // Over-long list goes out as a file, as in Python.
        let plain = msg.replace('*', "").replace('`', "");
        let path = std::env::temp_dir().join("channels.txt");
        tokio::fs::write(&path, plain).await?;
        ctx.send_file(&path, "Channels in Database").await?;
        let _ = tokio::fs::remove_file(&path).await;
        let _ = ctx.delete(x).await;
    } else {
        ctx.edit(x, &msg).await?;
    }
    Ok(())
}

/// `.forward` — port of `broadcast.py::forw`. Requires a replied message.
async fn forward(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        return ctx.reply(&ctx.get_str("ex_1")).await.map(|_| ());
    }
    let list = channels(ctx).await?;
    let x = ctx.reply("Sending...").await?;
    if list.is_empty() {
        return ctx
            .edit(x, "Please add channels by using `.addch` in them.")
            .await
            .map(|_| ());
    }
    // TODO(responder-op): forward the replied message to each channel id
    // (Python: broadcast.py:forw -> ultroid_bot.forward_messages).
    let _ = x;
    Err(todo_op(
        "forward replied message to stored channel ids",
        "broadcast.py:forw",
    ))
}

/// `.broadcast` — port of `broadcast.py::sending`. Requires a replied
/// message; polls must go through `.forward`.
async fn broadcast(ctx: &Ctx) -> Result<()> {
    let x = ctx.reply(&ctx.get_str("com_1")).await?;
    if ctx.reply_to_id.is_none() {
        return ctx.edit(x, &ctx.get_str("ex_1")).await.map(|_| ());
    }
    let list = channels(ctx).await?;
    if list.is_empty() {
        return ctx
            .edit(x, "Please add channels by using `.addch` in them.")
            .await
            .map(|_| ());
    }
    // TODO(responder-op): send the replied message to each channel id
    // (Python: broadcast.py:sending -> ultroid_bot.send_message(channel,
    // previous_message)). Only the replied text is visible here
    // (`reply_to_text`); media/polls need the full message object.
    let _ = x;
    Err(todo_op(
        "send replied message to stored channel ids",
        "broadcast.py:sending",
    ))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "addch",
            pattern: "addch( (.*)|$)",
            help: "Add chat to the broadcast database.",
            sudo_allowed: false,
            ..Default::default()
        },
        |ctx| Box::pin(addch(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "remch",
            pattern: "remch( (.*)|$)",
            help: "Remove a chat (or all) from the broadcast database.",
            sudo_allowed: false,
            ..Default::default()
        },
        |ctx| Box::pin(remch(ctx)),
    );
    reg_add(
        registry,
        def(
            "listchannels",
            "listchannels$",
            "List all chats in the broadcast database.",
        ),
        |ctx| Box::pin(listchannels(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "forward",
            pattern: "forward$",
            help: "Forward the replied message to all broadcast chats.",
            sudo_allowed: false,
            ..Default::default()
        },
        |ctx| Box::pin(forward(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "broadcast",
            pattern: "broadcast( (.*)|$)",
            help: "Send the replied message to all broadcast chats.",
            sudo_allowed: false,
            ..Default::default()
        },
        |ctx| Box::pin(broadcast(ctx)),
    );
}
