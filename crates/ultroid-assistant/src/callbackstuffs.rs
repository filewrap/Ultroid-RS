//! Assistant settings callbacks (menus, toggles, conversations).
//!
//! Port of `assistant/callbackstuffs.py`.
//!
//! Namespaces (see `crate::cbs`):
//! * `assistant:callbacks:menu:<key>` — one handler renders every
//!   settings submenu from the `_buttons` table.
//! * `assistant:callbacks:conv:<key>` — conversation-style value
//!   entry from the `_convo` table (prompt → owner's reply → set).
//! * `assistant:callbacks:changes:<...>` — changelog / update.
//! * `assistant:callbacks:pm_warns:<n>` — PM warning count.
//! * `assistant:callbacks:paste:<path>[|<index>]` — paste a plugin
//!   source to spaceb.in.
//! * `assistant:inline:send_plugin:<key>:<name>[:<index>]` — send a
//!   plugin source file to the owner in private.
//! * `assistant:callbacks:<action>` — single-shot toggles and value
//!   prompts (`dual_mode_on`, `pm_permit_on`, `handler`, …).
//! * `assistant:fdroid:<pkg>` — download an F-Droid APK.
//! * `noop` — acknowledge only.
//!
//! What is real here: the whole menu tree (heads, bodies, feet,
//! button styles incl. green-on-configured), every toggle write,
//! every conversation flow (prompt → validate → `setit` →
//! confirmation), PM warns, the F-Droid scrape + download, and the
//! spaceb.in paste call (real HTTP).
//!
//! Contract limits (the binary crate owns the missing half):
//! * Conversations: Python blocks on `conv.get_response()`. Here
//!   the prompt is sent and the pending action is stored in the DB
//!   (`asst:cbconv:<user_id>`); the binary routes the owner's next
//!   private message to `handle_conversation_reply`. `/cancel`
//!   aborts, exactly as in Python.
//! * Update / changelog (`updatenow`, `changes`): the Python
//!   updater engine is a git-based updater with no counterpart in
//!   this contract — those actions return an error naming it.
//! * Google Drive (`authorise`, `folderid`, `gdrive`): no Drive
//!   client exists in this contract — same treatment.
//! * `send_plugin` and the `bot_fsub` entity check need sends /
//!   entity resolution for other peers — the file lookup itself is
//!   real; the cross-peer send is reported, not faked.
//! * Media value flows (alive/PM/inline media, bot welcome media):
//!   a text URL is stored verbatim; a sticker/file upload or a
//!   telegra.ph re-upload needs Telegram media access / a multipart
//!   HTTP feature this build does not have, so those replies end
//!   with Python's "Terminated" message instead of a fake URL.
//! * `chatbot_on` used to load/unload the pmbot plugin handlers
//!   in-process; here it flips the `PMBOT` key (the loader state
//!   lives in the binary's `Registry`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use ultroid_core::Ctx;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};

use crate::cbs::{
    back_button, cb, db_get, fmtmsg, is_cancel, is_enabled, mask_secret, toggle_display,
    val_or_not_set,
};
use crate::init::setit;
use crate::rich_pages::{edit_callback_page, rich_page};

// ── token helpers ─────────────────────────────────────────────────────────

pub fn tok(action: &str) -> String {
    cb(&["assistant", "callbacks", action]).unwrap_or_default()
}

pub fn menu_tok(key: &str) -> String {
    cb(&["assistant", "callbacks", "menu", key]).unwrap_or_default()
}

pub fn conv_tok(key: &str) -> String {
    cb(&["assistant", "callbacks", "conv", key]).unwrap_or_default()
}

pub const MENU_PREFIX: [&str; 3] = ["assistant", "callbacks", "menu"];
pub const CONV_PREFIX: [&str; 3] = ["assistant", "callbacks", "conv"];
pub const CHANGES_PREFIX: [&str; 3] = ["assistant", "callbacks", "changes"];
pub const WARNS_PREFIX: [&str; 3] = ["assistant", "callbacks", "pm_warns"];
pub const PASTE_PREFIX: [&str; 3] = ["assistant", "callbacks", "paste"];
pub const FDROID_PREFIX: [&str; 2] = ["assistant", "fdroid"];

// ── menu table ────────────────────────────────────────────────────────────

/// One submenu: `(head, body, foot_key, buttons)` where buttons are
/// `(label, callback_data, style)` rows. Feet are computed live from
/// the DB (see `menu_foot`).
pub fn menu_buttons(key: &str) -> Vec<Vec<(String, String, Option<ButtonStyle>)>> {
    use ButtonStyle as S;
    let t = |a: &str| tok(a);
    let m = |k: &str| menu_tok(k);
    let c = |k: &str| conv_tok(k);
    let setter = cb(&["assistant", "start", "setter"]).unwrap_or_default();
    let row1 = |a: (String, String, Option<S>)| vec![a];
    let _ = row1;
    match key {
        "othervars" => vec![
            vec![
                ("Tag Logger".into(), t("taglogger"), None),
                ("SuperFban".into(), m("superfban"), None),
            ],
            vec![
                ("Sudo Mode".into(), t("sudo"), None),
                ("Handler".into(), t("handler"), None),
            ],
            vec![
                ("Extra Plugins".into(), t("plugins"), None),
                ("Addons".into(), t("addons"), None),
            ],
            vec![
                ("Emoji in Help".into(), t("help_emoji"), None),
                ("Set gDrive".into(), t("gdrive"), None),
            ],
            vec![
                ("Inline Pic".into(), t("inline_pic"), None),
                ("Sudo HNDLR".into(), t("sudo_handler"), None),
            ],
            vec![("Dual Mode".into(), m("dual_mode"), None)],
            vec![("Back".into(), setter, None)],
        ],
        "superfban" => vec![
            vec![("FBan Group".into(), t("fbangroup"), None)],
            vec![("Exclude Feds".into(), c("excludefeds"), None)],
            vec![("Back".into(), m("othervars"), None)],
        ],
        "apauto" => vec![
            vec![("Auto Approve ON".into(), t("auto_ap_on"), Some(S::Success))],
            vec![("Auto Approve OFF".into(), t("auto_ap_off"), Some(S::Danger))],
            vec![("Back".into(), m("pm_customisation"), None)],
        ],
        "alive_customisation" => vec![
            vec![("Alive Text".into(), c("alive_text"), None)],
            vec![("Alive media".into(), t("alive_media"), None)],
            vec![(
                "Delete Alive Media".into(),
                t("del_alive_media"),
                Some(S::Danger),
            )],
            vec![("Back".into(), setter, None)],
        ],
        "pm_customisation" => vec![
            vec![
                ("PM Text".into(), t("pm_txt"), None),
                ("PM Media".into(), t("pm_media"), None),
            ],
            vec![
                ("Auto Approve".into(), m("apauto"), Some(S::Success)),
                ("PM Logger".into(), t("pm_logger"), None),
            ],
            vec![
                ("Set Warns".into(), t("set_pm_warns"), Some(S::Success)),
                ("Delete PM Media".into(), t("del_pm_media"), Some(S::Danger)),
            ],
            vec![("PM Permit Type".into(), m("pm_type"), None)],
            vec![("Back".into(), m("pm_settings"), None)],
        ],
        "pm_type" => vec![
            vec![("Inline".into(), t("pm_inline"), Some(S::Success))],
            vec![("Normal".into(), t("pm_normal"), None)],
            vec![("Back".into(), m("pm_customisation"), None)],
        ],
        "pm_settings" => vec![
            vec![(
                "Turn PM Permit On".into(),
                t("pm_permit_on"),
                Some(S::Success),
            )],
            vec![(
                "Turn PM Permit Off".into(),
                t("pm_permit_off"),
                Some(S::Danger),
            )],
            vec![("Customize PM Permit".into(), m("pm_customisation"), None)],
            vec![("Back".into(), setter, None)],
        ],
        "chatbot" => vec![
            vec![
                ("Chat Bot On".into(), t("chatbot_on"), Some(S::Success)),
                ("Chat Bot Off".into(), t("chatbot_off"), Some(S::Danger)),
            ],
            vec![
                ("Bot Welcome".into(), t("bot_welcome"), None),
                ("Bot Welcome Media".into(), t("bot_welcome_media"), None),
            ],
            vec![("Bot Info Text".into(), t("bot_info_text"), None)],
            vec![("Force Subscribe".into(), t("bot_fsub"), None)],
            vec![("Back".into(), setter, None)],
        ],
        "vc_menu" => vec![
            vec![("VC Session".into(), c("vc_session"), None)],
            vec![("Back".into(), setter, None)],
        ],
        "dual_mode" => vec![
            vec![
                ("Dual Mode On".into(), t("dual_mode_on"), Some(S::Success)),
                ("Dual Mode Off".into(), t("dual_mode_off"), Some(S::Danger)),
            ],
            vec![("Dual Mode Handler".into(), t("dual_mode_handler"), None)],
            vec![("Back".into(), m("othervars"), None)],
        ],
        "apiset" => vec![
            vec![("Remove.bg API".into(), c("rmbg_key_set"), None)],
            vec![("DEEP API".into(), c("deep_key_set"), None)],
            vec![("OCR API".into(), c("ocr_key_set"), None)],
            vec![("SERP API".into(), c("serp_key_set"), None)],
            vec![("Back".into(), setter, None)],
        ],
        _ => vec![],
    }
}

