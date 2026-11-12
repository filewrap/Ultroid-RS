//! Global admin tools — port of `plugins/globaltools.py`.
//!
//! DB keys (raw udB keys, same as Python `pyUltroid/dB/gban_mute_db.py`
//! and `KeyManager("GBLACKLISTS")`):
//! `GBAN` maps user id -> reason, `GMUTE` is a list of user ids,
//! `GBLACKLISTS` is a list of chat ids excluded from global broadcasts.
//!
//! All argument parsing, DB reads/writes and reply text is ported.
//! Dialog iteration + promote / demote / kick / mute / unmute /
//! broadcasts / the gbanned-list file are wired to the `Responder` ops.
//! `gban` / `ungban` still need `contacts.Block` / `UnblockRequest`,
//! which the trait does not expose, and `gadmincast` still needs a
//! per-chat admin-rights check the trait does not expose, so those
//! sites return the honest `TODO(responder-op)` error at the exact
//! point the RPC would run.
//!
//! Note the patterns are kept exactly as written in Python, including the
//! literal `dbs.gban_mute.<cmd>` prefixes on `gban`/`ungban`/`gmute`/
//! `ungmute` (a quirk of the source: the regex `.` matches any char, so
//! the effective trigger is e.g. `gban` with any two leading chars).

use crate::util::{def, reg_add};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::tg::InlineButton;
use ultroid_core::{AdminRights, BannedRights, ChatKind, CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}gban <reply user/ username>`\n• `{i}ungban`\n    Ban/Unban Globally.\n\n• `{i}gstat <reply to user/userid/username>`\n   Check if user is GBanned.\n\n• `{i}listgban` : List all GBanned users.\n\n• `{i}gmute` | `{i}ungmute` <reply user/ username>\n    Mute/UnMute Globally.\n\n• `{i}gkick <reply/username>` `Globally Kick User`\n• `{i}gcast <text/reply>` `Globally Send msg in all grps`\n\n• `{i}gadmincast <text/reply>` `Globally broadcast in your admin chats`\n• `{i}gucast <text/reply>` `Globally send msg in all pm users`\n\n• `{i}gblacklist <chat id/username/nothing (for current chat)`\n   Add chat to blacklist and ignores global broadcast.\n• `{i}ungblacklist` `Remove the chat from blacklist.`\n\n• `{i}gpromote <reply to user> <channel/group/all> <rank>`\n    globally promote user where you are admin\n    - Set whether To promote only in groups/channels/all.\n      Eg- `gpromote group boss` ~ promotes user in all grps.\n        `gpromote @username all sar` ~ promote the user in all group & channel\n• `{i}gdemote` - `demote user globally`\n";

pub fn register(registry: &mut Registry) {
    let fullsudo = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        fullsudo: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, fullsudo("gpromote", "gpromote( (.*)|$)"), |ctx| {
        Box::pin(gpromote(ctx))
    });
    reg_add(registry, fullsudo("gdemote", "gdemote( (.*)|$)"), |ctx| {
        Box::pin(gdemote(ctx))
    });
    reg_add(
        registry,
        fullsudo("ungban", "dbs.gban_mute.ungban( (.*)|$)"),
        |ctx| Box::pin(ungban(ctx)),
    );
    reg_add(
        registry,
        fullsudo("gban", "dbs.gban_mute.gban( (.*)|$)"),
        |ctx| Box::pin(gban(ctx)),
    );
    reg_add(
        registry,
        fullsudo("gcast", "g(admin|)cast( (.*)|$)"),
        |ctx| Box::pin(gcast(ctx)),
    );
    reg_add(registry, fullsudo("gucast", "gucast( (.*)|$)"), |ctx| {
        Box::pin(gucast(ctx))
    });
    reg_add(registry, fullsudo("gkick", "gkick( (.*)|$)"), |ctx| {
        Box::pin(gkick(ctx))
    });
    reg_add(
        registry,
        fullsudo("gmute", "dbs.gban_mute.gmute( (.*)|$)"),
        |ctx| Box::pin(gmute(ctx)),
    );
    reg_add(
        registry,
        fullsudo("ungmute", "dbs.gban_mute.ungmute( (.*)|$)"),
        |ctx| Box::pin(ungmute(ctx)),
    );
    reg_add(registry, def("listgban", "listgban$", HELP), |ctx| {
        Box::pin(list_gban(ctx))
    });
    reg_add(registry, def("gstat", "gstat( (.*)|$)", HELP), |ctx| {
        Box::pin(gstat(ctx))
    });
    reg_add(registry, def("gblacklist", "gblacklist$", HELP), |ctx| {
        Box::pin(gblacklist(ctx))
    });
    reg_add(
        registry,
        def("ungblacklist", "ungblacklist$", HELP),
        |ctx| Box::pin(ungblacklist(ctx)),
    );
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Markdown mention of a user id (Python `fns.inline_mention` with only
/// the id known).
pub fn mention_id(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
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

/// GBAN map (Python `dbs.gban_mute.list_gbanned`): user id -> reason.
pub async fn gbanned(ctx: &Ctx) -> Result<HashMap<String, String>> {
    Ok(db_get_json(&*ctx.db, "GBAN").await?.unwrap_or_default())
}

async fn set_gbanned(ctx: &Ctx, map: &HashMap<String, String>) -> Result<()> {
    db_set_json(&*ctx.db, "GBAN", map).await
}

/// GMUTE list (Python `dbs.gban_mute.list_gmuted`).
pub async fn gmuted(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(db_get_json(&*ctx.db, "GMUTE").await?.unwrap_or_default())
}

async fn set_gmuted(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "GMUTE", &list.to_vec()).await
}

pub async fn is_gbanned(ctx: &Ctx, user_id: i64) -> Result<Option<String>> {
    Ok(gbanned(ctx).await?.get(&user_id.to_string()).cloned())
}

pub async fn is_gmuted(ctx: &Ctx, user_id: i64) -> Result<bool> {
    Ok(gmuted(ctx).await?.contains(&user_id))
}

/// GBLACKLISTS list (Python `KeyManager("GBLACKLISTS")`).
pub async fn gblacklists(ctx: &Ctx) -> Result<Vec<i64>> {
    Ok(db_get_json(&*ctx.db, "GBLACKLISTS")
        .await?
        .unwrap_or_default())
}

async fn set_gblacklists(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(&*ctx.db, "GBLACKLISTS", &list.to_vec()).await
}

/// Resolve a target user id from a reply or an argument (port of the
/// repeated pattern in `globaltools.py`): reply -> sender (needs fetch);
/// numeric arg -> id; `@username`/text -> needs resolve.
enum Target {
    Resolved(i64),
    NeedReplySender,
    NeedResolve,
    Missing,
}

fn resolve_target(ctx: &Ctx) -> Target {
    if ctx.reply_to_id.is_some() {
        return Target::NeedReplySender;
    }
    let arg = ctx.args.trim();
    if arg.is_empty() {
        if ctx.chat_type == ultroid_core::ChatType::Private {
            return Target::Resolved(ctx.chat_id);
        }
        return Target::Missing;
    }
    let first = arg.split_whitespace().next().unwrap_or("");
    match first.trim_start_matches('@').parse::<i64>() {
        Ok(id) => Target::Resolved(id),
        Err(_) => Target::NeedResolve,
    }
}

/// Resolve a target user id: reply -> the replied message's sender
/// (from `ctx.reply_to_sender_id`); numeric arg -> id; `@username` /
/// text id -> `resolve_user`.
async fn target_or_todo(ctx: &Ctx, _py_fn: &str) -> Result<i64> {
    match resolve_target(ctx) {
        Target::Resolved(id) => Ok(id),
        Target::NeedReplySender => ctx.reply_to_sender_id.ok_or_else(|| {
            Error::Other("could not determine the replied message's sender".to_string())
        }),
        Target::NeedResolve => {
            let first = ctx.args.trim().split_whitespace().next().unwrap_or("");
            Ok(ctx.responder.resolve_user(first).await?.id)
        }
        Target::Missing => Err(Error::Other("missing_target".to_string())),
    }
}

/// The chat kind of a dialog, via `chat_info` (`DialogInfo` itself
/// carries no kind). `None` when the fetch fails.
async fn dialog_kind(ctx: &Ctx, dialog_id: i64) -> Option<ChatKind> {
    ctx.responder
        .chat_info(dialog_id)
        .await
        .ok()
        .map(|c| c.kind)
}

/// Is this dialog a group (plain or super) or a channel, per Python's
/// `Dialog.is_group or Dialog.is_channel`?
async fn is_group_or_channel(ctx: &Ctx, dialog_id: i64) -> bool {
    matches!(
        dialog_kind(ctx, dialog_id).await,
        Some(ChatKind::Group) | Some(ChatKind::Supergroup) | Some(ChatKind::Channel)
    )
}

/// Kick a user: Telethon `kick_participant` is a ban followed by an
/// immediate unban.
async fn kick_user(ctx: &Ctx, chat_id: i64, user_id: i64) -> Result<()> {
    ctx.responder
        .ban_user(chat_id, user_id, BannedRights::all_banned())
        .await?;
    ctx.responder.unban_user(chat_id, user_id).await
}

/// The mute rights Python uses everywhere (`edit_permissions(...,
/// send_messages=False)`): only the send-messages right is banned,
/// view-messages stays open.
fn mute_rights(until: Option<i32>) -> BannedRights {
    BannedRights {
        until_date: until,
        send_messages: true,
        ..Default::default()
    }
}

/// The fixed global-promote rights (Python `_gpromote_rights`).
fn gpromote_rights() -> AdminRights {
    AdminRights {
        invite_users: true,
        ban_users: true,
        delete_messages: true,
        pin_messages: true,
        ..Default::default()
    }
}

/// Parse `[label | url]` button JSON (from `split_msg_button`) into
/// inline URL buttons. `None` when there are no buttons or the JSON
/// does not parse.
fn parse_buttons(btn_json: &str) -> Option<Vec<Vec<InlineButton>>> {
    let rows: Vec<Vec<(String, String)>> = serde_json::from_str(btn_json).ok()?;
    Some(
        rows.into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(label, url)| InlineButton::url(label, url))
                    .collect()
            })
            .collect(),
    )
}

