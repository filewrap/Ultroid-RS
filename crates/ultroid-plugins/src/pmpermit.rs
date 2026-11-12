//! PM permit — port of `plugins/pmpermit.py` (954 LOC).
//!
//! Approved PM users live in the `PMPERMIT` udB list (JSON list of user
//! ids); log-exempt chats in `LOGUSERS`; warn limit in `PMWARNS`
//! (default 4); the unapproved warning text in `PM_TEXT` (default is the
//! `pmperm_1` string); the block notice in `pmperm_2`; the no-reply
//! notice in `pmperm_3`.
//!
//! The incoming-PM warn flow (`permitpm` in Python, registered on the
//! userbot's `NewMessage` stream) cannot be registered through the
//! command `Registry`; the binary wires [`handle_incoming_pm`] into its
//! incoming-message path. The log-channel approve/block/disapprove
//! callbacks and the `ip_*` inline result live in the assistant crate's
//! surface and are not wired here — the DB operations behind them are.
//!
//! Telegram operations the `Responder` surface cannot express (contacts
//! block/unblock, `ReportSpam`, fetching the blocked list, log-channel
//! sends from the assistant) return `Err(Error::Other(...))` naming the
//! missing op at the exact call site, per the `TODO(responder-op)` rule.
//! Archive moves are wired through `Responder::set_archived`
//! (best-effort, as Python wraps them in try/except pass).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{ChatType, CommandDef, Ctx, Error, Registry, Result};

use crate::util::{db_get, db_set, def, reg_add};

async fn pmwarns(ctx: &Ctx) -> i64 {
    db_get_json::<i64>(ctx.db.as_ref(), "PMWARNS")
        .await
        .ok()
        .flatten()
        .unwrap_or(4)
}

/// The approved-PM list (`PMPERMIT` udB key, JSON list of user ids).
pub async fn approved_list(ctx: &Ctx) -> Vec<i64> {
    // Python normalises a stray int/str value into a one-element list.
    if let Ok(Some(v)) = db_get_json::<serde_json::Value>(ctx.db.as_ref(), "PMPERMIT").await {
        if let Some(list) = v.as_array() {
            return list.iter().filter_map(|x| x.as_i64()).collect();
        }
        if let Some(n) = v.as_i64() {
            return vec![n];
        }
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.trim().parse::<i64>() {
                return vec![n];
            }
        }
    }
    Vec::new()
}

async fn set_approved_list(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(ctx.db.as_ref(), "PMPERMIT", &list.to_vec()).await
}

pub async fn is_approved(ctx: &Ctx, user_id: i64) -> bool {
    approved_list(ctx).await.contains(&user_id)
}

pub async fn approve_user(ctx: &Ctx, user_id: i64) -> Result<bool> {
    let mut list = approved_list(ctx).await;
    if list.contains(&user_id) {
        return Ok(false);
    }
    list.push(user_id);
    set_approved_list(ctx, &list).await?;
    Ok(true)
}

pub async fn disapprove_user(ctx: &Ctx, user_id: i64) -> Result<bool> {
    let mut list = approved_list(ctx).await;
    if !list.contains(&user_id) {
        return Ok(false);
    }
    list.retain(|u| *u != user_id);
    set_approved_list(ctx, &list).await?;
    Ok(true)
}

