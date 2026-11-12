//! Port of `plugins/channelhacks.py` — channel autopost + bulk shift.
//!
//! `.shift <source> | <dest>` — copy every message from one channel to
//! another. `.asource`/`.dsource`/`.listsource` and
//! `.adest`/`.ddest`/`.listdest` — manage the autopost source/destination
//! lists (stored as JSON arrays under DB keys `CH_SOURCE` /
//! `CH_DESTINATIONS`).
//!
//! List management is fully real, source/destination display names come
//! from `Responder::chat_info`, and autopost forwards with
//! `Responder::forward_to`. Resolving a non-numeric username/`@handle`
//! to an id still needs a client call (`resolve_user` is user-only, so
//! it cannot resolve channels), and `.shift`'s message iteration has no
//! trait op — those are the remaining honest errors (see the
//! `TODO(responder-op)` site in `shift` and `resolve_id`).
//! The autopost watcher (`autopost`) must be called by the binary's
//! NewMessage path when the `AUTOPOST` key is set — `Registry` has no
//! listener surface.

use crate::util::{db_del, db_get, db_set, def, reg_add};
use serde::{Deserialize, Serialize};
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "shift",
            "shift (.*)",
            "Copy all messages from one channel to another: `.shift <src> | <dst>`.",
        ),
        |ctx| Box::pin(shift(ctx)),
    );
    reg_add(
        registry,
        def(
            "asource",
            "asource (.*)",
            "Add this (or the given) channel as an autopost source.",
        ),
        |ctx| Box::pin(asource(ctx)),
    );
    reg_add(
        registry,
        def(
            "dsource",
            "dsource( (.*)|$)",
            "Remove an autopost source (`.dsource all` clears them).",
        ),
        |ctx| Box::pin(dsource(ctx)),
    );
    reg_add(
        registry,
        def("listsource", "listsource", "List autopost source channels."),
        |ctx| Box::pin(listsource(ctx)),
    );
    reg_add(
        registry,
        def(
            "adest",
            "adest (.*)",
            "Add this (or the given) channel as an autopost destination.",
        ),
        |ctx| Box::pin(adest(ctx)),
    );
    reg_add(
        registry,
        def(
            "ddest",
            "ddest( (.*)|$)",
            "Remove an autopost destination (`.ddest all` clears them).",
        ),
        |ctx| Box::pin(ddest(ctx)),
    );
    reg_add(
        registry,
        def(
            "listdest",
            "listdest",
            "List autopost destination channels.",
        ),
        |ctx| Box::pin(listdest(ctx)),
    );
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IdList(Vec<i64>);

async fn load_list(ctx: &Ctx, key: &str) -> Result<IdList> {
    match db_get(ctx, key).await? {
        Some(v) => Ok(serde_json::from_str(&v).unwrap_or_default()),
        None => Ok(IdList::default()),
    }
}

async fn save_list(ctx: &Ctx, key: &str, list: &IdList) -> Result<()> {
    db_set(ctx, key, &serde_json::to_string(list)?).await
}

/// Resolve a numeric id; anything else needs client-side `parse_id`.
fn resolve_id(token: &str) -> std::result::Result<i64, Error> {
    token
        .trim()
        .trim_start_matches('@')
        .parse::<i64>()
        .map_err(|_| {
            Error::Other(format!(
                "resolve peer '{token}' to an id requires a binary Responder op (Python: channelhacks.py:_)"
            ))
        })
}

async fn shift(ctx: &Ctx) -> Result<()> {
    let Some((a, b)) = ctx.args.split_once('|') else {
        ctx.reply(&ctx.get_str("cha_1")).await?;
        return Ok(());
    };
    let _src = resolve_id(a.trim())?;
    let _dst = resolve_id(b.trim())?;
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): iterate messages in `src` (oldest first) and
    // send each to `dst`, sleeping 2s between sends and honouring
    // FloodWait (Python: channelhacks.py:_,
    // `iter_messages(src, reverse=True)` + `send_message(dst, msg)`).
    Err(Error::Other(
        "shift (iterate + forward channel messages) requires a binary Responder op (Python: channelhacks.py:_)".to_string(),
    ))
}

async fn asource(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let id = if arg.is_empty() {
        ctx.chat_id
    } else {
        resolve_id(arg)?
    };
    let mut list = load_list(ctx, "CH_SOURCE").await?;
    if list.0.contains(&id) {
        ctx.reply(&ctx.get_str("cha_3")).await?;
    } else {
        list.0.push(id);
        save_list(ctx, "CH_SOURCE", &list).await?;
        ctx.reply(&ctx.get_str("cha_2")).await?;
    }
    Ok(())
}

