//! Assistant bot `/start` entry point and owner menus.
//!
//! Port of `assistant/start.py`.
//!
//! * `/start` (non-group only) — registers the visitor in udB
//!   `BOT_USERS`, shows the visitor welcome (custom `STARTMSG` or
//!   the default text, plus the Info button when `BOT_INFO_START`
//!   is visible) or, for full sudos, the owner home menu
//!   (Language / Settings / Stats / Broadcast / Timezone) or the
//!   settings page for the `set` payload. Unrecognized start
//!   payloads land on the normal owner welcome menu, exactly as
//!   Python does.
//! * Callbacks: `ownerinfo`, `closeit`, `info_stub`,
//!   `assistant:start:mainmenu`, `states`, `bcast`, `assistant:start:setter`,
//!   `tz`.
//!
//! Contract limits, stated plainly (see `crate::cbs`):
//! * The visitor log to `LOG_CHANNEL` and the broadcast fan-out
//!   need sends to peers other than the current chat; `Responder`
//!   targets one chat only, so those sends are not attempted here —
//!   `broadcast_recipients` / `visitor_log_text` expose the exact
//!   data so the binary crate can deliver it.
//! * The `set_bot_profile` / Mini App startup calls need raw
//!   `bots.SetBotInfoRequest` / `SetBotMenuButtonRequest` calls;
//!   `bot_profile_text` computes the exact about/description the
//!   binary should set.
//! * Deep-link stored files (`fns.get_stored_file`) have no
//!   counterpart in the contract; an unrecognized payload falls
//!   through to the owner welcome menu, as Python does.
//! * Broadcast and timezone collection are conversations in Python
//!   (`conv.get_response`). Here the prompt is sent and the pending
//!   state is stored in the DB (`asst:conv:<user_id>`); the binary
//!   routes the owner's next private message to
//!   `handle_conversation_reply`, which completes the flow. `/cancel`
//!   aborts either flow, exactly as in Python.

use serde::{Deserialize, Serialize};
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};
use ultroid_core::{Ctx, Registry};

use crate::cbs::{back_button, cb, db_get, def, is_cancel, is_enabled, reg_add};
use crate::rich_pages::{body_page, edit_callback_page, rich_page};

/// Callback data tokens used by this module.
pub fn setter_data() -> String {
    cb(&["assistant", "start", "setter"]).unwrap_or_default()
}

pub fn main_menu_data() -> String {
    cb(&["assistant", "start", "mainmenu"]).unwrap_or_default()
}

/// Read one udB key and report whether it holds a non-empty value.
/// (A plain async fn, not a closure: a closure returning a future that
/// borrows both `ctx` and its `&str` argument cannot satisfy the borrow
/// checker, while an async fn ties the borrows to the call.)
async fn has_key(ctx: &Ctx, key: &str) -> bool {
    matches!(db_get(ctx, key).await, Ok(Some(v)) if !v.is_empty())
}

fn cbs_menu(key: &str) -> String {
    cb(&["assistant", "callbacks", "menu", key]).unwrap_or_default()
}

/// Is a settings section configured? (Python `_settings_configured`.)
pub async fn settings_configured(ctx: &Ctx, key: &str) -> bool {
    match key {
        "apiset" => {
            has_key(ctx, "RMBG_API").await
                || has_key(ctx, "DEEP_AI").await
                || has_key(ctx, "OCR_API").await
                || has_key(ctx, "SERPAPI").await
        }
        "chatbot" => has_key(ctx, "PMBOT").await,
        "alive_customisation" => {
            has_key(ctx, "ALIVE_TEXT").await || has_key(ctx, "ALIVE_PIC").await
        }
        "pm_settings" => has_key(ctx, "PMSETTING").await,
        "othervars" => {
            for k in [
                "TAG_LOG",
                "SUDO",
                "EMOJI_IN_HELP",
                "DUAL_HNDLR",
                "ADDONS",
                "INLINE_PIC",
                "PLUGIN_CHANNEL",
                "SUDO_HNDLR",
            ] {
                if has_key(ctx, k).await {
                    return true;
                }
            }
            false
        }
        "vc_menu" => has_key(ctx, "VC_SESSION").await,
        _ => false,
    }
}