/// Send a broadcast text (+ optional buttons) to one dialog, returning
/// `true` on success. Per-chat errors are swallowed by the caller,
/// exactly like Python's per-dialog `try/except: pass`.
async fn broadcast_send(ctx: &Ctx, dialog_id: i64, msg: &str, btn: &Option<String>) -> bool {
    let res = match btn.as_deref().and_then(parse_buttons) {
        Some(buttons) => ctx.responder.send_markup_to(dialog_id, msg, buttons).await,
        None => ctx.responder.send_to(dialog_id, msg).await,
    };
    res.is_ok()
}

/// The scope key for gpromote/gdemote: "group", "channel" or "all".
/// Parsed from the token after the command (reply path) or after the
/// username (argument path), exactly like Python.
fn promote_scope(tokens: &[&str], key_index: usize) -> &'static str {
    if let Some(tok) = tokens.get(key_index) {
        if tok.contains("group") {
            return "group";
        }
        if tok.contains("channel") {
            return "channel";
        }
    }
    "all"
}

async fn gpromote(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_string();
    if arg.is_empty() {
        ctx.reply(&ctx.get_str("schdl_2")).await?;
        return Ok(());
    }
    let (user_id, key, rank) = if ctx.reply_to_id.is_some() {
        let tokens: Vec<&str> = ctx.text.split_whitespace().collect();
        let key = promote_scope(&tokens, 1);
        let rank = tokens.get(2).copied().unwrap_or("AdMin");
        let user_id = ctx.reply_to_sender_id.ok_or_else(|| {
            Error::Other("could not determine the replied message's sender".to_string())
        })?;
        (user_id, key, rank.to_string())
    } else {
        let tokens: Vec<&str> = ctx.text.split_whitespace().collect();
        let user_tok = tokens.get(1).copied().unwrap_or("");
        let user_id = match user_tok.trim_start_matches('@').parse::<i64>() {
            Ok(id) => id,
            Err(_) => ctx.responder.resolve_user(user_tok).await?.id,
        };
        let key = promote_scope(&tokens, 2);
        let rank = tokens.get(3).copied().unwrap_or("AdMin");
        (user_id, key, rank.to_string())
    };
    // Python iterates dialogs, promotes in the scoped chats and swallows
    // per-chat errors, counting successes.
    let mut _count = 0;
    for dialog in ctx.responder.dialogs().await? {
        let kind = dialog_kind(ctx, dialog.id).await;
        let in_scope = match key {
            "group" => matches!(kind, Some(ChatKind::Group) | Some(ChatKind::Supergroup)),
            "channel" => matches!(kind, Some(ChatKind::Channel)),
            _ => matches!(
                kind,
                Some(ChatKind::Group) | Some(ChatKind::Supergroup) | Some(ChatKind::Channel)
            ),
        };
        if !in_scope {
            continue;
        }
        if ctx
            .responder
            .promote_user(dialog.id, user_id, gpromote_rights(), &rank)
            .await
            .is_ok()
        {
            _count += 1;
        }
    }
    Ok(())
}

