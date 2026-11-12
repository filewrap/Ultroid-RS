//! Per-group chat-action notifications — port of `plugins/_chatactions.py`.
//!
//! The `.actions` command and the per-chat settings store are fully ported.
//! The `ChatAction` listener itself (join/leave/kick events) and the `ca_*`
//! inline-button callbacks are event/update-stream behaviour, not commands:
//! the pure decision + rendering logic lives here as callable helpers
//! (`handle_chat_action`, `toggle_action`) for the binary's update loop to
//! wire, and every step that needs live Telegram data (the joining/leaving
//! user, the chat title, the participant count) is marked with the exact
//! `TODO(responder-op)` it needs.

use crate::util::{def, reg_add};
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::tg::InlineButton;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "Manage per-group event notifications (welcome/goodbye/kick/leave).\n\n• `.actions` — open the settings menu\n• `.actions disable` — disable all notifications\n• `.actions <event> on|off` — toggle (welcome/goodbye/kick/leave)";

/// Event types exposed in the `.actions` menu (Python `ACTION_EVENTS`).
pub const ACTION_EVENTS: [&str; 4] = ["welcome", "goodbye", "kick", "leave"];

/// Human labels (Python `_ACTION_LABELS`).
pub fn action_label(event: &str) -> &'static str {
    match event {
        "welcome" => "Welcome",
        "goodbye" => "Goodbye",
        "kick" => "Kick",
        "leave" => "Leave",
        _ => "?",
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "actions",
            pattern: "actions(?: |$)",
            help: HELP,
            groups_only: true,
            ..def("actions", "actions(?: |$)", HELP)
        },
        |ctx| Box::pin(actions_cmd(ctx)),
    );
}

fn actions_key(chat_id: i64) -> String {
    format!("CHAT_ACTIONS_{chat_id}")
}

/// Read a chat's action settings, backfilling missing keys against the
/// defaults: welcome/goodbye ON, kick/leave OFF (Python `_get_actions`).
pub async fn get_actions(ctx: &Ctx, chat_id: i64) -> Result<HashMap<String, bool>> {
    let stored: Option<HashMap<String, bool>> =
        db_get_json(&*ctx.db, &actions_key(chat_id)).await?;
    let mut out = HashMap::new();
    for ev in ACTION_EVENTS {
        let def = matches!(ev, "welcome" | "goodbye");
        out.insert(
            ev.to_string(),
            stored
                .as_ref()
                .and_then(|m| m.get(ev).copied())
                .unwrap_or(def),
        );
    }
    Ok(out)
}

pub async fn set_actions(ctx: &Ctx, chat_id: i64, settings: &HashMap<String, bool>) -> Result<()> {
    db_set_json(&*ctx.db, &actions_key(chat_id), settings).await
}

pub fn action_enabled(settings: &HashMap<String, bool>, event: &str) -> bool {
    settings
        .get(event)
        .copied()
        .unwrap_or_else(|| matches!(event, "welcome" | "goodbye"))
}

/// The `.actions` menu text (Python `_actions_menu_text`).
pub fn actions_menu_text(settings: &HashMap<String, bool>) -> String {
    let mut lines = vec!["**Chat Action Settings**".to_string(), String::new()];
    for ev in ACTION_EVENTS {
        let status = if action_enabled(settings, ev) {
            "ON"
        } else {
            "OFF"
        };
        lines.push(format!("• **{}**: `{status}`", action_label(ev)));
    }
    lines.push(String::new());
    lines.push("Tap a button to toggle, or `.actions <event> on|off`.".to_string());
    lines.join("\n")
}

/// Toggle one event (or `dis` = disable all) and persist — the pure half of
/// the `ca_*` callback (Python `actions_callback`). The callback surface
/// itself is not part of the command `Registry`; see module docs.
pub async fn toggle_action(ctx: &Ctx, chat_id: i64, op: &str) -> Result<HashMap<String, bool>> {
    let mut settings = get_actions(ctx, chat_id).await?;
    if op == "dis" {
        for ev in ACTION_EVENTS {
            settings.insert(ev.to_string(), false);
        }
    } else if ACTION_EVENTS.contains(&op) {
        let cur = action_enabled(&settings, op);
        settings.insert(op.to_string(), !cur);
    }
    set_actions(ctx, chat_id, &settings).await?;
    Ok(settings)
}