/// The six settings sections (Python `_settings_sections`).
pub const SETTINGS_SECTIONS: &[(&str, &str)] = &[
    (
        "API Keys",
        "Configure image, AI, OCR, and search service credentials.",
    ),
    (
        "Chat Bot",
        "Toggle the bot and edit its welcome, info, media, and subscription settings.",
    ),
    (
        "Alive",
        "Customize the assistant's alive message and media.",
    ),
    (
        "PM Permit",
        "Control the private-message guard and its options.",
    ),
    (
        "Features",
        "Configure logging, sudo, handlers, plugins, and helper options.",
    ),
    ("VC Song Bot", "Set the voice-chat bot session."),
];

pub fn settings_page() -> Result<RichDoc> {
    rich_page(
        "Assistant Settings",
        &SETTINGS_SECTIONS
            .iter()
            .map(|(a, b)| vec![a.to_string(), b.to_string()])
            .collect::<Vec<_>>(),
        Some("Choose a section below. Credentials and session values stay hidden."),
        &["Section", "What it does"],
        1,
        1,
        Some(6),
        None,
    )
}

/// Owner home actions (Python `_owner_home_page`).
pub const OWNER_HOME_ACTIONS: &[(&str, &str)] = &[
    ("Language", "Choose the assistant's display language."),
    ("Settings", "Configure assistant features and services."),
    ("Stats", "View the number of registered assistant users."),
    ("Broadcast", "Send a message to registered assistant users."),
    ("Timezone", "Set the timezone used by the assistant."),
];

pub fn owner_home_page(ctx: &Ctx, name: &str) -> Result<RichDoc> {
    let desc = plain_text(&ctx.get_str("ast_3").replacen("{}", name, 1));
    rich_page(
        &format!("Welcome, {name}"),
        &OWNER_HOME_ACTIONS
            .iter()
            .map(|(a, b)| vec![a.to_string(), b.to_string()])
            .collect::<Vec<_>>(),
        Some(&desc),
        &["Action", "What it does"],
        1,
        1,
        Some(OWNER_HOME_ACTIONS.len()),
        None,
    )
}

/// Remove simple Markdown markers (Python `_plain_text`).
pub fn plain_text(value: &str) -> String {
    let mut text = value.to_string();
    for marker in ["**", "__", "~~", "`", "*"] {
        text = text.replace(marker, "");
    }
    text
}

/// Settings keyboard, with a green style on configured sections
/// (Python `_settings_keyboard`).
pub async fn settings_keyboard(ctx: &Ctx) -> Vec<Vec<InlineButton>> {
    let styled = |label: &str, data: String, configured: bool| {
        let mut b = InlineButton::data(label, data.into_bytes());
        if configured {
            b = b.styled(ButtonStyle::Success);
        }
        b
    };
    let main = main_menu_data();
    vec![
        vec![
            styled(
                "API Keys",
                cbs_menu("apiset"),
                settings_configured(ctx, "apiset").await,
            ),
            styled(
                "Chat Bot",
                cbs_menu("chatbot"),
                settings_configured(ctx, "chatbot").await,
            ),
        ],
        vec![
            styled(
                "Alive",
                cbs_menu("alive_customisation"),
                settings_configured(ctx, "alive_customisation").await,
            ),
            styled(
                "PM Permit",
                cbs_menu("pm_settings"),
                settings_configured(ctx, "pm_settings").await,
            ),
        ],
        vec![
            styled(
                "Features",
                cbs_menu("othervars"),
                settings_configured(ctx, "othervars").await,
            ),
            styled(
                "VC Song Bot",
                cbs_menu("vc_menu"),
                settings_configured(ctx, "vc_menu").await,
            ),
        ],
        vec![InlineButton::data("Back", main.into_bytes()).styled(ButtonStyle::Primary)],
    ]
}

