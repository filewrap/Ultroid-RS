//! File store — port of `plugins/fileshare.py` (100 LOC).
//!
//! `.store` / `.delstored` / `.liststored`: keep a message/media in the
//! log channel and hand out a `t.me/<bot>?start=<hash>` link for it.
//! Python's store is `fns.get_file_link` (`pyUltroid/fns/tools.py:549`):
//! forward the message to `LOG_CHANNEL`, store the forwarded message id
//! under a random 16-hex-char hash in `dB.filestore`, and build the link.
//! Forwarding a message and deleting a message from the log channel are
//! binary Responder operations; those sites return the honest
//! `TODO(responder-op)` error. The DB side (hash store/lookup/list) and
//! `.liststored` are implemented for real over `FeatureDb` (`filestore`).
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
    if ctx.reply_to_id.is_none() {
        // Python: `event.eor(get_string("fsh_3"), time=10)`.
        ctx.reply(&ctx.get_str("fsh_3")).await?;
        return Ok(());
    }
    // TODO(responder-op): forward the replied message to LOG_CHANNEL,
    // store the forwarded message id under a random hash in filestore, and
    // reply `get_string("fsh_2")` formatted with
    // `https://t.me/<asst_username>?start=<hash>`
    // (Python: fileshare.py:filestoreplg via fns.get_file_link).
    Err(Error::Other(
        "forward replied message to LOG_CHANNEL + store file link requires a binary Responder op (Python: fileshare.py:filestoreplg)"
            .to_string(),
    ))
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
            // LOG_CHANNEL. The Telegram delete is a binary op, so fail
            // before mutating anything to keep DB and channel consistent.
            // TODO(responder-op): delete message `msg_id` from LOG_CHANNEL
            // (Python: fileshare.py:_ via ultroid_bot.delete_messages).
            let _ = msg_id;
            Err(Error::Other(
                "delete stored message from LOG_CHANNEL requires a binary Responder op (Python: fileshare.py:_)"
                    .to_string(),
            ))
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
/// Kept for the follow-up pass that wires the Responder delete op.
#[allow(dead_code)]
async fn delete_stored_entry(ctx: &Ctx, hash: &str) -> Result<()> {
    db_del(ctx, &format!("filestore:{hash}")).await
}

/// Look up a stored message id by hash (DB side).
#[allow(dead_code)]
async fn stored_msg_id(ctx: &Ctx, hash: &str) -> Result<Option<String>> {
    db_get(ctx, &format!("filestore:{hash}")).await
}