async fn dsource(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    if arg == "all" {
        ctx.edit(id, &ctx.get_str("bd_8")).await?;
        db_del(ctx, "CH_SOURCE").await?;
        ctx.edit(id, &ctx.get_str("cha_4")).await?;
        return Ok(());
    }
    let target = if arg.is_empty() {
        ctx.chat_id
    } else {
        resolve_id(arg)?
    };
    let mut list = load_list(ctx, "CH_SOURCE").await?;
    if list.0.contains(&target) {
        list.0.retain(|x| *x != target);
        save_list(ctx, "CH_SOURCE", &list).await?;
        ctx.edit(id, &ctx.get_str("cha_5")).await?;
    } else {
        ctx.edit(id, "Source channel is already removed from database. ")
            .await?;
    }
    Ok(())
}

async fn listsource(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    let list = load_list(ctx, "CH_SOURCE").await?;
    if list.0.is_empty() {
        ctx.edit(id, "No chats were added.").await?;
        return Ok(());
    }
    let mut msg = ctx.get_str("cha_8");
    for channel in &list.0 {
        // Display name via chat_info; fall back to the id alone when the
        // chat cannot be resolved.
        let title = ctx
            .responder
            .chat_info(*channel)
            .await
            .map(|c| c.title)
            .unwrap_or_default();
        if title.is_empty() {
            msg.push_str(&format!("\n=> **** [`{channel}`]"));
        } else {
            msg.push_str(&format!("\n=> **{title}** [`{channel}`]"));
        }
    }
    msg.push_str(&format!("\nTotal {} channels.", list.0.len()));
    ctx.edit(id, &msg).await?;
    Ok(())
}

async fn adest(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let id = if arg.is_empty() {
        ctx.chat_id
    } else {
        resolve_id(arg)?
    };
    let mut list = load_list(ctx, "CH_DESTINATIONS").await?;
    if list.0.contains(&id) {
        ctx.reply("Destination channel already added").await?;
    } else {
        list.0.push(id);
        save_list(ctx, "CH_DESTINATIONS", &list).await?;
        ctx.reply("Destination added succesfully").await?;
    }
    Ok(())
}

async fn ddest(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    if arg == "all" {
        ctx.edit(id, &ctx.get_str("bd_8")).await?;
        db_del(ctx, "CH_DESTINATIONS").await?;
        ctx.edit(id, "Destinations database cleared.").await?;
        return Ok(());
    }
    let target = if arg.is_empty() {
        ctx.chat_id
    } else {
        resolve_id(arg)?
    };
    let mut list = load_list(ctx, "CH_DESTINATIONS").await?;
    if list.0.contains(&target) {
        list.0.retain(|x| *x != target);
        save_list(ctx, "CH_DESTINATIONS", &list).await?;
        ctx.edit(id, "Destination removed from database").await?;
    } else {
        ctx.edit(id, "Destination channel is already removed from database. ")
            .await?;
    }
    Ok(())
}

async fn listdest(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    let list = load_list(ctx, "CH_DESTINATIONS").await?;
    if list.0.is_empty() {
        ctx.edit(id, "No chats were added.").await?;
        return Ok(());
    }
    let mut msg = ctx.get_str("cha_7");
    for channel in &list.0 {
        // Display name via chat_info; fall back to the id alone when the
        // chat cannot be resolved.
        let title = ctx
            .responder
            .chat_info(*channel)
            .await
            .map(|c| c.title)
            .unwrap_or_default();
        if title.is_empty() {
            msg.push_str(&format!("\n=> **** [`{channel}`]"));
        } else {
            msg.push_str(&format!("\n=> **{title}** [`{channel}`]"));
        }
    }
    msg.push_str(&format!("\nTotal {} channels.", list.0.len()));
    ctx.edit(id, &msg).await?;
    Ok(())
}

/// Autopost watcher (Python `autopost_func`): when the `AUTOPOST` key is
/// set and this message arrived in a listed source channel, forward it
/// to every destination. Returns `Ok(true)` when it forwarded.
pub async fn autopost(ctx: &Ctx) -> Result<bool> {
    if db_get(ctx, "AUTOPOST").await?.is_none() {
        return Ok(false);
    }
    let sources = load_list(ctx, "CH_SOURCE").await?;
    if !sources.0.contains(&ctx.chat_id) {
        return Ok(false);
    }
    let destinations = load_list(ctx, "CH_DESTINATIONS").await?;
    for dest in &destinations.0 {
        ctx.responder
            .forward_to(ctx.chat_id, ctx.message_id as i32, *dest)
            .await?;
    }
    Ok(true)
}
