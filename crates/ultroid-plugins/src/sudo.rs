//! Sudo-user management — port of `plugins/sudo.py`.
//!
//! DB keys (raw udB keys, same as Python): `SUDOS` (list of user ids),
//! `SUDO` (`"True"` when sudo mode is on).
//!
//! User resolution: a numeric id / the current private chat resolve
//! locally; a `@username` or a replied message's sender needs a client
//! entity lookup, which the `Ctx` surface does not expose — those paths
//! return the honest `TODO(responder-op)` error instead of guessing.

use crate::util::{def, reg_add};
use serde_json::Value;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{ChatType, CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}addsudo`\n    Add Sudo Users by replying to user or using <space> separated userid(s)\n\n• `{i}delsudo`\n    Remove Sudo Users by replying to user or using <space> separated userid(s)\n\n• `{i}listsudo`\n    List all sudo users.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "addsudo",
            pattern: "addsudo( (.*)|$)",
            help: HELP,
            fullsudo: true,
            ..def("addsudo", "addsudo( (.*)|$)", HELP)
        },
        |ctx| Box::pin(add_sudo(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "delsudo",
            pattern: "delsudo( (.*)|$)",
            help: HELP,
            fullsudo: true,
            ..def("delsudo", "delsudo( (.*)|$)", HELP)
        },
        |ctx| Box::pin(del_sudo(ctx)),
    );
    reg_add(registry, def("listsudo", "listsudo$", HELP), |ctx| {
        Box::pin(list_sudo(ctx))
    });
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Markdown mention of a user id (Python `fns.inline_mention` when only
/// the id is known — no display name is available without a client fetch).
pub fn mention_id(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
}

/// Current sudo list (Python `pyUltroid._misc.sudoers` over udB `SUDOS`).
pub async fn sudoers(ctx: &Ctx) -> Result<Vec<i64>> {
    let v: Option<Vec<Value>> = db_get_json(&*ctx.db, "SUDOS").await?;
    Ok(v.unwrap_or_default()
        .into_iter()
        .filter_map(|x| x.as_i64())
        .collect())
}

async fn set_sudoers(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "SUDOS", &list.to_vec()).await
}

/// Resolve the target user for `.addsudo` / `.delsudo` (port of Python
/// `fns`-style resolution in `sudo.py`):
/// reply -> replied sender (needs fetch); numeric arg -> id;
/// `@username` -> needs resolve; private chat -> the chat itself.
fn resolve_target(ctx: &Ctx) -> Result<i64> {
    let inputs = ctx.args.trim();
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): get replied message -> sender id.
        // Python: plugins/sudo.py (addsudo/delsudo).
        return Err(todo_op(
            "get replied message sender",
            "plugins/sudo.py:addsudo",
        ));
    }
    if !inputs.is_empty() {
        let first = inputs.split_whitespace().next().unwrap_or("");
        if let Ok(id) = first.trim_start_matches('@').parse::<i64>() {
            return Ok(id);
        }
        // TODO(responder-op): resolve @username / text id via client.
        return Err(todo_op(
            "resolve user id/username",
            "plugins/sudo.py:addsudo",
        ));
    }
    if ctx.chat_type == ChatType::Private {
        return Ok(ctx.chat_id);
    }
    Err(Error::Other(ctx.get_str("sudo_1")))
}

async fn add_sudo(ctx: &Ctx) -> Result<()> {
    let id = resolve_target(ctx)?;
    // Bot/verified check needs the user entity — unresolvable here; the
    // numeric path has no entity to check (Python checks only after fetch).
    let name = mention_id(id);
    let owner_id = ctx.config.owner_id;
    if id == owner_id {
        ctx.reply(&ctx.get_str("sudo_2")).await?;
        return Ok(());
    }
    let mut list = sudoers(ctx).await?;
    if list.contains(&id) {
        ctx.reply(&format!("{name} `is already a SUDO User ...`"))
            .await?;
        return Ok(());
    }
    ctx.db.set("SUDO", "True").await?;
    list.push(id);
    set_sudoers(ctx, &list).await?;
    ctx.reply(&format!("**Added** {name} **as SUDO User**"))
        .await?;
    Ok(())
}

async fn del_sudo(ctx: &Ctx) -> Result<()> {
    let id = resolve_target(ctx)?;
    let name = mention_id(id);
    let mut list = sudoers(ctx).await?;
    if !list.contains(&id) {
        ctx.reply(&format!("{name} `wasn't a SUDO User ...`"))
            .await?;
        return Ok(());
    }
    list.retain(|x| *x != id);
    set_sudoers(ctx, &list).await?;
    ctx.reply(&format!("**Removed** {name} **from SUDO User(s)**"))
        .await?;
    Ok(())
}

async fn list_sudo(ctx: &Ctx) -> Result<()> {
    let sudos = sudoers(ctx).await?;
    if sudos.is_empty() {
        ctx.reply(&ctx.get_str("sudo_3")).await?;
        return Ok(());
    }
    let mut msg = String::new();
    for i in sudos {
        // Python resolves each id to a name via get_entity and falls back
        // to "`<id>` -> Invalid User" on failure; without a client fetch
        // the port uses the id fallback directly.
        msg.push_str(&format!("• `{i}` -> Invalid User\n"));
    }
    let sudo_mode: Option<String> = ctx.db.get("SUDO").await?;
    let mode = sudo_mode.unwrap_or_else(|| "True".to_string());
    let mode = if mode == "False" {
        "[False](https://graph.org/Ultroid-04-06)".to_string()
    } else {
        mode
    };
    ctx.reply(&format!(
        "**SUDO MODE : {mode}\n\nList of SUDO Users :**\n{msg}"
    ))
    .await?;
    Ok(())
}
