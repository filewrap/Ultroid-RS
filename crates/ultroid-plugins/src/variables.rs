//! Port of `plugins/variables.py` — read variables / DB keys.
//!
//! `.get var <name>` — value of a DB key or env var.
//! `.get type <name>` — whether the name is a DB key or an env var.
//! `.get db <key>` — raw DB value for a key.
//! `.get keys` — list all DB keys (filtered like the Python version).

use crate::util::{db_get, def, reg_add};
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut d = def(
        "get",
        "get($| (.*))",
        "Get value/type of a variable or DB key; `.get keys` lists all DB keys.",
    );
    d.fullsudo = true;
    reg_add(registry, d, |ctx| Box::pin(get_var(ctx)));
}

async fn get_var(ctx: &Ctx) -> Result<()> {
    let mut parts = ctx.args.splitn(2, char::is_whitespace);
    let opt = parts.next().unwrap_or("").trim();
    if opt.is_empty() {
        ctx.reply("what to get?\nRead `.help variables`").await?;
        return Ok(());
    }
    if opt != "keys" {
        let Some(varname) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
            let id = ctx.reply(&ctx.get_str("com_1")).await?;
            ctx.edit(id, "Such a var doesn't exist!").await?;
            return Ok(());
        };
        let varname = varname.to_string();
        let id = ctx.reply(&ctx.get_str("com_1")).await?;
        match opt {
            "var" => {
                let mut found = 0;
                if let Some(val) = db_get(ctx, &varname).await? {
                    found += 1;
                    ctx.edit(
                        id,
                        &format!(
                            "**Variable** - `{varname}`\n**Value**: `{val}`\n**Type**: Redis Key."
                        ),
                    )
                    .await?;
                }
                if let Ok(val) = std::env::var(&varname) {
                    found += 1;
                    ctx.edit(
                        id,
                        &format!(
                            "**Variable** - `{varname}`\n**Value**: `{val}`\n**Type**: Env Var."
                        ),
                    )
                    .await?;
                }
                if found == 0 {
                    ctx.edit(id, "Such a var doesn't exist!").await?;
                }
            }
            "type" => {
                let mut found = 0;
                if db_get(ctx, &varname).await?.is_some() {
                    found += 1;
                    ctx.edit(
                        id,
                        &format!("**Variable** - `{varname}`\n**Type**: Redis Key."),
                    )
                    .await?;
                }
                if std::env::var(&varname).is_ok() {
                    found += 1;
                    ctx.edit(
                        id,
                        &format!("**Variable** - `{varname}`\n**Type**: Env Var."),
                    )
                    .await?;
                }
                if found == 0 {
                    ctx.edit(id, "Such a var doesn't exist!").await?;
                }
            }
            "db" => match db_get(ctx, &varname).await? {
                Some(val) => {
                    ctx.edit(id, &format!("**Key** - `{varname}`\n**Value**: `{val}`"))
                        .await?;
                }
                None => {
                    ctx.edit(id, "No such key!").await?;
                }
            },
            _ => {
                ctx.edit(id, "Such a var doesn't exist!").await?;
            }
        }
        return Ok(());
    }

    // `.get keys`
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    let mut keys = ctx.db.keys("").await?;
    keys.sort();
    let mut msg = String::new();
    for k in keys {
        if k.chars().all(|c| c.is_ascii_digit())
            || k.starts_with('-')
            || k.starts_with('_')
            || k.starts_with("GBAN_REASON_")
        {
            continue;
        }
        msg.push_str(&format!("• `{k}`\n"));
    }
    ctx.edit(id, &format!("**List of DB Keys :**\n{msg}"))
        .await?;
    Ok(())
}