async fn gdemote(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_string();
    if arg.is_empty() {
        ctx.reply(&ctx.get_str("schdl_2")).await?;
        return Ok(());
    }
    let (user_id, key) = if ctx.reply_to_id.is_some() {
        let tokens: Vec<&str> = ctx.text.split_whitespace().collect();
        let key = promote_scope(&tokens, 1);
        let user_id = ctx.reply_to_sender_id.ok_or_else(|| {
            Error::Other("could not determine the replied message's sender".to_string())
        })?;
        (user_id, key)
    } else {
        let tokens: Vec<&str> = ctx.text.split_whitespace().collect();
        let user_tok = tokens.get(1).copied().unwrap_or("");
        let user_id = match user_tok.trim_start_matches('@').parse::<i64>() {
            Ok(id) => id,
            Err(_) => ctx.responder.resolve_user(user_tok).await?.id,
        };
        (user_id, promote_scope(&tokens, 2))
    };
    // Python demotes with all-false rights and the fixed rank "Not AdMin".
    let mut _count = 0;
    for dialog in ctx.responder.dialogs().await? {
        let kind = dialog_kind(ctx, dialog.id).await;
        let in_scope = match key {
            "group" => matches!(kind, Some(ChatKind::Group) | Some(ChatKind::Supergroup)),
            "channel" => matches!(kind, Some(ChatKind::Channel)),
            _ => matches!(
                kind,
                Some(ChatKind::Group) | Some(ChatKind::Supergroup) | Some(ChatKind::Channel)
            ),
        };
        if !in_scope {
            continue;
        }
        if ctx
            .responder
            .promote_user(dialog.id, user_id, AdminRights::default(), "Not AdMin")
            .await
            .is_ok()
        {
            _count += 1;
        }
    }
    Ok(())
}