async fn log_users(ctx: &Ctx) -> Vec<i64> {
    db_get_json(ctx.db.as_ref(), "LOGUSERS")
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn set_log_users(ctx: &Ctx, list: &[i64]) -> Result<()> {
    db_set_json(ctx.db.as_ref(), "LOGUSERS", &list.to_vec()).await
}

fn is_setting_on(value: Option<String>) -> bool {
    matches!(value.as_deref(), Some("True") | Some("true") | Some("1"))
}

async fn unapproved_text(
    ctx: &Ctx,
    warn: i64,
    twarn: i64,
    user_id: i64,
    first_name: &str,
) -> String {
    let count = approved_list(ctx).await.len();
    let mention = format!("[{first_name}](tg://user?id={user_id})");
    let on = ctx.config.owner_id;
    let custom = db_get(ctx, "PM_TEXT").await.ok().flatten();
    match custom {
        Some(t) if !t.is_empty() => {
            // Python formats PM_TEXT with {ON},{UND},{warn},{twarn},
            // {name},{fullname},{username},{count},{mention}.
            t.replace("{ON}", &on.to_string())
                .replace("{UND}", &ctx.get_str("pmperm_1"))
                .replace("{warn}", &warn.to_string())
                .replace("{twarn}", &twarn.to_string())
                .replace("{name}", first_name)
                .replace("{fullname}", first_name)
                .replace("{username}", "")
                .replace("{count}", &count.to_string())
                .replace("{mention}", &mention)
        }
        _ => {
            let und = ctx.get_str("pmperm_1");
            format!("**PMSecurity of {on}!**\n\n{und}\n\nYou have {warn}/{twarn} warnings!")
        }
    }
}

// ── In-memory warn state (Python module globals) ─────────────────────────

struct WarnState {
    count_pm: HashMap<i64, i64>,
    last_msg: HashMap<i64, String>,
    warn_msgs: HashMap<i64, String>,
    u_warns: HashMap<i64, i64>,
    /// Last warn message id sent to each user (for deletion).
    to_delete: HashMap<i64, i64>,
    /// Log-channel message id tracking the user's pending approval.
    /// Populated when the log-channel Responder op lands (Python sets
    /// `NOT_APPROVED` where the log-channel warn note is sent).
    #[allow(dead_code)]
    not_approved: HashMap<i64, i64>,
}

fn warn_state() -> &'static Mutex<WarnState> {
    static STATE: OnceLock<Mutex<WarnState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(WarnState {
            count_pm: HashMap::new(),
            last_msg: HashMap::new(),
            warn_msgs: HashMap::new(),
            u_warns: HashMap::new(),
            to_delete: HashMap::new(),
            not_approved: HashMap::new(),
        })
    })
}

pub fn update_pm(user_id: i64, message: &str, warns_given: i64) {
    let mut s = warn_state().lock().expect("warn state lock");
    s.warn_msgs.insert(user_id, message.to_string());
    s.u_warns.insert(user_id, warns_given);
}

async fn delete_pm_warn_msgs(ctx: &Ctx, chat_id: i64) {
    let id = warn_state()
        .lock()
        .expect("warn state lock")
        .to_delete
        .remove(&chat_id);
    if let Some(msg_id) = id {
        let _ = ctx.delete(msg_id).await;
    }
}

// ── Log-channel notification (assistant side) ────────────────────────────

/// The assistant-side log-channel note. Not expressible through the
/// command `Ctx` (which replies only in the current chat).
fn log_channel_note() -> Result<()> {
    // TODO(responder-op): send/edit a log-channel message via the assistant
    // (Python: plugins/pmpermit.py approvepm/disapprovepm/blockpm/unblockpm
    // asst.edit_message / asst.send_message to LOG_CHANNEL).
    Err(Error::Other(
        "log-channel note requires a binary Responder op (Python: plugins/pmpermit.py asst.send_message/edit_message to LOG_CHANNEL)".to_string(),
    ))
}

// ── Target resolution ────────────────────────────────────────────────────

/// Resolve the target user for approve/disapprove/block/unblock.
///
/// Python resolves a replied-to sender, a parsed id/username, or the
/// current private chat. The replied sender comes from
/// `ctx.reply_to_sender_id`; a `@username` resolves via `resolve_user`.
async fn resolve_target(ctx: &Ctx) -> Result<i64> {
    if ctx.reply_to_id.is_some() {
        if let Some(sender_id) = ctx.reply_to_sender_id {
            return Ok(sender_id);
        }
        return Err(Error::Other(
            "could not determine the replied message's sender".into(),
        ));
    }
    let arg = ctx.args.trim();
    if !arg.is_empty() {
        let first = arg.split_whitespace().next().unwrap_or("");
        if let Ok(id) = first.trim_start_matches('@').parse::<i64>() {
            return Ok(id);
        }
        let user = ctx.responder.resolve_user(first).await?;
        return Ok(user.id);
    }
    if ctx.chat_type == ChatType::Private {
        return Ok(ctx.chat_id);
    }
    Err(Error::Other(ctx.get_str("pmperm_3")))
}

fn is_dev(ctx: &Ctx, user_id: i64) -> bool {
    ctx.config.devlist_ids.contains(&user_id)
}

