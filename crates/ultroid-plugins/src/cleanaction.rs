//! Clean chat actions — port of `plugins/cleanaction.py`.
//!
//! DB key (raw udB key, same as Python): `CLEANCHAT` is a list of chat
//! ids where service/action messages (joins, leaves, pins, …) are deleted
//! on sight. The deletion itself lives in the `ChatAction` listener
//! (`chatactions::handle_chat_action`, `clean_chat` flag) — this module
//! owns the three management commands, all fully ported. Listing resolves
//! chat titles via `get_entity` in Python and falls back to a placeholder
//! on failure; without a client fetch the port lists the ids directly.

use crate::util::{def, reg_add};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

pub const HELP: &str = " -\n\n-`{i}addclean`\n    Clean all Upcoming action msg in added chat like someone joined/left/pin etc.\n\n-`{i}remclean`\n    Remove chat from database.\n\n-`{i}listclean`\n   To get list of all chats where its activated.\n\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "addclean",
            pattern: "addclean$",
            help: HELP,
            admins_only: true,
            ..def("addclean", "addclean$", HELP)
        },
        |ctx| Box::pin(add_clean(ctx)),
    );
    reg_add(registry, def("remclean", "remclean$", HELP), |ctx| {
        Box::pin(remove_clean(ctx))
    });
    reg_add(registry, def("listclean", "listclean$", HELP), |ctx| {
        Box::pin(list_clean(ctx))
    });
}

/// Chats with clean-action on (Python `udB.get_key("CLEANCHAT")`).
pub async fn clean_chats(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(db_get_json(&*ctx.db, "CLEANCHAT")
        .await?
        .unwrap_or_default())
}

async fn set_clean_chats(ctx: &Ctx, chats: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "CLEANCHAT", &chats.to_vec()).await
}

async fn add_clean(ctx: &Ctx) -> Result<()> {
    let mut key = clean_chats(ctx).await?;
    if key.contains(&ctx.chat_id) {
        ctx.reply(&ctx.get_str("clan_5")).await?;
        return Ok(());
    }
    key.push(ctx.chat_id);
    set_clean_chats(ctx, &key).await?;
    ctx.reply(&ctx.get_str("clan_1")).await?;
    Ok(())
}

async fn remove_clean(ctx: &Ctx) -> Result<()> {
    let mut key = clean_chats(ctx).await?;
    if key.contains(&ctx.chat_id) {
        key.retain(|c| *c != ctx.chat_id);
        set_clean_chats(ctx, &key).await?;
    }
    ctx.reply(&ctx.get_str("clan_2")).await?;
    Ok(())
}

async fn list_clean(ctx: &Ctx) -> Result<()> {
    let chats = clean_chats(ctx).await?;
    if chats.is_empty() {
        ctx.reply(&ctx.get_str("clan_4")).await?;
        return Ok(());
    }
    let mut out = String::new();
    for x in chats {
        out.push_str(&format!("{x}\n"));
    }
    ctx.reply(&out).await?;
    Ok(())
}