/// `(head, body)` for a submenu.
pub fn menu_head_body(ctx: &Ctx, key: &str) -> (String, String) {
    let h = ctx.config.hndlr.clone();
    match key {
        "othervars" => (
            "Other Variables".into(),
            "Configure the remaining variables for your Ultroid assistant.".into(),
        ),
        "superfban" => (
            "SuperFban Settings".into(),
            "SuperFban bans spammers across federations for your channel.".into(),
        ),
        "apauto" => (
            "Auto Approve".into(),
            "Automatically approve incoming private messages.".into(),
        ),
        "alive_customisation" => (
            "Alive Customisation".into(),
            format!("Customise your {h}alive. Choose from the options below."),
        ),
        "pm_customisation" => (
            "PM Permit Settings".into(),
            "Customise your PMPERMIT settings below.".into(),
        ),
        "pm_type" => (
            "PM Permit Type".into(),
            "Choose how the PM Permit guard handles incoming private messages.".into(),
        ),
        "pm_settings" => (
            "PM Permit".into(),
            "Control the PM Permit guard for incoming private messages.".into(),
        ),
        "chatbot" => (
            "Chat Bot".into(),
            "Chat with people through your assistant bot.".into(),
        ),
        "vc_menu" => (
            "VC Song Bot".into(),
            "Play songs in group voice chat.".into(),
        ),
        "dual_mode" => (
            "Dual Mode".into(),
            "Configure dual-client routing and its handler.".into(),
        ),
        "apiset" => ("API Keys".into(), ctx.get_str("ast_1")),
        _ => ("Settings".into(), String::new()),
    }
}

/// Read one udB key and render it as a value-or-`Not Set` foot line.
/// (A plain async fn, not a closure: a closure returning a future that
/// borrows both `ctx` and its `&str` argument cannot satisfy the borrow
/// checker, while an async fn ties the borrows to the call.)
async fn db_val(ctx: &Ctx, key: &str) -> String {
    val_or_not_set(db_get(ctx, key).await.ok().flatten().as_deref())
}

/// Live foot line for a submenu (Python `foot` lambdas).
pub async fn menu_foot(ctx: &Ctx, key: &str) -> Option<String> {
    match key {
        "superfban" => Some(format!(
            "Currently: FBan group {}",
            db_val(ctx, "FBAN_GROUP_ID").await
        )),
        "apauto" => Some(format!(
            "Currently: {}",
            toggle_display(db_get(ctx, "AUTOAPPROVE").await.ok().flatten().as_deref())
        )),
        "alive_customisation" => Some(format!(
            "Currently: alive text {}",
            db_val(ctx, "ALIVE_TEXT").await
        )),
        "pm_type" => {
            let inline = matches!(
                db_get(ctx, "INLINE_PM").await.ok().flatten().as_deref(),
                Some("True") | Some("true")
            );
            Some(format!(
                "Currently: {}",
                if inline { "Inline" } else { "Normal" }
            ))
        }
        "pm_settings" => Some(format!(
            "Currently: {}",
            toggle_display(db_get(ctx, "PMSETTING").await.ok().flatten().as_deref())
        )),
        "chatbot" => Some(format!(
            "Currently: {}",
            toggle_display(db_get(ctx, "PMBOT").await.ok().flatten().as_deref())
        )),
        "vc_menu" => Some(format!(
            "Currently: session {}",
            mask_secret(db_get(ctx, "VC_SESSION").await.ok().flatten().as_deref())
        )),
        "dual_mode" => Some(format!(
            "Currently: {}",
            toggle_display(db_get(ctx, "DUAL_MODE").await.ok().flatten().as_deref())
        )),
        "apiset" => {
            let mut n = 0;
            for k in ["RMBG_API", "DEEP_AI", "OCR_API", "SERPAPI"] {
                if db_get(ctx, k).await.ok().flatten().is_some() {
                    n += 1;
                }
            }
            Some(format!("Keys set: {n}/4"))
        }
        _ => None,
    }
}