async fn actions_cmd(ctx: &Ctx) -> Result<()> {
    let chat_id = ctx.chat_id;
    // Python: parts = event.text.split(maxsplit=2); parts[0] == ".actions".
    let parts: Vec<&str> = ctx.text.splitn(3, ' ').collect();
    if parts.len() == 1 {
        // Menu delivery: Python tries bot-first, then inline, then plain
        // text. The first two need client ops outside the `Ctx` surface, so
        // the port delivers the text fallback (Python's own 3rd tier).
        let settings = get_actions(ctx, chat_id).await?;
        let text = format!(
            "{}\n\n_⚠️ Interactive buttons unavailable here. Use `.actions <event> on|off` to toggle._",
            actions_menu_text(&settings)
        );
        ctx.reply(&text).await?;
        return Ok(());
    }
    let sub = parts[1].to_lowercase();
    if sub == "disable" {
        let mut settings = get_actions(ctx, chat_id).await?;
        for ev in ACTION_EVENTS {
            settings.insert(ev.to_string(), false);
        }
        set_actions(ctx, chat_id, &settings).await?;
        ctx.reply("**All chat action notifications disabled for this chat.**")
            .await?;
        return Ok(());
    }
    if ACTION_EVENTS.contains(&sub.as_str()) {
        if parts.len() == 3
            && (parts[2].eq_ignore_ascii_case("on") || parts[2].eq_ignore_ascii_case("off"))
        {
            let value = parts[2].eq_ignore_ascii_case("on");
            let mut settings = get_actions(ctx, chat_id).await?;
            settings.insert(sub.clone(), value);
            set_actions(ctx, chat_id, &settings).await?;
            ctx.reply(&format!(
                "**{}** notifications turned **{}**.",
                action_label(&sub),
                if value { "ON" } else { "OFF" }
            ))
            .await?;
            return Ok(());
        }
        ctx.reply(&format!("Usage: `.actions {sub} on|off`"))
            .await?;
        return Ok(());
    }
    ctx.reply(
        "Usage:\n• `.actions` — open the settings menu\n• `.actions disable` — disable all notifications\n• `.actions <event> on|off` — toggle (welcome/goodbye/kick/leave)",
    )
    .await?;
    Ok(())
}

/// What a `ChatAction` update is, from the update loop's point of view.
/// The dispatcher fills these from the raw update; the handler below only
/// decides and renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatActionKind {
    Joined,
    Added,
    Kicked,
    Left,
}

/// Decide + deliver one chat-action event (port of Python `DummyHandler`).
/// Callers pass the already-resolved user mention / chat title / participant
/// count — resolving them is a client op the `Ctx` surface does not have
/// (see `TODO(responder-op)` notes in the report).
///
/// * `clean_chat` — whether the chat is in the `CLEANCHAT` list (its action
///   messages are deleted; deletion itself is a responder op).
/// * `must_thank` / `member_count` — the thank-members 100× sticker rule;
///   the sticker send needs a client media op.
pub async fn handle_chat_action(
    ctx: &Ctx,
    kind: ChatActionKind,
    user_mention: &str,
    user_name: &str,
    user_fullname: &str,
    username: &str,
    user_id: i64,
    chat_title: &str,
    member_count: i64,
    clean_chat: bool,
    must_thank: bool,
    kicker_mention: Option<&str>,
) -> Result<()> {
    if clean_chat {
        // TODO(responder-op): delete the service message.
        // Python: _chatactions.py:DummyHandler (CLEANCHAT block).
        return Err(Error::Other(
            "delete service message requires a binary Responder op (Python: plugins/_chatactions.py:DummyHandler)".into(),
        ));
    }
    if must_thank && member_count % 100 == 0 {
        // TODO(responder-op): send the milestone sticker (needs the `stickers`
        // table + a media send). Python: _chatactions.py:DummyHandler.
        return Err(Error::Other(
            "send milestone sticker requires a binary Responder op (Python: plugins/_chatactions.py:DummyHandler)".into(),
        ));
    }
    let settings = get_actions(ctx, ctx.chat_id).await?;
    match kind {
        ChatActionKind::Joined | ChatActionKind::Added => {
            if action_enabled(&settings, "welcome") {
                deliver_welcome(
                    ctx,
                    user_mention,
                    user_name,
                    user_fullname,
                    username,
                    user_id,
                    chat_title,
                    member_count,
                )
                .await?;
            }
        }
        ChatActionKind::Kicked => {
            if action_enabled(&settings, "kick") {
                notify_kick(ctx, user_mention, chat_title, kicker_mention).await?;
            } else if action_enabled(&settings, "goodbye") {
                deliver_farewell(
                    ctx,
                    user_mention,
                    user_name,
                    user_fullname,
                    username,
                    user_id,
                    chat_title,
                    member_count,
                )
                .await?;
            }
        }
        ChatActionKind::Left => {
            if action_enabled(&settings, "goodbye")
                && crate::greetings::get_goodbye_note(ctx, ctx.chat_id)
                    .await?
                    .is_some()
            {
                deliver_farewell(
                    ctx,
                    user_mention,
                    user_name,
                    user_fullname,
                    username,
                    user_id,
                    chat_title,
                    member_count,
                )
                .await?;
            } else if action_enabled(&settings, "leave") {
                notify_leave(ctx, user_mention, chat_title).await?;
            }
        }
    }
    Ok(())
}