async fn gban(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Gbanning...`").await?;
    // Reason: text after the user token, or after the command on reply.
    let reason = if ctx.reply_to_id.is_some() {
        ctx.text
            .splitn(2, ' ')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        ctx.text
            .splitn(3, ' ')
            .nth(2)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:gban").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if user_id == ctx.config.owner_id {
        ctx.reply("`I can't gban myself.`").await?;
        return Ok(());
    }
    if ctx.config.devlist_ids.contains(&user_id) {
        ctx.reply("`I can't gban my Developers.`").await?;
        return Ok(());
    }
    if is_gbanned(ctx, user_id).await?.is_some() {
        ctx.reply("`User is already gbanned and added to gbanwatch.`")
            .await?;
        return Ok(());
    }
    let _ = reason;
    // TODO(responder-op): iter dialogs + edit_permissions + BlockRequest.
    // Python: plugins/globaltools.py:gban handler.
    Err(todo_op(
        "global ban (iter dialogs + edit_permissions + block)",
        "plugins/globaltools.py:gban",
    ))
}

async fn ungban(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`UnGbanning...`").await?;
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:ungban").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if is_gbanned(ctx, user_id).await?.is_none() {
        ctx.reply("`User/Channel is not Gbanned...`").await?;
        return Ok(());
    }
    // TODO(responder-op): iter dialogs + edit_permissions + UnblockRequest.
    Err(todo_op(
        "global unban (iter dialogs + edit_permissions + unblock)",
        "plugins/globaltools.py:ungban",
    ))
}

async fn gkick(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Gkicking...`").await?;
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:gkick").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if user_id == ctx.config.owner_id {
        ctx.reply("`I can't gkick myself.`").await?;
        return Ok(());
    }
    if ctx.config.devlist_ids.contains(&user_id) {
        ctx.reply("`I can't gkick my Developers.`").await?;
        return Ok(());
    }
    // Python: kick_participant per group/channel dialog, swallowing
    // per-chat errors.
    for dialog in ctx.responder.dialogs().await? {
        if !is_group_or_channel(ctx, dialog.id).await {
            continue;
        }
        let _ = kick_user(ctx, dialog.id, user_id).await;
    }
    Ok(())
}