/// Button purposes for the menu table (Python
/// `_MENU_BUTTON_DESCRIPTIONS`).
pub fn button_purpose(label: &str) -> &'static str {
    match label.to_ascii_lowercase().as_str() {
        "remove.bg api" => "Set or replace the API key; its value stays hidden.",
        "deep api" => "Set or replace the DeepAI key; its value stays hidden.",
        "ocr api" => "Set or replace the OCR key; its value stays hidden.",
        "serp api" => "Set or replace the search key; its value stays hidden.",
        "chat bot on" => "Enable the private-message chat bot.",
        "chat bot off" => "Disable the private-message chat bot.",
        "bot welcome" => "Edit the welcome message shown by the bot.",
        "bot welcome media" => "Set the media shown with the welcome message.",
        "bot info text" => "Edit the information shown from the Info button.",
        "force subscribe" => "Set the channel or group membership requirement.",
        "alive text" => "Edit the assistant's alive message.",
        "alive media" => "Set media for the alive message.",
        "delete alive media" => "Remove the saved alive media.",
        "turn pm permit on" => "Enable the incoming private-message guard.",
        "turn pm permit off" => "Disable the incoming private-message guard.",
        "customize pm permit" => "Open text, media, logging, and approval options.",
        "tag logger" => "Choose the chat used for tag logs.",
        "superfban" => "Configure federated-ban group settings.",
        "sudo mode" => "Manage sudo access.",
        "handler" => "Change the assistant command prefix.",
        "extra plugins" => "Choose the source for additional plugins.",
        "addons" => "Enable or disable add-on plugins.",
        "emoji in help" => "Configure emoji labels in the help menu.",
        "set gdrive" => "Configure Google Drive access.",
        "inline pic" => "Set or clear the inline result picture.",
        "sudo hndlr" => "Change the sudo command prefix.",
        "dual mode" => "Configure dual-client routing and its handler.",
        "vc session" => "Set the voice-chat bot session; its value stays hidden.",
        "fban group" => "Set the group used by federated-ban actions.",
        "exclude feds" => "Set the federations excluded from these actions.",
        "auto approve" => "Choose whether outgoing messages are auto-approved.",
        "pm logger" => "Enable or configure private-message logging.",
        "set warns" => "Set the private-message warning limit.",
        "delete pm media" => "Remove the saved PM Permit media.",
        "pm permit type" => "Choose normal or inline PM Permit handling.",
        "inline" => "Use inline PM Permit responses.",
        "normal" => "Use normal PM Permit responses.",
        "dual mode on" => "Enable dual-client mode.",
        "dual mode off" => "Disable dual-client mode.",
        "dual mode handler" => "Set the dual-client command handler.",
        _ => "Open or update this setting.",
    }
}

/// Style a menu button like Python `_menu_button_style`: Back is
/// primary, destructive is danger, toggles are green when their
/// desired state is active, value buttons are green when a value is
/// saved, group buttons green when any group key is saved.
pub async fn menu_button_style(ctx: &Ctx, data: &str, label: &str) -> Option<ButtonStyle> {
    let action = data.rsplit(':').next().unwrap_or("").to_ascii_lowercase();
    if crate::cbs::clean_nav_label(label).eq_ignore_ascii_case("back") {
        return Some(ButtonStyle::Primary);
    }
    if matches!(
        action.as_str(),
        "deltag" | "del_alive_media" | "del_pm_media"
    ) {
        return Some(ButtonStyle::Danger);
    }
    let toggles: &[(&str, &str, bool)] = &[
        ("edon", "ADDONS", true),
        ("edof", "ADDONS", false),
        ("onsudo", "SUDO", true),
        ("ofsudo", "SUDO", false),
        ("auto_ap_on", "AUTOAPPROVE", true),
        ("auto_ap_off", "AUTOAPPROVE", false),
        ("pm_permit_on", "PMSETTING", true),
        ("pm_permit_off", "PMSETTING", false),
        ("chatbot_on", "PMBOT", true),
        ("chatbot_off", "PMBOT", false),
        ("pm_inline", "INLINE_PM", true),
        ("pm_normal", "INLINE_PM", false),
        ("dual_mode_on", "DUAL_MODE", true),
        ("dual_mode_off", "DUAL_MODE", false),
        ("pm_logger_on", "PMLOG", true),
        ("pm_logger_off", "PMLOG", false),
    ];
    for (name, key, desired) in toggles {
        if action == *name {
            let v = db_get(ctx, key).await.ok().flatten();
            if is_enabled(v.as_deref()) == *desired && v.is_some() {
                return Some(ButtonStyle::Success);
            }
            return None;
        }
    }
    let value_keys: &[(&str, &str)] = &[
        ("taglogger", "TAG_LOG"),
        ("help_emoji", "EMOJI_IN_HELP"),
        ("plugins", "PLUGIN_CHANNEL"),
        ("handler", "HNDLR"),
        ("sudo_handler", "SUDO_HNDLR"),
        ("addons", "ADDONS"),
        ("sudo", "SUDO"),
        ("fbangroup", "FBAN_GROUP_ID"),
        ("excludefeds", "EXCLUDE_FED"),
        ("alive_text", "ALIVE_TEXT"),
        ("alive_media", "ALIVE_PIC"),
        ("pm_txt", "PM_TEXT"),
        ("set_pm_warns", "PMWARNS"),
        ("pm_media", "PMPIC"),
        ("pm_logger", "PMLOG"),
        ("pm_logger_group", "PMLOGGROUP"),
        ("bot_welcome", "STARTMSG"),
        ("bot_welcome_media", "STARTMEDIA"),
        ("bot_info_text", "BOT_INFO_START"),
        ("bot_fsub", "PMBOT_FSUB"),
        ("chatbot", "PMBOT"),
        ("vc_session", "VC_SESSION"),
        ("dual_mode", "DUAL_MODE"),
        ("dual_mode_handler", "DUAL_HNDLR"),
        ("rmbg_key_set", "RMBG_API"),
        ("deep_key_set", "DEEP_AI"),
        ("ocr_key_set", "OCR_API"),
        ("serp_key_set", "SERPAPI"),
    ];
    for (name, key) in value_keys {
        if action == *name {
            let v = db_get(ctx, key).await.ok().flatten();
            if v.as_deref().is_some_and(|s| !s.is_empty()) {
                return Some(ButtonStyle::Success);
            }
            return None;
        }
    }
    None
}

/// Rebuild a menu keyboard with live styles (Python
/// `_refresh_menu_buttons`).
pub async fn refresh_menu_keyboard(ctx: &Ctx, key: &str) -> Vec<Vec<InlineButton>> {
    let mut rows = Vec::new();
    for row in menu_buttons(key) {
        let mut out = Vec::new();
        for (label, data, style) in row {
            let style = match style {
                Some(s) => Some(s),
                None => menu_button_style(ctx, &data, &label).await,
            };
            let mut b = InlineButton::data(crate::cbs::clean_nav_label(&label), data.into_bytes());
            if let Some(s) = style {
                b = b.styled(s);
            }
            out.push(b);
        }
        rows.push(out);
    }
    rows
}

/// Render one submenu as a rich page (Python `_edit_to`).
pub async fn render_menu(ctx: &Ctx, key: &str) -> Result<()> {
    let (head, body) = menu_head_body(ctx, key);
    let foot = menu_foot(ctx, key).await;
    let mut description = crate::start::plain_text(&body);
    if let Some(f) = foot {
        description = format!("{description}\n{f}");
    }
    let rows: Vec<Vec<String>> = menu_buttons(key)
        .iter()
        .flatten()
        .filter(|(label, _, _)| crate::cbs::clean_nav_label(label).to_ascii_lowercase() != "back")
        .map(|(label, _, _)| {
            vec![
                crate::cbs::clean_nav_label(label),
                button_purpose(label).to_string(),
            ]
        })
        .collect();
    let total = rows.len();
    let page = rich_page(
        &crate::cbs::clean_nav_label(&head),
        &rows,
        Some(&description),
        &["Button", "What it does"],
        1,
        1,
        Some(total),
        None,
    )?;
    let kb = refresh_menu_keyboard(ctx, key).await;
    edit_callback_page(ctx, &page, kb).await
}