/// Parse `[label | url]` button JSON (from `split_msg_button`) into
/// inline URL buttons. `None` when the JSON does not parse.
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

/// Send rendered text, with inline buttons when the note carries them.
/// Python routes button notes through the assistant bot's inline query
/// (`something()` in `_inline.py`); the trait has no STUFF-registry
/// inline path, so this delivers the same message + URL buttons
/// directly via `send_markup_to`, falling back to a plain reply when
/// the stored button JSON does not parse.
async fn send_rendered(ctx: &Ctx, rendered: &str, button: &Option<String>) -> Result<()> {
    match button.as_deref().and_then(parse_buttons) {
        Some(buttons) => {
            ctx.responder
                .send_markup_to(ctx.chat_id, rendered, buttons)
                .await?;
        }
        None => {
            ctx.reply(rendered).await?;
        }
    }
    Ok(())
}

async fn deliver_welcome(
    ctx: &Ctx,
    user_mention: &str,
    user_name: &str,
    user_fullname: &str,
    username: &str,
    user_id: i64,
    chat_title: &str,
    member_count: i64,
) -> Result<()> {
    let Some(note) = crate::greetings::get_welcome_note(ctx, ctx.chat_id).await? else {
        return Ok(());
    };
    if note.button.is_some() {
        if let Some(text) = &note.text {
            let rendered = crate::greetings::render_welcome(
                text,
                user_mention,
                user_name,
                user_fullname,
                username,
                user_id,
                chat_title,
                member_count,
            );
            send_rendered(ctx, &rendered, &note.button).await?;
            return Ok(());
        }
        // Button note with no text falls through to the media branch
        // below (Python sends media + buttons together via `something()`).
    }
    if let Some(media) = &note.media {
        let _ = media;
        // TODO(responder-op): media notes need a media send (file id /
        // telegraph URL). Python: _chatactions.py:_deliver_welcome.
        return Err(Error::Other(
            "send welcome media requires a binary Responder op (Python: plugins/_chatactions.py:_deliver_welcome)".into(),
        ));
    }
    if let Some(text) = &note.text {
        let rendered = crate::greetings::render_welcome(
            text,
            user_mention,
            user_name,
            user_fullname,
            username,
            user_id,
            chat_title,
            member_count,
        );
        ctx.reply(&rendered).await?;
    }
    Ok(())
}

async fn deliver_farewell(
    ctx: &Ctx,
    user_mention: &str,
    user_name: &str,
    user_fullname: &str,
    username: &str,
    user_id: i64,
    chat_title: &str,
    member_count: i64,
) -> Result<()> {
    let Some(note) = crate::greetings::get_goodbye_note(ctx, ctx.chat_id).await? else {
        return Ok(());
    };
    if note.button.is_some() {
        if let Some(text) = &note.text {
            let rendered = crate::greetings::render_farewell(
                text,
                user_mention,
                user_name,
                user_fullname,
                username,
                user_id,
                chat_title,
                member_count,
            );
            send_rendered(ctx, &rendered, &note.button).await?;
            return Ok(());
        }
        // Button note with no text falls through to the media branch
        // below (Python sends media + buttons together via `something()`).
    }
    if let Some(media) = &note.media {
        let _ = media;
        return Err(Error::Other(
            "send goodbye media requires a binary Responder op (Python: plugins/_chatactions.py:_deliver_farewell)".into(),
        ));
    }
    if let Some(text) = &note.text {
        let rendered = crate::greetings::render_farewell(
            text,
            user_mention,
            user_name,
            user_fullname,
            username,
            user_id,
            chat_title,
            member_count,
        );
        ctx.reply(&rendered).await?;
    }
    Ok(())
}

async fn notify_kick(
    ctx: &Ctx,
    user_mention: &str,
    chat_title: &str,
    kicker_mention: Option<&str>,
) -> Result<()> {
    let text = match kicker_mention {
        Some(k) => format!("👢 {k} kicked {user_mention} from **{chat_title}**."),
        None => format!("👢 {user_mention} was kicked from **{chat_title}**."),
    };
    ctx.reply(&text).await?;
    Ok(())
}

async fn notify_leave(ctx: &Ctx, user_mention: &str, chat_title: &str) -> Result<()> {
    ctx.reply(&format!("👋 {user_mention} left **{chat_title}**."))
        .await?;
    Ok(())
}