async fn gmute(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`Gmuting...`").await?;
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:gmute").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if user_id == ctx.config.owner_id {
        ctx.reply("`I can't gmute myself.`").await?;
        return Ok(());
    }
    if ctx.config.devlist_ids.contains(&user_id) {
        ctx.reply("`I can't gmute my Developers.`").await?;
        return Ok(());
    }
    if is_gmuted(ctx, user_id).await? {
        ctx.reply("`User is already gmuted.`").await?;
        return Ok(());
    }
    // Python: edit_permissions(send_messages=False) per group dialog,
    // swallowing per-chat errors, then record the gmute.
    for dialog in ctx.responder.dialogs().await? {
        if !matches!(
            dialog_kind(ctx, dialog.id).await,
            Some(ChatKind::Group) | Some(ChatKind::Supergroup)
        ) {
            continue;
        }
        let _ = ctx
            .responder
            .ban_user(dialog.id, user_id, mute_rights(None))
            .await;
    }
    let mut muted = gmuted(ctx).await?;
    if !muted.contains(&user_id) {
        muted.push(user_id);
    }
    set_gmuted(ctx, &muted).await?;
    Ok(())
}

async fn ungmute(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply("`UnGmuting...`").await?;
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:ungmute").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if !is_gmuted(ctx, user_id).await? {
        ctx.reply("`User is not gmuted.`").await?;
        return Ok(());
    }
    // Python: edit_permissions(send_messages=True) per group dialog,
    // swallowing per-chat errors, then drop the gmute record.
    // `unban_user` clears the banned rights, which lifts the send ban.
    for dialog in ctx.responder.dialogs().await? {
        if !matches!(
            dialog_kind(ctx, dialog.id).await,
            Some(ChatKind::Group) | Some(ChatKind::Supergroup)
        ) {
            continue;
        }
        let _ = ctx.responder.unban_user(dialog.id, user_id).await;
    }
    let mut muted = gmuted(ctx).await?;
    muted.retain(|u| *u != user_id);
    set_gmuted(ctx, &muted).await?;
    Ok(())
}

/// Parse a broadcast's text + button markup (Python `fns.get_msg_button`
/// on the argument or the replied message's text). Returns
/// `(message_text, button_json_or_none)`; the button JSON is the same
/// `[label | url]` markup encoding used by `greetings::split_msg_button`.
fn broadcast_parts(ctx: &Ctx) -> Option<(String, Option<String>)> {
    if !ctx.args.trim().is_empty() {
        let (txt, btn) = crate::greetings::split_msg_button(ctx.args.trim());
        return Some((txt, btn));
    }
    if let Some(reply_text) = ctx.reply_to_text.as_deref() {
        // Buttons on the replied message are not exposed on `Ctx`; the
        // text half is ported faithfully.
        let (txt, btn) = crate::greetings::split_msg_button(reply_text);
        return Some((txt, btn));
    }
    None
}

async fn gcast(ctx: &Ctx) -> Result<()> {
    // Pattern `g(admin|)cast( (.*)|$)`: group 1 is "admin" or "".
    let admin_only = ctx.captures.get(1).map(|s| s.as_str()) == Some("admin");
    let Some((msg, btn)) = broadcast_parts(ctx) else {
        ctx.reply("`Give some text to Globally Broadcast or reply a message..`")
            .await?;
        return Ok(());
    };
    let _ = ctx.reply("`Globally Broadcasting Msg...`").await?;
    if admin_only {
        // Python only broadcasts to chats where the account holds admin
        // rights / is creator; no trait op reports that, so the
        // admin-only variant stays un-wired.
        // TODO(responder-op): iter dialogs (groups, minus GBLACKLISTS and
        // NOSPAM_CHAT; admin-only checks admin_rights) + send_message each.
        // Python: plugins/globaltools.py:gcast.
        return Err(todo_op(
            "global broadcast (iter dialogs + send_message)",
            "plugins/globaltools.py:gcast",
        ));
    }
    // Text (+ URL buttons) to every group dialog, minus GBLACKLISTS.
    // (The NOSPAM_CHAT list is not ported. A broadcast built from a
    // replied message's media goes out as text only: `Ctx` does not
    // expose the replied message's media.)
    let blacklists = gblacklists(ctx).await?;
    for dialog in ctx.responder.dialogs().await? {
        if !matches!(
            dialog_kind(ctx, dialog.id).await,
            Some(ChatKind::Group) | Some(ChatKind::Supergroup)
        ) {
            continue;
        }
        if blacklists.contains(&dialog.id) {
            continue;
        }
        let _ = broadcast_send(ctx, dialog.id, &msg, &btn).await;
    }
    Ok(())
}