use crate::cbs::{cb_tail, now_unix};

// ── conversation table ────────────────────────────────────────────────────

/// One `_convo` entry: `(db var, display name, prompt, back token)`.
pub fn convo_entry(key: &str) -> Option<(String, String, String, String)> {
    let entry = match key {
        "rmbg_key_set" => (
            "RMBG_API",
            "Remove.bg API Key",
            "Get your Remove.bg API key and send it here. Use /cancel to stop.",
            menu_tok("apiset"),
        ),
        "deep_key_set" => (
            "DEEP_AI",
            "Deep AI Api Key",
            "Get Your Deep Api from deepai.org and send here.",
            menu_tok("apiset"),
        ),
        "ocr_key_set" => (
            "OCR_API",
            "Ocr Api Key",
            "Get Your OCR api from ocr.space and send that Here.",
            menu_tok("apiset"),
        ),
        "serp_key_set" => (
            "SERPAPI",
            "Serp Api Key",
            "Get Your SERP api from https://serpapi.com/users/sign_in and send that Here.",
            menu_tok("apiset"),
        ),
        "pm_logger_group" => (
            "PMLOGGROUP",
            "Pm Log Group",
            "Send chat id of chat which you want to save as Pm log Group.",
            tok("pm_logger"),
        ),
        "vc_session" => (
            "VC_SESSION",
            "Vc Session",
            "**Vc session**\nEnter the New session u generated for vc bot.\n\nUse /cancel to terminate the operation.",
            menu_tok("vc_menu"),
        ),
        "set_pm_logger" => (
            "TAG_LOG",
            "Tag Log Group",
            "Make a group, add your assistant and make it admin.\nGet the id of that group and send it here for tag logs.\n\nUse /cancel to cancel.",
            tok("taglogger"),
        ),
        "alive_text" => (
            "ALIVE_TEXT",
            "Alive Text",
            "**Alive Text**\nEnter the new alive text.\n\nUse /cancel to terminate the operation.",
            menu_tok("alive_customisation"),
        ),
        "excludefeds" => (
            "EXCLUDE_FED",
            "Excluded Fed",
            "Send the Fed IDs you want to exclude in the ban. Split by a space.\negid1 id2 id3\nSet is as None if you dont want any.\nUse /cancel to go back.",
            menu_tok("superfban"),
        ),
        _ => return None,
    };
    Some((
        entry.0.to_string(),
        entry.1.to_string(),
        entry.2.to_string(),
        entry.3,
    ))
}

/// Pending settings conversation, stored in the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCbConv {
    /// `"convo:<key>"` (table entry) or `"action:<name>"` (prompt action).
    pub flow: String,
    pub started_at: i64,
}

fn pending_key(user_id: i64) -> String {
    format!("asst:cbconv:{user_id}")
}

async fn store_pending(ctx: &Ctx, flow: &str) -> Result<()> {
    let p = PendingCbConv {
        flow: flow.to_string(),
        started_at: now_unix(),
    };
    ctx.db
        .set(
            &pending_key(ctx.sender_id),
            &serde_json::to_string(&p).map_err(Error::from)?,
        )
        .await
}

/// Edit the callback message with a `fmtmsg` head/msgs/foot page
/// and a single Back button (the common toggle-confirm shape).
async fn edit_simple(ctx: &Ctx, head: &str, msgs: &str, foot: &str, back_data: &str) -> Result<()> {
    let text = fmtmsg(head, &[], msgs, foot);
    let doc = RichDoc::html(format!("<p>{}</p>", escape_html(&text)))?;
    let back = back_button(back_data, "Back");
    edit_callback_page(ctx, &doc, vec![vec![back]]).await
}