fn mention(user_id: i64) -> String {
    format!("[{user_id}](tg://user?id={user_id})")
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_logpm(ctx: &Ctx) -> Result<()> {
    if !is_setting_on(db_get(ctx, "PMLOG").await.ok().flatten()) {
        ctx.reply("`PM logging is not enabled (PMLOG key is not set).`")
            .await?;
        return Ok(());
    }
    if ctx.chat_type != ChatType::Private {
        ctx.reply("`Use me in Private.`").await?;
        return Ok(());
    }
    let mut users = log_users(ctx).await;
    if !users.contains(&ctx.chat_id) {
        ctx.reply("`Wasn't logging msgs from here.`").await?;
        return Ok(());
    }
    users.retain(|u| *u != ctx.chat_id);
    set_log_users(ctx, &users).await?;
    ctx.reply("`Now I Will log msgs from here.`").await?;
    Ok(())
}

async fn cmd_nologpm(ctx: &Ctx) -> Result<()> {
    if !is_setting_on(db_get(ctx, "PMLOG").await.ok().flatten()) {
        ctx.reply("`PM logging is not enabled (PMLOG key is not set).`")
            .await?;
        return Ok(());
    }
    if ctx.chat_type != ChatType::Private {
        ctx.reply("`Use me in Private.`").await?;
        return Ok(());
    }
    let mut users = log_users(ctx).await;
    if users.contains(&ctx.chat_id) {
        ctx.reply("`Wasn't logging msgs from here.`").await?;
        return Ok(());
    }
    users.push(ctx.chat_id);
    set_log_users(ctx, &users).await?;
    ctx.reply("`Now I Won't log msgs from here.`").await?;
    Ok(())
}

async fn cmd_archive(ctx: &Ctx) -> Result<()> {
    let action = ctx.captures.get(1).map(|s| s.as_str()).unwrap_or("");
    match action {
        "start" => {
            db_set(ctx, "MOVE_ARCHIVE", "True").await?;
            ctx.reply("Now I will move new Unapproved DM's to archive")
                .await?;
        }
        "stop" => {
            db_set(ctx, "MOVE_ARCHIVE", "False").await?;
            ctx.reply("Now I won't move new Unapproved DM's to archive")
                .await?;
        }
        "clear" => {
            // Python: `client.edit_folder(unpack=1)` — unarchive every
            // dialog. The trait has no whole-folder op, so each archived
            // dialog is unarchived in turn; failures are counted and
            // reported instead of aborting the rest.
            match ctx.responder.dialogs().await {
                Ok(dialogs) => {
                    let mut failed = 0usize;
                    for d in dialogs.iter().filter(|d| d.archived) {
                        if ctx.responder.set_archived(d.id, false).await.is_err() {
                            failed += 1;
                        }
                    }
                    if failed == 0 {
                        ctx.reply("Unarchived all chats").await?;
                    } else {
                        ctx.reply(&format!(
                            "Unarchived all chats except {failed} that failed."
                        ))
                        .await?;
                    }
                }
                Err(e) => {
                    ctx.reply(&format!("`{e}`")).await?;
                }
            }
        }
        _ => {
            ctx.reply("Usage: `.startarchive`, `.stoparchive` or `.cleararchive`")
                .await?;
        }
    }
    Ok(())
}

async fn cmd_approve(ctx: &Ctx) -> Result<()> {
    let user_id = resolve_target(ctx).await?;
    if is_dev(ctx, user_id) {
        ctx.reply("Lol, He is my Developer\nHe is auto Approved")
            .await?;
        return Ok(());
    }
    if approve_user(ctx, user_id).await? {
        delete_pm_warn_msgs(ctx, user_id).await;
        // Python moves the chat out of the archive (`edit_folder(folder=0)`,
        // wrapped in try/except pass) — best-effort here too.
        if let Err(e) = ctx.responder.set_archived(user_id, false).await {
            tracing::debug!("approve: unarchive of {user_id} failed: {e}");
        }
        ctx.reply(&format!(
            "<b>{}</b> <code>approved to PM!</code>",
            mention(user_id)
        ))
        .await?;
        // Log-channel note (assistant side); surfaces as the honest error.
        log_channel_note()?;
    } else {
        ctx.reply("`User may already be approved.`").await?;
    }
    Ok(())
}

async fn cmd_disapprove(ctx: &Ctx) -> Result<()> {
    let user_id = resolve_target(ctx).await?;
    if is_dev(ctx, user_id) {
        ctx.reply("`Lol, He is my Developer\nHe Can't Be DisApproved.`")
            .await?;
        return Ok(());
    }
    if disapprove_user(ctx, user_id).await? {
        ctx.reply(&format!(
            "<b>{}</b> <code>Disapproved to PM!</code>",
            mention(user_id)
        ))
        .await?;
        log_channel_note()?;
    } else {
        ctx.reply(&format!(
            "<b>{}</b> <code>was never approved!</code>",
            mention(user_id)
        ))
        .await?;
    }
    Ok(())
}

async fn cmd_block(ctx: &Ctx) -> Result<()> {
    let user_id = resolve_target(ctx).await?;
    disapprove_user(ctx, user_id).await?;
    // TODO(responder-op): contacts.BlockRequest
    // (Python: plugins/pmpermit.py blockpm via ultroid_bot(BlockRequest(user))).
    Err(Error::Other(
        "blocking a user requires a binary Responder op (Python: plugins/pmpermit.py blockpm via contacts.BlockRequest)".to_string(),
    ))
}

async fn cmd_unblock(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.eq_ignore_ascii_case("all") {
        // TODO(responder-op): contacts.GetBlockedRequest + UnblockRequest loop
        // (Python: plugins/pmpermit.py unblockpm via GetBlockedRequest(0, 0)).
        return Err(Error::Other(
            "listing blocked users requires a binary Responder op (Python: plugins/pmpermit.py unblockpm via contacts.GetBlockedRequest)".to_string(),
        ));
    }
    let user_id = resolve_target(ctx).await?;
    // TODO(responder-op): contacts.UnblockRequest
    // (Python: plugins/pmpermit.py unblockpm via client(UnblockRequest(user))).
    let _ = user_id;
    Err(Error::Other(
        "unblocking a user requires a binary Responder op (Python: plugins/pmpermit.py unblockpm via contacts.UnblockRequest)".to_string(),
    ))
}

async fn cmd_listapproved(ctx: &Ctx) -> Result<()> {
    let all = approved_list(ctx).await;
    if all.is_empty() {
        ctx.reply("`You haven't approved anyone yet!`").await?;
        return Ok(());
    }
    let mut text = format!("{:<24}{}\n", "UserName", "UserID");
    for id in &all {
        text.push_str(&format!("{:<24}{}\n", "", id));
    }
    let path = std::env::temp_dir().join(format!("approved_pms_{}.txt", ctx.chat_id));
    tokio::fs::write(&path, text).await?;
    let caption = format!(
        "List of users approved by [{}](tg://user?id={})",
        ctx.config.owner_id, ctx.config.owner_id
    );
    ctx.send_file(&path, &caption).await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(())
}

// ── Incoming-PM warn flow (binary-wired) ─────────────────────────────────

/// Outcome of one incoming private message for the permit system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingPmAction {
    /// Not a permit matter (approved user, bot, self, verified, dev,
    /// logging-exempt, or PMSETTING off).
    Ignored,
    /// A warn message was sent; `warns` is the new warn count.
    Warned { warns: i64 },
    /// Warn limit reached: the user must be blocked + reported as spam.
    BlockForSpam { warns: i64 },
}