async fn gucast(ctx: &Ctx) -> Result<()> {
    let Some((msg, btn)) = broadcast_parts(ctx) else {
        ctx.reply("`Give some text to Globally Broadcast or reply a message..`")
            .await?;
        return Ok(());
    };
    let _ = ctx.reply("`Globally Broadcasting Msg...`").await?;
    // Text (+ URL buttons) to every private non-bot user dialog, minus
    // GBLACKLISTS. (A broadcast built from a replied message's media
    // goes out as text only: `Ctx` does not expose the replied
    // message's media.)
    let blacklists = gblacklists(ctx).await?;
    for dialog in ctx.responder.dialogs().await? {
        if !matches!(dialog_kind(ctx, dialog.id).await, Some(ChatKind::User)) {
            continue;
        }
        if blacklists.contains(&dialog.id) {
            continue;
        }
        match ctx.responder.user_full(dialog.id).await {
            Ok(user) if !user.bot => {}
            _ => continue,
        }
        let _ = broadcast_send(ctx, dialog.id, &msg, &btn).await;
    }
    Ok(())
}

async fn list_gban(ctx: &Ctx) -> Result<()> {
    let users = gbanned(ctx).await?;
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    if users.is_empty() {
        ctx.reply("`You haven't GBanned anyone!`").await?;
        return Ok(());
    }
    // Names need get_entity per id; without a client fetch the port lists
    // ids with their stored reasons (Python falls back to the raw id too).
    let mut msg = String::new();
    for (id, reason) in &users {
        msg.push_str(&format!("<strong>User</strong>: {id}\n"));
        if !reason.is_empty() {
            msg.push_str(&format!("<strong>Reason</strong>: {reason}\n\n"));
        } else {
            msg.push('\n');
        }
    }
    let text = format!(
        "<strong>List of users GBanned by {}</strong>:\n\n{msg}",
        ctx.config.owner_id
    );
    if text.len() > 4096 {
        // Python writes gbanned.txt and sends it as a file.
        let path = std::env::temp_dir().join("gbanned.txt");
        std::fs::write(&path, &text).map_err(|e| Error::Other(e.to_string()))?;
        ctx.responder
            .send_file_to(ctx.chat_id, &path, "GBanned users")
            .await?;
        return Ok(());
    }
    ctx.reply(&text).await?;
    Ok(())
}

async fn gstat(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let user_id = match target_or_todo(ctx, "plugins/globaltools.py:gstat_").await {
        Ok(id) => id,
        Err(e) if e.to_string() == "missing_target" => {
            ctx.reply("`Reply to some msg or add their id.`").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let name = mention_id(user_id);
    match is_gbanned(ctx, user_id).await? {
        Some(reason) => {
            let reason_part = if reason.is_empty() {
                ".**".to_string()
            } else {
                format!(" with reason** `{reason}`")
            };
            ctx.reply(&format!("**{name} is Globally Banned{reason_part}"))
                .await?;
        }
        None => {
            ctx.reply(&format!("**{name} is not Globally Banned.**"))
                .await?;
        }
    }
    Ok(())
}

async fn gblacklist(ctx: &Ctx) -> Result<()> {
    gblacker(ctx, "add").await
}

async fn ungblacklist(ctx: &Ctx) -> Result<()> {
    gblacker(ctx, "remove").await
}

async fn gblacker(ctx: &Ctx, action: &str) -> Result<()> {
    // `.gblacklist <chat id>` or nothing (current chat).
    let arg = ctx.text.splitn(2, ' ').nth(1).map(str::trim).unwrap_or("");
    let chat_id: i64 = if arg.is_empty() {
        ctx.chat_id
    } else {
        match arg.trim_start_matches('@').parse() {
            Ok(id) => id,
            Err(_) => {
                return Err(todo_op(
                    "resolve chat id/username",
                    "plugins/globaltools.py:gblacker",
                ));
            }
        }
    };
    let mut list = gblacklists(ctx).await?;
    match action {
        "add" => {
            if !list.contains(&chat_id) {
                list.push(chat_id);
            }
        }
        "remove" => list.retain(|c| *c != chat_id),
        _ => {}
    }
    set_gblacklists(ctx, &list).await?;
    ctx.reply(&format!("Global Broadcasts: \n{action}ed {chat_id}"))
        .await?;
    Ok(())
}
