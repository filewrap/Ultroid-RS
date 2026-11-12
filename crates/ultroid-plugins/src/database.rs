//! Raw database commands — port of `plugins/database.py`
//! (`.setdb` / `.deldb` / `.rendb`).
//!
//! Fully portable: raw udB keys over `ctx.db`.
//!
//! * `.setdb key value` / `.setdb key | value` — delimiter is `" | "` when
//!   the input contains `|`, else a single space. `--extend` / `-e` reads
//!   the current value and appends (Python: `f"{old} {new}"`).
//! * `.deldb key` / `.deldb -m key1 key2 ...` — deletes one or many keys.
//!   `Database::del` returns no count, so existence is checked first to
//!   reproduce Python's "No Such Key." reply.
//! * `.rendb old | new` — `Database` has no rename, so it is implemented
//!   as get + set + del, which is exactly what `udB.rename` does at the
//!   key/value level.

use crate::util::{db_del, db_get, db_set, reg_add};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

fn split_delim(input: &str) -> Vec<String> {
    let delim = if input.contains('|') { " | " } else { " " };
    input.splitn(2, delim).map(|s| s.to_string()).collect()
}

/// `.setdb key value` — port of `database.py` setdb handler.
async fn setdb(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim();
    if input.is_empty() {
        return ctx.reply("Provide key and value to set!").await.map(|_| ());
    }
    let mut data = split_delim(input);
    if data.len() < 2 {
        return ctx.reply(&ctx.get_str("com_7")).await.map(|_| ());
    }
    if data[0] == "--extend" || data[0] == "-e" {
        let rest: Vec<String> = data[1]
            .splitn(2, char::is_whitespace)
            .map(|s| s.to_string())
            .collect();
        if rest.len() < 2 {
            return ctx.reply(&ctx.get_str("com_7")).await.map(|_| ());
        }
        let old = db_get(ctx, &rest[0]).await?.unwrap_or_default();
        data = vec![rest[0].clone(), format!("{old} {}", rest[1])];
    }
    db_set(ctx, &data[0], &data[1]).await?;
    ctx.reply(&format!(
        "**DB Key Value Pair Updated\nKey :** `{}`\n**Value :** `{}`",
        data[0], data[1]
    ))
    .await
    .map(|_| ())
}

/// `.deldb key` / `.deldb -m k1 k2` — port of `database.py` deldb handler.
async fn deldb(ctx: &Ctx) -> Result<()> {
    let key = ctx.args.trim();
    if key.is_empty() {
        return ctx.reply("Give me a key name to delete!").await.map(|_| ());
    }
    let parts: Vec<&str> = key.splitn(2, char::is_whitespace).collect();
    if parts[0] == "-m" {
        let keys: Vec<&str> = parts
            .get(1)
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();
        let mut deleted = Vec::new();
        for k in &keys {
            if ctx.db.exists(k).await? {
                db_del(ctx, k).await?;
                deleted.push(*k);
            }
        }
        if deleted.is_empty() {
            return ctx.reply("`No Such Key.`").await.map(|_| ());
        }
        return ctx
            .reply(&format!("`Successfully deleted key {}`", deleted.join(" ")))
            .await
            .map(|_| ());
    }
    if !ctx.db.exists(key).await? {
        return ctx.reply("`No Such Key.`").await.map(|_| ());
    }
    db_del(ctx, key).await?;
    ctx.reply(&format!("`Successfully deleted key {key}`"))
        .await
        .map(|_| ())
}

/// `.rendb old | new` — port of `database.py` rendb handler.
async fn rendb(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim();
    if input.is_empty() {
        return ctx
            .reply("`Provide Keys name to rename..`")
            .await
            .map(|_| ());
    }
    let data = split_delim(input);
    if data.len() < 2 {
        return ctx.reply(&ctx.get_str("com_7")).await.map(|_| ());
    }
    if ctx.db.exists(&data[0]).await? {
        let value = db_get(ctx, &data[0]).await?.unwrap_or_default();
        db_set(ctx, &data[1], &value).await?;
        db_del(ctx, &data[0]).await?;
        ctx.reply(&format!(
            "**DB Key Rename Successful\nOld Key :** `{}`\n**New Key :** `{}`",
            data[0], data[1]
        ))
        .await
        .map(|_| ())
    } else {
        ctx.reply("Key not found").await.map(|_| ())
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "setdb",
            pattern: "setdb( (.*)|$)",
            help: "Set a raw database key/value pair.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(setdb(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "deldb",
            pattern: "deldb( (.*)|$)",
            help: "Delete a raw database key (or several with -m).",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(deldb(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "rendb",
            pattern: "rendb( (.*)|$)",
            help: "Rename a raw database key.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(rendb(ctx)),
    );
}