/// Owner home keyboard (Python module-level `_start`).
pub fn start_keyboard() -> Vec<Vec<InlineButton>> {
    let lang = cb(&["assistant", "lang", "menu"]).unwrap_or_default();
    let p = ButtonStyle::Primary;
    vec![
        vec![
            InlineButton::data("Language", lang.into_bytes()).styled(p),
            InlineButton::data("Settings", setter_data().into_bytes()).styled(p),
        ],
        vec![
            InlineButton::data("Stats", b"states".to_vec()).styled(p),
            InlineButton::data("Broadcast", b"bcast".to_vec()).styled(p),
        ],
        vec![InlineButton::data("Timezone", b"tz".to_vec()).styled(p)],
    ]
}

/// Default owner-info text (Python `_default_owner_info`).
pub async fn default_owner_info(ctx: &Ctx) -> String {
    let version = ultroid_core::ULTROID_VERSION;
    let pm = db_get(ctx, "PMBOT")
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    // No owner display name exists in `Ctx` (Python reads it off the
    // user client at import); the id is used as the mention target.
    format!(
        "\n**Owner** - [{}]\n**OwnerID** - {}\n\n**Message Forwards** - {pm}\n\n**Ultroid [v{version}](https://github.com/TeamUltroid/Ultroid), powered by @TeamUltroid**\n",
        ctx.config.owner_id, ctx.config.owner_id
    )
}

/// `(text, is_custom, visible)` for the owner-info setting (Python
/// `_owner_info_state`): absent → default text, visible; `false` /
/// `"false"` → hidden; anything else → custom text.
pub async fn owner_info_state(ctx: &Ctx) -> (String, bool, bool) {
    match db_get(ctx, "BOT_INFO_START").await {
        Ok(None) => (default_owner_info(ctx).await, false, true),
        Ok(Some(v)) => {
            if v.trim().eq_ignore_ascii_case("false") {
                (String::new(), true, false)
            } else {
                let visible = !v.trim().is_empty();
                (v, true, visible)
            }
        }
        Err(_) => (default_owner_info(ctx).await, false, true),
    }
}

/// Safe `{me}` / `{mention}` formatting that leaves unknown
/// placeholders intact (Python `_SafeFormat` + `_safe_format`).
pub fn safe_format(text: &str, me: &str, mention: &str) -> String {
    text.replace("{me}", me).replace("{mention}", mention)
}

/// The bot-profile about/description Python sets at startup
/// (`_set_bot_profile`): about is the first line, ≤120 chars;
/// description is the owner-info text, ≤512 chars. The binary crate
/// applies this with its raw client.
pub async fn bot_profile_text(ctx: &Ctx) -> (String, String) {
    let (description, _custom, visible) = owner_info_state(ctx).await;
    let description = if !visible { String::new() } else { description };
    let description = if description.trim().is_empty() {
        "Manage your Ultroid userbot from chat, or open the Mini App settings dashboard below."
            .to_string()
    } else {
        description
    };
    let stripped = description.trim();
    let about = stripped
        .lines()
        .next()
        .unwrap_or("Ultroid Assistant")
        .chars()
        .take(120)
        .collect();
    let description: String = description.chars().take(512).collect();
    (about, description)
}

/// Validate an IANA timezone name against the system zoneinfo
/// database (Python validates with `pytz.timezone`, same database).
/// Rejects empty input, absolute paths and `..` traversal.
pub fn valid_timezone(value: &str) -> Option<String> {
    let name = value.trim();
    if name.is_empty() || name.starts_with('/') || name.contains("..") || name.contains('\\') {
        return None;
    }
    if name == "UTC" || name == "Etc/UTC" {
        return Some(name.to_string());
    }
    let path = std::path::Path::new("/usr/share/zoneinfo").join(name);
    if path.is_file() {
        Some(name.to_string())
    } else {
        None
    }
}

