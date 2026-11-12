//! Port of `plugins/redis.py` — raw DB (Redis) key management.
//!
//! `.setredis key | value` (or `key value`), `.delredis key`,
//! `.renredis old | new`. Full-sudo gated, like the Python version
//! (non-owner callers are refused by the dispatcher via `fullsudo`).

use crate::util::{db_del, db_get, db_set, def, reg_add};
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut set = def(
        "setredis",
        "setredis ?(.*)",
        "Set a DB key: `.setredis key | value`.",
    );
    set.fullsudo = true;
    reg_add(registry, set, |ctx| Box::pin(setredis(ctx)));

    let mut del = def("delredis", "delredis ?(.*)", "Delete a DB key.");
    del.fullsudo = true;
    reg_add(registry, del, |ctx| Box::pin(delredis(ctx)));

    let mut ren = def(
        "renredis",
        "renredis ?(.*)",
        "Rename a DB key: `.renredis old | new`.",
    );
    ren.fullsudo = true;
    reg_add(registry, ren, |ctx| Box::pin(renredis(ctx)));
}

/// Split `key | value` (or `key value`) exactly like the Python version:
/// ` | ` when a pipe is present, otherwise a single space.
fn split_pair(arg: &str) -> Option<(String, String)> {
    let delim = if arg.contains('|') { " | " } else { " " };
    let mut it = arg.splitn(2, delim);
    let key = it.next()?.trim().to_string();
    let val = it.next().unwrap_or("").trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, val))
}

async fn setredis(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`...`").await?;
    match split_pair(&ctx.args) {
        Some((key, val)) => {
            db_set(ctx, &key, &val).await?;
            let stored = db_get(ctx, &key).await?.unwrap_or_default();
            ctx.edit(
                id,
                &format!("Redis Key Value Pair Updated\nKey : `{key}`\nValue : `{stored}`"),
            )
            .await?;
        }
        None => {
            ctx.edit(id, "`Something Went Wrong`").await?;
        }
    }
    Ok(())
}

async fn delredis(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`Deleting data from Redis ...`").await?;
    let key = ctx.args.trim();
    if key.is_empty() {
        ctx.edit(id, "`Something Went Wrong`").await?;
        return Ok(());
    }
    db_del(ctx, key).await?;
    ctx.edit(id, &format!("`Successfully deleted key {key}`"))
        .await?;
    Ok(())
}

async fn renredis(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`...`").await?;
    match split_pair(&ctx.args) {
        Some((old, new)) => {
            if new.is_empty() {
                ctx.edit(id, "`Something Went Wrong`").await?;
                return Ok(());
            }
            match db_get(ctx, &old).await? {
                Some(val) => {
                    // The `Database` trait has no rename; copy + delete is
                    // the same observable operation.
                    db_set(ctx, &new, &val).await?;
                    db_del(ctx, &old).await?;
                    ctx.edit(
                        id,
                        &format!(
                            "Redis Key Rename Successful\nOld Key : `{old}`\nNew Key : `{new}`"
                        ),
                    )
                    .await?;
                }
                None => {
                    ctx.edit(id, "Key not found").await?;
                }
            }
        }
        None => {
            ctx.edit(id, "Something went wrong ...").await?;
        }
    }
    Ok(())
}