/// The `permitpm` listener, as a binary-callable function.
///
/// `sender_is_bot`, `sender_is_self`, `sender_verified` and the sender's
/// first name come from the update; the warn message is sent with
/// `ctx.reply` (the chat *is* the sender's PM). Archive moves, the
/// block/report-spam calls and the log-channel note need Responder ops
/// and surface as `Err` at their call sites.
pub async fn handle_incoming_pm(
    ctx: &Ctx,
    sender_first_name: &str,
    sender_is_bot: bool,
    sender_is_self: bool,
    sender_verified: bool,
) -> Result<IncomingPmAction> {
    if !is_setting_on(db_get(ctx, "PMSETTING").await.ok().flatten()) {
        return Ok(IncomingPmAction::Ignored);
    }
    if ctx.chat_type != ChatType::Private {
        return Ok(IncomingPmAction::Ignored);
    }
    let user_id = ctx.sender_id;
    if sender_is_bot || sender_is_self || sender_verified || is_dev(ctx, user_id) {
        return Ok(IncomingPmAction::Ignored);
    }
    if log_users(ctx).await.contains(&user_id) {
        return Ok(IncomingPmAction::Ignored);
    }
    if is_approved(ctx, user_id).await {
        return Ok(IncomingPmAction::Ignored);
    }
    if ctx.text == ctx.get_str("pmperm_1") {
        return Ok(IncomingPmAction::Ignored);
    }
    // Python archives the chat when MOVE_ARCHIVE is on
    // (`edit_folder(folder=1)`, wrapped in try/except) — best-effort here.
    if is_setting_on(db_get(ctx, "MOVE_ARCHIVE").await.ok().flatten()) {
        if let Err(e) = ctx.responder.set_archived(ctx.chat_id, true).await {
            tracing::debug!("permitpm: archiving {} failed: {e}", ctx.chat_id);
        }
    }

    let warns = pmwarns(ctx).await;
    let state = warn_state().lock().expect("warn state lock");
    let prev = state.last_msg.get(&user_id).cloned();
    let count = state.count_pm.get(&user_id).copied().unwrap_or(0);
    let wrn = count + 1;
    drop(state);

    // The warn text only changes when the incoming text differs from the
    // last one (or on first contact); duplicates re-send the same warn.
    let _ = prev;
    let text = unapproved_text(ctx, wrn, warns, user_id, sender_first_name).await;
    delete_pm_warn_msgs(ctx, user_id).await;
    let sent_id = ctx.reply(&text).await?;
    {
        let mut state = warn_state().lock().expect("warn state lock");
        state.to_delete.insert(user_id, sent_id);
        state.count_pm.insert(user_id, wrn);
        state.last_msg.insert(user_id, ctx.text.clone());
        state.warn_msgs.insert(user_id, text);
        state.u_warns.insert(user_id, wrn);
    }

    if wrn >= warns {
        delete_pm_warn_msgs(ctx, user_id).await;
        ctx.reply(&ctx.get_str("pmperm_2")).await?;
        {
            let mut state = warn_state().lock().expect("warn state lock");
            state.count_pm.remove(&user_id);
            state.last_msg.remove(&user_id);
        }
        // TODO(responder-op): block + report spam
        // (Python: plugins/pmpermit.py permitpm via BlockRequest + ReportSpamRequest).
        return Err(Error::Other(
            "blocking a spammer requires a binary Responder op (Python: plugins/pmpermit.py permitpm via contacts.BlockRequest + messages.ReportSpamRequest)".to_string(),
        ));
    }
    Ok(IncomingPmAction::Warned { warns: wrn })
}