/// Reply with a `fmtmsg` page and a Back button (conversation flows
/// use fresh messages, not edits).
async fn reply_simple(
    ctx: &Ctx,
    head: &str,
    msgs: &str,
    foot: &str,
    back_data: &str,
) -> Result<()> {
    let text = fmtmsg(head, &[], msgs, foot);
    let doc = RichDoc::html(format!("<p>{}</p>", escape_html(&text)))?;
    let _back = back_button(back_data, "Back");
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

/// Parse a reply the way Python does (`ast.literal_eval` attempt):
/// `True`/`False` → bool strings, ints/floats kept as text, `None`
/// → empty; anything else stays a raw string.
pub fn parse_literal(value: &str) -> String {
    let v = value.trim();
    match v {
        "True" | "False" | "None" => v.to_string(),
        _ => {
            if v.parse::<i64>().is_ok() || v.parse::<f64>().is_ok() {
                v.to_string()
            } else {
                value.to_string()
            }
        }
    }
}

/// Route an incoming private message to a pending settings
/// conversation. Returns `true` when it consumed the message.
/// The binary crate calls this for private text that is not a
/// command (see the module docs).
pub async fn handle_conversation_reply(ctx: &mut Ctx) -> Result<bool> {
    let key = pending_key(ctx.sender_id);
    let Some(raw) = ctx.db.get(&key).await? else {
        return Ok(false);
    };
    let Ok(pending) = serde_json::from_str::<PendingCbConv>(&raw) else {
        return Ok(false);
    };
    let text = ctx.text.trim().to_string();
    let (flow, name) = match pending.flow.split_once(':') {
        Some((f, n)) => (f.to_string(), n.to_string()),
        None => return Ok(false),
    };
    if flow == "convo" {
        let Some((var, label, _prompt, back)) = convo_entry(&name) else {
            return Ok(false);
        };
        if is_cancel(&text) {
            ctx.db.del(&key).await?;
            reply_simple(ctx, "Settings cancelled", "Nothing was changed.", "", &back).await?;
            return Ok(true);
        }
        let value = parse_literal(&text);
        setit(ctx, &var, &value).await?;
        ctx.db.del(&key).await?;
        let secret = matches!(
            var.as_str(),
            "RMBG_API" | "DEEP_AI" | "OCR_API" | "SERPAPI" | "VC_SESSION"
        );
        let shown = if secret {
            "Set (hidden)".to_string()
        } else {
            crate::start::plain_text(&value)
        };
        let page = rich_page(
            &format!("{label} updated"),
            &[vec!["Result".to_string(), shown]],
            None,
            &["Setting", "Status"],
            1,
            1,
            Some(1),
            None,
        )?;
        ctx.send_rich_or_plain(&page).await?;
        return Ok(true);
    }
    if flow == "action" {
        return handle_action_reply(ctx, &key, &name, &text).await;
    }
    Ok(false)
}

/// Validate + store one action-flow reply. Returns `true` when the
/// pending flow was consumed (including cancellations / errors).
async fn handle_action_reply(ctx: &mut Ctx, key: &str, action: &str, text: &str) -> Result<bool> {
    let hndlr = ctx.config.hndlr.clone();
    let back_othervars = menu_tok("othervars");
    let cancelled = || is_cancel(text);
    match action {
        "dual_mode_handler" | "handler" | "sudo_handler" => {
            let (var, name) = match action {
                "dual_mode_handler" => ("DUAL_HNDLR", "Dual Handler"),
                "handler" => ("HNDLR", "Handler/Trigger"),
                _ => ("SUDO_HNDLR", "Sudo Handler"),
            };
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Cancelled",
                    "Nothing was changed.",
                    "",
                    &back_othervars,
                )
                .await?;
            } else if text.chars().count() > 1 {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Error", "Incorrect Handler.", "", &back_othervars).await?;
            } else if text.starts_with('/') || text.starts_with('#') || text.starts_with('@') {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Error",
                    "This cannot be used as handler.",
                    "",
                    &back_othervars,
                )
                .await?;
            } else {
                setit(ctx, var, text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    &format!("{name} Changed"),
                    &format!("New value: {text}"),
                    "",
                    &back_othervars,
                )
                .await?;
            }
        }
        "help_emoji" => {
            let name = format!("Emoji in {hndlr}help menu");
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Cancelled",
                    "Nothing was changed.",
                    "",
                    &back_othervars,
                )
                .await?;
            } else if text.starts_with('/') || text.starts_with(&hndlr) {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Error", "Incorrect Emoji.", "", &back_othervars).await?;
            } else {
                setit(ctx, "EMOJI_IN_HELP", text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    &format!("{name} Changed"),
                    &format!("New value: {text}"),
                    "",
                    &back_othervars,
                )
                .await?;
            }
        }
        "plugins" => {
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Cancelled",
                    "Nothing was changed.",
                    "",
                    &back_othervars,
                )
                .await?;
            } else if text.starts_with('/') || text.starts_with(&hndlr) {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Error", "Incorrect channel.", "", &back_othervars).await?;
            } else {
                setit(ctx, "PLUGIN_CHANNEL", text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Plugin Channel Changed",
                    &format!("New value: {text}"),
                    "Restart after setting all things.",
                    &back_othervars,
                )
                .await?;
            }
        }
        "fbangroup" => {
            let back = menu_tok("superfban");
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Cancelled", "Nothing was changed.", "", &back).await?;
            } else {
                setit(ctx, "FBAN_GROUP_ID", text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "FBan Group ID Changed",
                    &format!("New value: {text}"),
                    "",
                    &back,
                )
                .await?;
            }
        }
        "pm_txt" => {
            let back = menu_tok("pm_customisation");
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Cancelled", "Nothing was changed.", "", &back).await?;
            } else if text.chars().count() > 4090 {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Error",
                    "Message too long!\nGive a shorter message please.",
                    "",
                    &back,
                )
                .await?;
            } else {
                setit(ctx, "PM_TEXT", text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "PM Text Changed",
                    &format!("New value: {text}\n\nAfter setting all things, restart."),
                    "",
                    &back,
                )
                .await?;
            }
        }
        "bot_welcome" => {
            let back = menu_tok("chatbot");
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Cancelled", "Nothing was changed.", "", &back).await?;
            } else {
                setit(ctx, "STARTMSG", text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Bot Welcome Message: Changed",
                    &format!("New value: {text}"),
                    "",
                    &back,
                )
                .await?;
            }
        }
        "bot_info_text" => {
            let back = menu_tok("chatbot");
            if text.starts_with('/') {
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Terminated",
                    "The operation was terminated.",
                    "",
                    &back,
                )
                .await?;
            } else {
                setit(ctx, "BOT_INFO_START", text).await?;
                ctx.db.del(key).await?;
                reply_simple(ctx, "Bot Info Text", "Text has been saved.", "", &back).await?;
            }
        }
        "bot_fsub" => {
            let back = menu_tok("chatbot");
            if text == "/clear" {
                ctx.db.del("PMBOT_FSUB").await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Force Subscribe",
                    "Force Subscribe has been stopped and applies to the next incoming message.",
                    "",
                    &back,
                )
                .await?;
            } else if text.starts_with('/') {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Cancelled", "Nothing was changed.", "", &back).await?;
            } else {
                let mut ids: Vec<i64> = Vec::new();
                let mut err = String::new();
                for line in text.lines() {
                    let chat = line.trim();
                    if chat.is_empty() {
                        continue;
                    }
                    match chat.parse::<i64>() {
                        Ok(id)
                            if chat.starts_with('-')
                                || chat.chars().all(|c| c.is_ascii_digit()) =>
                        {
                            ids.push(id)
                        }
                        _ => err.push_str(&format!(
                            "**{chat}** : could not resolve (no entity lookup in this contract)\n"
                        )),
                    }
                }
                if !err.is_empty() {
                    ctx.db.del(key).await?;
                    reply_simple(ctx, "Error", &err, "", &back).await?;
                } else {
                    // Python stores `str(list_of_peer_ids)`; the pmbot
                    // reader parses that legacy literal.
                    let stored = format!("{ids:?}");
                    setit(ctx, "PMBOT_FSUB", &stored).await?;
                    ctx.db.del(key).await?;
                    reply_simple(
                        ctx,
                        "Force Subscribe",
                        "Done. The setting applies to the next incoming message.",
                        "",
                        &back,
                    )
                    .await?;
                }
            }
        }
        "alive_media" | "pm_media" | "inline_pic" => {
            let (var, name, back) = match action {
                "alive_media" => ("ALIVE_PIC", "Alive Media", menu_tok("alive_customisation")),
                "pm_media" => ("PMPIC", "PM Media", menu_tok("pm_customisation")),
                _ => (
                    "INLINE_PIC",
                    "Inline Media",
                    cb(&["assistant", "start", "setter"]).unwrap_or_default(),
                ),
            };
            if cancelled() {
                ctx.db.del(key).await?;
                reply_simple(ctx, "Cancelled", "Nothing was changed.", "", &back).await?;
            } else if text.starts_with("http://") || text.starts_with("https://") {
                // A plain URL / webpage link is stored verbatim (Python
                // `text_to_url` path).
                setit(ctx, var, text).await?;
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    &format!("{name} Changed"),
                    &format!("New value: {text}"),
                    "",
                    &back,
                )
                .await?;
            } else {
                // Sticker / uploaded media needs Telegram media access
                // plus a telegra.ph multipart upload; neither exists
                // in this contract — end as Python does on failure.
                ctx.db.del(key).await?;
                reply_simple(
                    ctx,
                    "Terminated",
                    "The operation was terminated.",
                    "",
                    &back,
                )
                .await?;
            }
        }
        "bot_welcome_media" => {
            let back = menu_tok("chatbot");
            // Python requires an actual media message here; a text
            // reply (no media in `Ctx`) terminates the flow.
            ctx.db.del(key).await?;
            reply_simple(
                ctx,
                "Terminated",
                "The operation was terminated.",
                "",
                &back,
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Start a value-entry prompt flow (Python handlers that delete the
/// menu message and open a conversation).
async fn start_action_prompt(
    ctx: &Ctx,
    action: &str,
    prompt_head: &str,
    prompt: &str,
) -> Result<()> {
    ctx.delete(ctx.message_id).await?;
    store_pending(ctx, &format!("action:{action}")).await?;
    let doc = RichDoc::html(format!(
        "<p>{}</p>",
        escape_html(&fmtmsg(prompt_head, &[], prompt, ""))
    ))?;
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

// ── F-Droid download ──────────────────────────────────────────────────────

fn fd_media() -> &'static Mutex<HashMap<String, String>> {
    static FD: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    FD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn extract_between<'a>(html: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = html.find(start)? + start.len();
    let j = html[i..].find(end)? + i;
    Some(&html[i..j])
}

/// Download an F-Droid APK (Python `fdroid_dler`): scrape the package
/// page for the download link + title + icon, download the APK with
/// `ctx.http`, and send it as a file.
async fn fdroid_download(ctx: &Ctx, uri: &str) -> Result<()> {
    if let Some(path) = fd_media().lock().await.get(uri).cloned() {
        if std::path::Path::new(&path).is_file() {
            ctx.send_file(std::path::Path::new(&path), "").await?;
            return Ok(());
        }
    }
    let url = format!("https://f-droid.org/packages/{uri}");
    let page = ctx.http.get(&url).send().await?.text().await?;
    let href = extract_between(&page, "package-version-download", "</p>")
        .and_then(|seg| extract_between(seg, "href=\"", "\""))
        .ok_or_else(|| Error::Other(format!("F-Droid page for {uri} has no download link")))?;
    let title = extract_between(&page, "package-name", "</h3>")
        .map(|s| s.trim_start_matches('>').trim().to_string())
        .unwrap_or_else(|| uri.to_string());
    let dl = if href.starts_with("http") {
        href.to_string()
    } else {
        format!("https://f-droid.org{href}")
    };
    let bytes = ctx.http.get(&dl).send().await?.bytes().await?;
    let tmp = std::env::temp_dir().join(format!("{title}.apk"));
    tokio::fs::write(&tmp, &bytes).await?;
    ctx.send_file(&tmp, &format!("**  [{title}]({url})  **"))
        .await?;
    fd_media()
        .lock()
        .await
        .insert(uri.to_string(), tmp.to_string_lossy().to_string());
    Ok(())
}

// ── paste ─────────────────────────────────────────────────────────────────

/// Paste text to spaceb.in (Python `fns.get_paste`): returns
/// `(ok, key)`; the public link is `https://spaceb.in/{key}`.
pub async fn spacebin_paste(ctx: &Ctx, text: &str) -> Result<(bool, String)> {
    let resp = ctx
        .http
        .post("https://spaceb.in/api/v1/documents/")
        .form(&[("content", text)])
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return Ok((false, e.to_string())),
    };
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let key = body
        .get("payload")
        .and_then(|p| p.get("id"))
        .or_else(|| body.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        Ok((false, "spaceb.in returned no document id".to_string()))
    } else {
        Ok((true, key))
    }
}

// ── main callback dispatch ────────────────────────────────────────────────

/// Dispatch callback data owned by this module. Returns `true` when
/// the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if data == "noop" {
        return Ok(true);
    }
    if let Some(tail) = cb_tail(data, &MENU_PREFIX)? {
        render_menu(ctx, &tail).await?;
        return Ok(true);
    }
    if let Some(tail) = cb_tail(data, &CONV_PREFIX)? {
        return convo_start(ctx, &tail).await;
    }
    if let Some(tail) = cb_tail(data, &CHANGES_PREFIX)? {
        return changes(ctx, &tail).await;
    }
    if let Some(tail) = cb_tail(data, &WARNS_PREFIX)? {
        return set_pm_warns(ctx, &tail).await;
    }
    if let Some(tail) = cb_tail(data, &PASTE_PREFIX)? {
        return paste(ctx, &tail).await;
    }
    if let Some(tail) = cb_tail(data, &FDROID_PREFIX)? {
        fdroid_download(ctx, &tail).await?;
        return Ok(true);
    }
    if let Some(rest) = data.strip_prefix("assistant:inline:send_plugin:") {
        return send_plugin(ctx, rest).await;
    }
    // Exact single-shot action tokens: assistant:callbacks:<action>.
    let action = data
        .strip_prefix("assistant:callbacks:")
        .unwrap_or("")
        .to_string();
    if data.starts_with("assistant:callbacks:") && !action.contains(':') {
        return handle_action(ctx, &action).await;
    }
    Ok(false)
}

