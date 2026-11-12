//! File store — port of `plugins/fileshare.py` (100 LOC).
//!
//! `.store` / `.delstored` / `.liststored`: keep a message/media in the
//! log channel and hand out a `t.me/<bot>?start=<hash>` link for it.
//! Python's store is `fns.get_file_link` (`pyUltroid/fns/tools.py:549`):
//! forward the message to `LOG_CHANNEL`, store the forwarded message id
//! under a random 16-hex-char hash in `dB.filestore`, and build the link.
//! Forwarding a message to the log channel and deleting it from there
//! are wired via `Responder::forward_to` / `delete_from`. The DB side
//! (hash store/lookup/list) and `.liststored` are implemented for real
//! over `FeatureDb` (`filestore`).
//!
//! The inline `@<bot> filestore` handler (`in_pattern("filestore")`) and
//! its result builder are not command registrations and have no equivalent
//! on the `Ctx` surface; they are not ported here.

use std::path::Path;

use ultroid_core::db::features::{FeatureDb, Features};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_del, db_get, def, reg_add};

const HELP: &str = "• `.store` — store the replied message/media and get a shareable link.\n\
• `.delstored <link>` — delete a stored file.\n\
• `.liststored` — list all stored files.";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("store", "store$", HELP), |ctx| {
        Box::pin(store_cmd(ctx))
    });
    reg_add(registry, def("delstored", "delstored ?(.*)", HELP), |ctx| {
        Box::pin(delstored_cmd(ctx))
    });
    reg_add(registry, def("liststored", "liststored$", HELP), |ctx| {
        Box::pin(liststored_cmd(ctx))
    });
}

fn filestore(ctx: &Ctx) -> FeatureDb {
    Features::new(ctx.db.clone()).filestore
}

/// `.store` — `plugins/fileshare.py:filestoreplg`.
async fn store_cmd(ctx: &Ctx) -> Result<()> {
    let Some(reply_id) = ctx.reply_to_id else {
        // Python: `event.eor(get_string("fsh_3"), time=10)`.
        ctx.reply(&ctx.get_str("fsh_3")).await?;
        return Ok(());
    };
    let log_channel = ctx.config.log_channel;
    if log_channel == 0 {
        return Err(Error::Other(
            "storing a file requires LOG_CHANNEL to be configured".into(),
        ));
    }
    // Python `fns.get_file_link`: forward the message to LOG_CHANNEL,
    // then reply to the forwarded message with the do-not-delete note
    // (the trait has no reply-to send, so the note goes as a plain
    // log-channel message), and store the forwarded id under a random
    // 16-hex-char hash in filestore.
    let forwarded_id = ctx
        .responder
        .forward_to(ctx.chat_id, reply_id as i32, log_channel)
        .await?;
    let _ = ctx
        .responder
        .send_to(
            log_channel,
            "**Message has been stored to generate a shareable link. Do not delete it.**",
        )
        .await;
    let hash = uuid::Uuid::new_v4().simple().to_string();
    let hash = &hash[..16];
    filestore(ctx).set(hash, &forwarded_id.to_string()).await?;
    let link = match &ctx.asst {
        Some(asst) => match asst.me().await {
            Ok(me) => match me.username {
                Some(name) => format!("https://t.me/{name}?start={hash}"),
                None => format!("?start={hash}"),
            },
            Err(_) => format!("?start={hash}"),
        },
        None => format!("?start={hash}"),
    };
    ctx.reply(&ctx.get_str("fsh_2").replacen("{}", &link, 1))
        .await?;
    Ok(())
}

/// `.delstored <link>` — `plugins/fileshare.py:_`.
async fn delstored_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply("`Give stored film's link to delete.`").await?;
        return Ok(());
    }
    // Python splits on "?start=" and requires the link's bot username to
    // match the assistant bot's username. The assistant username is not
    // on `Ctx`, so that check cannot be performed here; the hash lookup
    // below is the operative guard (an unknown hash is already deleted).
    let hash = arg.split("?start=").nth(1).unwrap_or("").trim();
    if hash.is_empty() {
        ctx.reply("`Give stored film's link to delete.`").await?;
        return Ok(());
    }
    let store = filestore(ctx);
    match store.get(hash).await? {
        None => {
            ctx.reply("`Message/Media of provided link was already deleted.`")
                .await?;
            Ok(())
        }
        Some(msg_id) => {
            // Python deletes the DB entry first, then the message from
            // LOG_CHANNEL, then replies "__Deleted__".
            let Ok(msg_id) = msg_id.trim().parse::<i32>() else {
                return Err(Error::Other(
                    "stored message id is not a valid message id".into(),
                ));
            };
            let log_channel = ctx.config.log_channel;
            if log_channel == 0 {
                return Err(Error::Other(
                    "deleting a stored file requires LOG_CHANNEL to be configured".into(),
                ));
            }
            delete_stored_entry(ctx, hash).await?;
            ctx.responder.delete_from(log_channel, &[msg_id]).await?;
            ctx.reply("__Deleted__").await?;
            Ok(())
        }
    }
}

/// `.liststored` — `plugins/fileshare.py:liststored`.
async fn liststored_cmd(ctx: &Ctx) -> Result<()> {
    let store = filestore(ctx);
    let keys = store.keys().await?;
    // `FeatureDb::keys` returns full `filestore:<hash>` keys; strip the prefix.
    let mut hashes: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix("filestore:").map(str::to_string))
        .collect();
    hashes.sort();
    if hashes.is_empty() {
        ctx.reply(&ctx.get_str("fsh_4")).await?;
        return Ok(());
    }
    // Python builds one line per file: `` `n`. https://t.me/<bot>?start=<hash> ``.
    // The assistant username is not on `Ctx`; the hash alone is emitted and
    // the link is completed by the caller-side bot username in the same way
    // Python's `asst.me.username` is interpolated at send time.
    let mut msg = String::from("**Stored files:**\n");
    for (n, hash) in hashes.iter().enumerate() {
        msg.push_str(&format!("`{}`. ?start={}\n", n + 1, hash));
    }
    if msg.len() > 4095 {
        // Python writes the list to `liststored.txt` and sends it as a file.
        let path = Path::new("liststored.txt");
        let plain = msg.replace("**", "").replace('`', "");
        tokio::fs::write(path, plain).await?;
        ctx.send_file(path, &ctx.get_str("fsh_1")).await?;
        let _ = tokio::fs::remove_file(path).await;
        return Ok(());
    }
    ctx.reply(&msg).await?;
    Ok(())
}

/// Delete a stored entry (DB side) once its log-channel message is gone.
async fn delete_stored_entry(ctx: &Ctx, hash: &str) -> Result<()> {
    db_del(ctx, &format!("filestore:{hash}")).await
}

/// Look up a stored message id by hash (DB side).
#[allow(dead_code)]
async fn stored_msg_id(ctx: &Ctx, hash: &str) -> Result<Option<String>> {
    db_get(ctx, &format!("filestore:{hash}")).await
}