pub fn register(registry: &mut Registry) {
    let help = "Commands Available -\n• `.a` / `.approve` — Approve someone to PM.\n• `.da` / `.disapprove` — Disapprove someone to PM.\n• `.block` — Block someone.\n• `.unblock` / `.unblock all` — Unblock someone.\n• `.logpm` / `.nologpm` — Start/stop logging messages from the user.\n• `.startarchive` / `.stoparchive` / `.cleararchive` — Archive new PMs.\n• `.listapproved` — List all approved PMs.";
    reg_add(registry, def("logpm", "logpm$", help), |ctx| {
        Box::pin(cmd_logpm(ctx))
    });
    reg_add(registry, def("nologpm", "nologpm$", help), |ctx| {
        Box::pin(cmd_nologpm(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "archive",
            pattern: "(start|stop|clear)archive$",
            help,
            fullsudo: true,
            ..def("archive", "(start|stop|clear)archive$", help)
        },
        |ctx| Box::pin(cmd_archive(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "approve",
            pattern: "(a|approve)(?: |$)",
            help,
            fullsudo: true,
            ..def("approve", "(a|approve)(?: |$)", help)
        },
        |ctx| Box::pin(cmd_approve(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "disapprove",
            pattern: "(da|disapprove)(?: |$)",
            help,
            fullsudo: true,
            ..def("disapprove", "(da|disapprove)(?: |$)", help)
        },
        |ctx| Box::pin(cmd_disapprove(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "block",
            pattern: "block( (.*)|$)",
            help,
            fullsudo: true,
            ..def("block", "block( (.*)|$)", help)
        },
        |ctx| Box::pin(cmd_block(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "unblock",
            pattern: "unblock( (.*)|$)",
            help,
            fullsudo: true,
            ..def("unblock", "unblock( (.*)|$)", help)
        },
        |ctx| Box::pin(cmd_unblock(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "listapproved",
            pattern: "listapproved$",
            help,
            owner_only: true,
            ..def("listapproved", "listapproved$", help)
        },
        |ctx| Box::pin(cmd_listapproved(ctx)),
    );
}