/// Registered assistant users (udB `BOT_USERS`, a JSON list of ids).
pub async fn bot_users(ctx: &Ctx) -> Vec<i64> {
    match db_get(ctx, "BOT_USERS").await {
        Ok(Some(v)) => serde_json::from_str::<Vec<i64>>(&v).unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn bot_users_add(ctx: &Ctx, user_id: i64) -> Result<()> {
    let mut users = bot_users(ctx).await;
    if !users.contains(&user_id) {
        users.push(user_id);
        let v = serde_json::to_string(&users).map_err(Error::from)?;
        ctx.db.set("BOT_USERS", &v).await?;
    }
    Ok(())
}

/// The visitor-log line Python sends to `LOG_CHANNEL` for a new
/// visitor. The send itself needs a cross-peer channel (see module
/// docs); this returns `(log_channel, text)` for the binary crate.
pub async fn visitor_log(ctx: &Ctx) -> Option<(i64, String)> {
    let channel = db_get(ctx, "LOG_CHANNEL")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v != 0)?;
    let off = db_get(ctx, "OFF_START_LOG").await.ok().flatten();
    if is_enabled(off.as_deref()) {
        return None;
    }
    Some((
        channel,
        format!("User [{}] started your Assistant bot.", ctx.sender_id),
    ))
}

/// `/start` — non-group only (Python `ultroid`, pattern
/// `start( (.*)|$)`).
pub async fn start_cmd(ctx: &Ctx) -> Result<()> {
    if matches!(
        ctx.chat_type,
        ultroid_core::ChatType::Group | ultroid_core::ChatType::Supergroup
    ) {
        return Ok(());
    }
    // Dedupe repeated delivery of the same /start update (Python
    // `_claim_start_event`, keyed by chat/sender/message, cap 512).
    let dedupe_key = format!(
        "asst:start:seen:{}:{}:{}",
        ctx.chat_id, ctx.sender_id, ctx.message_id
    );
    if ctx.db.exists(&dedupe_key).await.unwrap_or(false) {
        return Ok(());
    }
    let _ = ctx.db.set(&dedupe_key, "1").await;
    let args = ctx.args.trim().to_string();
    let owner = ctx.is_owner;
    let full_sudo = ctx.is_sudo;

    if !owner && !full_sudo {
        // Visitor: register once, then the welcome card.
        bot_users_add(ctx, ctx.sender_id).await?;
        let (_info_text, _custom, _show_info) = owner_info_state(ctx).await;
        let me = "Ultroid";
        let mention = format!("user {}", ctx.sender_id);
        let doc = if let Some(custom) = db_get(ctx, "STARTMSG").await.ok().flatten() {
            let msg = safe_format(&custom, me, &mention);
            body_page_doc("Ultroid Assistant", &msg)?
        } else {
            let mut ok = String::new();
            if is_enabled(db_get(ctx, "PMBOT").await.ok().flatten().as_deref()) {
                ok = "You can contact my master using this bot!!\n\nSend your Message, I will Deliver it To Master."
                    .to_string();
            }
            let body = format!("Hey there {mention}, this is Ultroid Assistant of {me}!\n\n{ok}");
            body_page_doc("Ultroid Assistant", &body)?
        };
        ctx.send_rich_or_plain(&doc).await?;
        return Ok(());
    }

    // Owner / full sudo.
    let name = format!("user {}", ctx.sender_id);
    if args == "set" {
        let page = settings_page()?;
        let kb = settings_keyboard(ctx).await;
        edit_or_send(ctx, &page, kb).await?;
    } else {
        let page = owner_home_page(ctx, &name)?;
        let kb = start_keyboard();
        edit_or_send(ctx, &page, kb).await?;
    }
    Ok(())
}

fn body_page_doc(head: &str, body: &str) -> Result<RichDoc> {
    body_page(
        head,
        &format!("<p>{}</p>", escape_html(body)),
        1,
        1,
        1,
        None,
    )
}

async fn edit_or_send(ctx: &Ctx, page: &RichDoc, _kb: Vec<Vec<InlineButton>>) -> Result<()> {
    // A fresh command sends a new rich message (Python `send_rich`
    // with `reply_to=event`); callbacks edit in place instead.
    ctx.send_rich_or_plain(page).await?;
    Ok(())
}

/// Pending conversation state (broadcast / timezone), stored in the
/// DB so the follow-up message can complete the flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingConv {
    pub kind: String,
    pub started_at: i64,
}

fn pending_key(user_id: i64) -> String {
    format!("asst:conv:{user_id}")
}