async fn convo_start(ctx: &Ctx, key: &str) -> Result<bool> {
    let Some((var, name, text, back)) = convo_entry(key) else {
        return Ok(false);
    };
    let _ = var;
    ctx.delete(ctx.message_id).await?;
    store_pending(ctx, &format!("convo:{key}")).await?;
    let page = rich_page(
        &name,
        &[vec![
            "Instructions".to_string(),
            crate::start::plain_text(&text),
        ]],
        None,
        &["Step", "What to do"],
        1,
        1,
        Some(1),
        None,
    )?;
    ctx.send_rich_or_plain(&page).await?;
    let _ = back;
    Ok(true)
}

async fn changes(ctx: &Ctx, _tail: &str) -> Result<bool> {
    let _ = ctx;
    Err(Error::Other(
        "changelog/update needs pyUltroid's git-based updater engine (update_engine.engine.check/apply); no updater exists in the ultroid-core contract, so this action cannot run here".to_string(),
    ))
}

async fn set_pm_warns(ctx: &Ctx, tail: &str) -> Result<bool> {
    let Ok(value) = tail.trim().parse::<i64>() else {
        ctx.reply("Invalid warning count.").await?;
        return Ok(true);
    };
    setit(ctx, "PMWARNS", &value.to_string()).await?;
    edit_simple(
        ctx,
        "PM Warnings",
        &format!("New users will have {value} chances in PMs before getting banned."),
        &format!("Currently: {value}"),
        &menu_tok("pm_customisation"),
    )
    .await?;
    Ok(true)
}

async fn paste(ctx: &Ctx, tail: &str) -> Result<bool> {
    let (path, _index) = match tail.split_once('|') {
        Some((p, i)) => (p.to_string(), Some(i.to_string())),
        None => (tail.to_string(), None),
    };
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| Error::Other(format!("plugin source file {path} is unavailable")))?;
    let (ok, key) = spacebin_paste(ctx, &content).await?;
    if !ok {
        ctx.reply(&format!("Paste failed: {key}")).await?;
        return Ok(true);
    }
    ctx.reply(&format!(
        "Link: https://spaceb.in/{key}\nRaw: https://spaceb.in/api/v1/documents/{key}/raw"
    ))
    .await?;
    Ok(true)
}