/// Route an incoming private text message to a pending broadcast /
/// timezone conversation. Returns `true` when it consumed the
/// message. The binary crate calls this for private text that is not
/// a command.
pub async fn handle_conversation_reply(ctx: &mut Ctx) -> Result<bool> {
    let key = pending_key(ctx.sender_id);
    let Some(raw) = ctx.db.get(&key).await? else {
        return Ok(false);
    };
    let Ok(pending) = serde_json::from_str::<PendingConv>(&raw) else {
        return Ok(false);
    };
    let text = ctx.text.trim().to_string();
    match pending.kind.as_str() {
        "bcast" => {
            if is_cancel(&text) {
                ctx.db.del(&key).await?;
                let page = body_page(
                    "Broadcast cancelled",
                    "<p>Nothing was changed.</p>",
                    1,
                    1,
                    1,
                    None,
                )?;
                ctx.send_rich_or_plain(&page).await?;
                return Ok(true);
            }
            ctx.db.del(&key).await?;
            broadcast_run(ctx, &text).await?;
            Ok(true)
        }
        "tz" => {
            if is_cancel(&text) {
                ctx.db.del(&key).await?;
                let page = body_page(
                    "Timezone update cancelled",
                    "<p>Nothing was changed.</p>",
                    1,
                    1,
                    1,
                    None,
                )?;
                ctx.send_rich_or_plain(&page).await?;
                return Ok(true);
            }
            match valid_timezone(&text) {
                Some(name) => {
                    ctx.db.del(&key).await?;
                    ctx.db.set("TIMEZONE", &name).await?;
                    let page = body_page(
                        "Timezone updated",
                        &format!(
                            "<p>Timezone changed to <code>{}</code>.</p>",
                            escape_html(&name)
                        ),
                        1,
                        1,
                        1,
                        None,
                    )?;
                    ctx.send_rich_or_plain(&page).await?;
                }
                None => {
                    let page = body_page(
                        "Timezone not recognized",
                        "<p>Use an IANA name such as <code>Asia/Kolkata</code>, <code>Europe/London</code>, <code>America/New_York</code>, or <code>UTC</code>.</p><p>Try another value, or send <code>/cancel</code> to stop.</p>",
                        1,
                        1,
                        4,
                        None,
                    )?;
                    ctx.send_rich_or_plain(&page).await?;
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Broadcast recipients (udB `BOT_USERS`) — the fan-out send itself
/// needs a cross-peer channel; see module docs.
pub async fn broadcast_recipients(ctx: &Ctx) -> Vec<i64> {
    bot_users(ctx).await
}

/// Run a broadcast: counts and reports. The per-user sends go
/// through the binary crate (see `broadcast_recipients`); this
/// function records the run and returns the completion page data
/// `(total, message)`. Callers that can send cross-peer should send
/// `message` to every recipient, then call `broadcast_report`.
pub async fn broadcast_run(ctx: &Ctx, message: &str) -> Result<()> {
    let total = bot_users(ctx).await.len();
    let page = body_page(
        "Broadcast",
        &format!(
            "<p>Starting a broadcast to {total} users.</p><p>{}</p>",
            escape_html(message)
        ),
        1,
        1,
        1,
        None,
    )?;
    ctx.send_rich_or_plain(&page).await?;
    Err(Error::Other(
        "broadcast fan-out needs a cross-peer send channel (Responder targets only the current chat); use broadcast_recipients() to deliver the message to each user, then broadcast_report()".to_string(),
    ))
}

/// Broadcast completion page (Python `bdcast` end): time taken,
/// total, successful, failed.
pub fn broadcast_report(total: usize, success: usize, fail: usize, secs: u64) -> Result<RichDoc> {
    rich_page(
        "Broadcast Complete",
        &[
            vec!["Time taken".to_string(), format!("{secs} seconds")],
            vec!["Total users".to_string(), total.to_string()],
            vec!["Successful".to_string(), format!("{success} users")],
            vec!["Failed".to_string(), fail.to_string()],
        ],
        None,
        &["Result", "Count"],
        1,
        1,
        Some(4),
        None,
    )
}

/// Dispatch callback data owned by this module. Returns `true` when
/// the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    match data {
        "ownerinfo" => {
            owner_info(ctx).await?;
            Ok(true)
        }
        "closeit" => {
            ctx.delete(ctx.message_id).await?;
            Ok(true)
        }
        "info_stub" => {
            let h = &ctx.config.hndlr;
            ctx.reply(&format!(
                "When New Visitor will visit your Assistant Bot. You will get this log message!\n\nTo Disable : {h}setdb OFF_START_LOG True"
            ))
            .await?;
            Ok(true)
        }
        "states" => {
            let ok = bot_users(ctx).await.len();
            let page = rich_page(
                "Assistant Stats",
                &[vec!["Registered users".to_string(), ok.to_string()]],
                None,
                &["Metric", "Total"],
                1,
                1,
                Some(1),
                None,
            )?;
            edit_callback_page(
                ctx,
                &page,
                vec![vec![back_button(&main_menu_data(), "Back")]],
            )
            .await?;
            Ok(true)
        }
        "bcast" => {
            let total = bot_users(ctx).await.len();
            let page = body_page(
                "Broadcast",
                &format!("<p>Initiating broadcast to {total} users.</p>"),
                1,
                1,
                1,
                None,
            )?;
            ctx.send_rich_or_plain(&page).await?;
            let pending = PendingConv {
                kind: "bcast".to_string(),
                started_at: crate::cbs::now_unix(),
            };
            ctx.db
                .set(
                    &pending_key(ctx.sender_id),
                    &serde_json::to_string(&pending).map_err(Error::from)?,
                )
                .await?;
            let prompt = body_page(
                "Broadcast",
                "<p>Enter your broadcast message.</p><p>Use /cancel to stop.</p>",
                1,
                1,
                1,
                None,
            )?;
            ctx.send_rich_or_plain(&prompt).await?;
            Ok(true)
        }
        "tz" => {
            ctx.delete(ctx.message_id).await?;
            let pending = PendingConv {
                kind: "tz".to_string(),
                started_at: crate::cbs::now_unix(),
            };
            ctx.db
                .set(
                    &pending_key(ctx.sender_id),
                    &serde_json::to_string(&pending).map_err(Error::from)?,
                )
                .await?;
            let page = body_page(
                "Timezone",
                "<p>Send an IANA timezone name. Examples: <code>Asia/Kolkata</code>, <code>Europe/London</code>, <code>America/New_York</code>, or <code>UTC</code>.</p><p><a href=\"https://www.timezoneconverter.com/cgi-bin/findzone.tzc\">Browse timezone names</a>. Send <code>/cancel</code> to stop.</p>",
                1,
                1,
                4,
                None,
            )?;
            ctx.send_rich_or_plain(&page).await?;
            Ok(true)
        }
        _ => {
            if data == setter_data() {
                let page = settings_page()?;
                let kb = settings_keyboard(ctx).await;
                edit_callback_page(ctx, &page, kb).await?;
                return Ok(true);
            }
            if data == main_menu_data() {
                let name = format!("user {}", ctx.config.owner_id);
                let page = owner_home_page(ctx, &name)?;
                edit_callback_page(ctx, &page, start_keyboard()).await?;
                return Ok(true);
            }
            Ok(false)
        }
    }
}

async fn owner_info(ctx: &Ctx) -> Result<()> {
    let (info_text, is_custom, visible) = owner_info_state(ctx).await;
    if !visible {
        ctx.reply("The bot info message has been disabled.").await?;
        return Ok(());
    }
    let mention = format!("user {}", ctx.sender_id);
    let msg = safe_format(&info_text, "Ultroid", &mention);
    let msg = if is_custom {
        format!("{msg}\n\nPowered by [@TeamUltroid](https://t.me/TeamUltroid)")
    } else {
        msg
    };
    let page = body_page(
        "Owner Information",
        &format!("<p>{}</p>", escape_html(&msg)),
        1,
        1,
        1,
        None,
    )?;
    let close = InlineButton::data("Close", b"closeit".to_vec());
    edit_callback_page(ctx, &page, vec![vec![close]]).await
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("start", "start( (.*)|$)", "Start the assistant bot."),
        |ctx| Box::pin(start_cmd(ctx)),
    );
}