async fn send_plugin(ctx: &Ctx, rest: &str) -> Result<bool> {
    let (key, name_index) = match rest.split_once(':') {
        Some((k, r)) => (k.to_string(), r.to_string()),
        None => (rest.to_string(), String::new()),
    };
    let (name, _index) = match name_index.rsplit_once(':') {
        Some((n, i)) if !i.contains('/') && !i.contains('.') => {
            (n.to_string(), Some(i.to_string()))
        }
        _ => (name_index, None),
    };
    let folder = match key.as_str() {
        "Official" => "plugins",
        "Addons" => "addons",
        "VCBot" => "vc/plugins",
        _ => return Ok(false),
    };
    let rel = name.trim_start_matches('/').replace('.', "/");
    let candidate = std::path::Path::new(folder).join(format!("{rel}.py"));
    if !candidate.is_file() {
        ctx.reply("The plugin source file is unavailable.").await?;
        return Ok(true);
    }
    Err(Error::Other(format!(
        "plugin source {} resolves, but sending it to the owner in private needs a cross-peer send channel (Responder targets only the current chat)",
        candidate.display()
    )))
}

async fn handle_action(ctx: &mut Ctx, action: &str) -> Result<bool> {
    let hndlr = ctx.config.hndlr.clone();
    match action {
        "updatenow" => Err(Error::Other(
            "update needs pyUltroid's git-based updater engine (update_engine.engine.apply); no updater exists in the ultroid-core contract, so this action cannot run here".to_string(),
        )),
        "authorise" | "folderid" | "gdrive" => {
            if !matches!(ctx.chat_type, ultroid_core::ChatType::Private) {
                return Ok(true);
            }
            if action == "gdrive" {
                edit_simple(
                    ctx,
                    "Google Drive",
                    "Click Authorise and send the code.\n\nYou can use your own CLIENT ID and SECRET.",
                    "",
                    &menu_tok("othervars"),
                )
                .await?;
                return Ok(true);
            }
            Err(Error::Other(
                "Google Drive authorisation needs pyUltroid.fns.gDrive.GDriveManager (OAuth token flow); no Drive client exists in the ultroid-core contract, so this action cannot run here".to_string(),
            ))
        }
        "dual_mode_on" => {
            setit(ctx, "DUAL_MODE", "True").await?;
            edit_simple(ctx, "Dual Mode", "Dual Mode has been turned on.", "Currently: On", &menu_tok("othervars")).await?;
            Ok(true)
        }
        "dual_mode_off" => {
            ctx.db.del("DUAL_MODE").await?;
            edit_simple(ctx, "Dual Mode", "Dual Mode has been turned off.", "Currently: Off", &menu_tok("othervars")).await?;
            Ok(true)
        }
        "dual_mode_handler" => {
            start_action_prompt(
                ctx,
                "dual_mode_handler",
                "Dual Handler",
                &format!("Send the symbol you want as Handler/Trigger for your assistant bot.\nCurrent Handler is {}.\n\nUse /cancel to cancel.", ctx.db.get("DUAL_HNDLR").await.ok().flatten().unwrap_or_else(|| "/".into())),
            )
            .await?;
            Ok(true)
        }
        "help_emoji" => {
            start_action_prompt(
                ctx,
                "help_emoji",
                &format!("Emoji in {hndlr}help menu"),
                "Send the emoji you want to set .\n\nUse /cancel to cancel.",
            )
            .await?;
            Ok(true)
        }
        "plugins" => {
            start_action_prompt(
                ctx,
                "plugins",
                "Plugin Channel",
                "Send the id or username of the channel to install plugins from.\n\nOur Channel: @ultroidplugins\n\nUse /cancel to cancel.",
            )
            .await?;
            Ok(true)
        }
        "handler" => {
            start_action_prompt(
                ctx,
                "handler",
                "Handler/Trigger",
                "Send the symbol you want as Handler/Trigger to use the bot.\nUse /cancel to cancel.",
            )
            .await?;
            Ok(true)
        }
        "sudo_handler" => {
            start_action_prompt(
                ctx,
                "sudo_handler",
                "Sudo Handler",
                "Send the symbol you want as Sudo Handler/Trigger to use the bot.\n\nUse /cancel to cancel.",
            )
            .await?;
            Ok(true)
        }
        "taglogger" => {
            let mut kb = vec![
                vec![InlineButton::data("SET TAG LOG", conv_tok("set_pm_logger").into_bytes())],
                vec![InlineButton::data("DELETE TAG LOG", tok("deltag").into_bytes()).styled(ButtonStyle::Danger)],
                vec![back_button(&menu_tok("othervars"), "Back")],
            ];
            // Keyboard is built with the exact Python buttons; `Ctx::edit`
            // carries no keyboard channel, so the binary crate must attach
            // it to the edit if it wants the buttons visible.
            let _ = &mut kb;
            let foot = format!(
                "Currently: {}",
                val_or_not_set(db_get(ctx, "TAG_LOG").await.ok().flatten().as_deref())
            );
            edit_simple(
                ctx,
                "Tag Logger",
                "Choose whether to set or delete the tag log group.",
                &foot,
                &menu_tok("othervars"),
            )
            .await?;
            Ok(true)
        }
        "deltag" => {
            ctx.db.del("TAG_LOG").await?;
            edit_simple(ctx, "Tag Logger", "Tag Logger has been turned off.", "Currently: Off", &menu_tok("othervars")).await?;
            Ok(true)
        }
        "addons" => {
            let on = is_enabled(db_get(ctx, "ADDONS").await.ok().flatten().as_deref());
            let (label, data, style) = if on {
                ("Addons Off", tok("edof"), ButtonStyle::Danger)
            } else {
                ("Addons On", tok("edon"), ButtonStyle::Success)
            };
            // Toggle row for the exact Python button; see the note above
            // `taglogger` — no keyboard channel exists on `Ctx::edit`.
            let _ = (label, data, style);
            edit_simple(
                ctx,
                "Addons",
                "Extra plugins can be enabled or disabled here.",
                &format!("Currently: {}", toggle_display(db_get(ctx, "ADDONS").await.ok().flatten().as_deref())),
                &menu_tok("othervars"),
            )
            .await?;
            Ok(true)
        }
        "edon" => {
            setit(ctx, "ADDONS", "True").await?;
            edit_simple(ctx, "Addons", "ADDONS has been turned on.\nAfter setting all things, restart.", "Currently: On", &tok("addons")).await?;
            Ok(true)
        }
        "edof" => {
            setit(ctx, "ADDONS", "False").await?;
            edit_simple(ctx, "Addons", "ADDONS has been turned off.\nAfter setting all things, restart.", "Currently: Off", &tok("addons")).await?;
            Ok(true)
        }
        "sudo" => {
            edit_simple(
                ctx,
                "Sudo Mode",
                &format!("Some people you select can use your bot.\nUse {hndlr}help sudo to know more."),
                &format!("Currently: {}", toggle_display(db_get(ctx, "SUDO").await.ok().flatten().as_deref())),
                &menu_tok("othervars"),
            )
            .await?;
            Ok(true)
        }
        "onsudo" => {
            setit(ctx, "SUDO", "True").await?;
            edit_simple(ctx, "Sudo Mode", "SUDO MODE has been turned on.\nAfter setting all things, restart.", "Currently: On", &tok("sudo")).await?;
            Ok(true)
        }
        "ofsudo" => {
            setit(ctx, "SUDO", "False").await?;
            edit_simple(ctx, "Sudo Mode", "SUDO MODE has been turned off.\nAfter setting all things, restart.", "Currently: Off", &tok("sudo")).await?;
            Ok(true)
        }
        "fbangroup" => {
            start_action_prompt(
                ctx,
                "fbangroup",
                "FBan Group ID",
                &format!("Make a group, add @MissRose_Bot, send {hndlr}id, copy that and send it here.\nUse /cancel to go back."),
            )
            .await?;
            Ok(true)
        }
        "alive_media" => {
            start_action_prompt(ctx, "alive_media", "Alive Media", "Send me a pic/gif/media to set as alive media.\n\nUse /cancel to terminate the operation.").await?;
            Ok(true)
        }
        "del_alive_media" => {
            ctx.db.del("ALIVE_PIC").await?;
            edit_simple(ctx, "Alive Media", "Alive media has been deleted.", "Currently: Not set", &menu_tok("alive_customisation")).await?;
            Ok(true)
        }
        "pm_inline" => {
            setit(ctx, "INLINE_PM", "True").await?;
            edit_simple(ctx, "PM Permit Type", "PM Permit type has been set to inline.", "Currently: Inline", &menu_tok("pm_type")).await?;
            Ok(true)
        }
        "pm_normal" => {
            setit(ctx, "INLINE_PM", "False").await?;
            edit_simple(ctx, "PM Permit Type", "PM Permit type has been set to normal.", "Currently: Normal", &menu_tok("pm_type")).await?;
            Ok(true)
        }
        "pm_txt" => {
            start_action_prompt(
                ctx,
                "pm_txt",
                "PM Text",
                "Enter the new PM Permit message.\n\nYou can use {name}, {fullname}, {count}, {mention}, and {username} placeholders.\n\nSend /cancel to stop.",
            )
            .await?;
            Ok(true)
        }
        "set_pm_warns" => {
            let mut rows: Vec<Vec<InlineButton>> = Vec::new();
            let btns: Vec<InlineButton> = (1..=9)
                .map(|x| InlineButton::data(x.to_string(), cb(&["assistant", "callbacks", "pm_warns", &x.to_string()]).unwrap_or_default().into_bytes()).styled(ButtonStyle::Primary))
                .collect();
            for chunk in btns.chunks(3) {
                rows.push(chunk.to_vec());
            }
            rows.push(vec![back_button(&menu_tok("pm_customisation"), "Back")]);
            let foot = format!("Currently: {}", val_or_not_set(db_get(ctx, "PMWARNS").await.ok().flatten().as_deref()));
            edit_simple(
                ctx,
                "PM Warnings",
                "Select the number of warnings for a user before getting blocked in PMs.",
                &foot,
                &menu_tok("pm_customisation"),
            )
            .await?;
            Ok(true)
        }
        "pm_media" => {
            start_action_prompt(ctx, "pm_media", "PM Media", "Send a photo, GIF, sticker, or link for the PM Permit media.\n\nSend /cancel to stop.").await?;
            Ok(true)
        }
        "del_pm_media" => {
            ctx.db.del("PMPIC").await?;
            edit_simple(ctx, "PM Media", "PM media has been deleted.", "Currently: Not set", &menu_tok("pm_customisation")).await?;
            Ok(true)
        }
        "auto_ap_on" => {
            setit(ctx, "AUTOAPPROVE", "True").await?;
            edit_simple(ctx, "Auto Approve", "AUTOAPPROVE has been started.", "Currently: On", &menu_tok("apauto")).await?;
            Ok(true)
        }
        "auto_ap_off" => {
            setit(ctx, "AUTOAPPROVE", "False").await?;
            edit_simple(ctx, "Auto Approve", "AUTOAPPROVE has been stopped.", "Currently: Off", &menu_tok("apauto")).await?;
            Ok(true)
        }
        "pm_logger" => {
            edit_simple(
                ctx,
                "PM Logger",
                "This will forward your PMs to your private group.",
                &format!("Currently: {}", toggle_display(db_get(ctx, "PMLOG").await.ok().flatten().as_deref())),
                &menu_tok("pm_customisation"),
            )
            .await?;
            Ok(true)
        }
        "pm_logger_on" => {
            setit(ctx, "PMLOG", "True").await?;
            edit_simple(ctx, "PM Logger", "PM Logger is on.", "Currently: On", &tok("pm_logger")).await?;
            Ok(true)
        }
        "pm_logger_off" => {
            ctx.db.del("PMLOG").await?;
            edit_simple(ctx, "PM Logger", "PM Logger is off.", "Currently: Off", &tok("pm_logger")).await?;
            Ok(true)
        }
        "pm_permit_on" => {
            setit(ctx, "PMSETTING", "True").await?;
            edit_simple(ctx, "PM Permit", "PM Permit is on.", "Currently: On", &menu_tok("pm_settings")).await?;
            Ok(true)
        }
        "pm_permit_off" => {
            setit(ctx, "PMSETTING", "False").await?;
            edit_simple(ctx, "PM Permit", "PM Permit is off.", "Currently: Off", &menu_tok("pm_settings")).await?;
            Ok(true)
        }
        "bot_welcome_media" => {
            start_action_prompt(ctx, "bot_welcome_media", "Bot Welcome Media", "Send any media to keep at your bot's welcome.").await?;
            Ok(true)
        }
        "bot_info_text" => {
            start_action_prompt(ctx, "bot_info_text", "Bot Info Text", "Send the message to display when a user presses the Info button in Bot Welcome.\n\nSend False to completely remove that button.").await?;
            Ok(true)
        }
        "bot_fsub" => {
            start_action_prompt(ctx, "bot_fsub", "Force Subscribe", "Send the chat ID(s) the user must join before using Chat/PM Bot.\n\nSend /clear to disable PMBot Force Subscribe.\nSend /cancel to stop this process.").await?;
            let _ = ctx.reply("Example\n-1001234567\n-100778888\n\nFor multiple chats.").await;
            Ok(true)
        }
        "bot_welcome" => {
            start_action_prompt(ctx, "bot_welcome", "Bot Welcome Message:", "Enter the message to show when someone starts your assistant bot.\nYou can use {me} and {mention} parameters too.\nUse /cancel to terminate the operation.").await?;
            Ok(true)
        }
        "chatbot_on" => {
            setit(ctx, "PMBOT", "True").await?;
            edit_simple(ctx, "Chat Bot", "Now you can chat with people via this bot.", "Currently: On", &menu_tok("chatbot")).await?;
            Ok(true)
        }
        "chatbot_off" => {
            setit(ctx, "PMBOT", "False").await?;
            edit_simple(ctx, "Chat Bot", "Chat with people via this bot has been stopped.", "Currently: Off", &menu_tok("chatbot")).await?;
            Ok(true)
        }
        "inline_pic" => {
            start_action_prompt(ctx, "inline_pic", "Inline Media", "Send me a pic/gif/link to set as inline media.\n\nUse /cancel to terminate the operation.").await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
