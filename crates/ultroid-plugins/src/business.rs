//! Telegram Business profile — port of `plugins/business.py` (`.biz`).
//!
//! Everything that is pure logic is ported for real: subcommand routing,
//! the work-hours spec parser (`mon-fri 9-17;sat 10-14` -> weekly windows
//! in minutes-since-Monday-00:00), the off-word set, the chat-link slug
//! helper, and the rich/plain confirmation rendering.
//!
//! The Telegram operations themselves are raw MTProto business-profile
//! calls (`pyUltroid/_core/business_profile.py`: `set_greeting_message`,
//! `set_away_message`, `set_intro`, `set_location`, `set_work_hours`,
//! `get_chat_links`, `create_chat_link`, `delete_chat_link`, the
//! `users.GetFullUserRequest` premium/overview probe, the quick-reply
//! shortcut save + `messages.GetQuickRepliesRequest` lookup, and the
//! replied-sticker `InputDocument` resolution). None of those exists on
//! the frozen `Ctx`/`Responder` surface or in `ultroid_core::business`
//! (which only stores business-connection ids and guest state), so each
//! one is an honest `TODO(responder-op)` error at the exact call site,
//! named after the Python wrapper and the TL request it performs.
//!
//! `ui_cards_enabled()` is read from `crate::rebrand` so the rich-card
//! opt-in behaves exactly like Python's.

use crate::rebrand::ui_cards_enabled;
use crate::util::reg_add;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result, RichDoc};

const GREETING_SHORTCUT: &str = "sarahub-biz-greeting";
const AWAY_SHORTCUT: &str = "sarahub-biz-away";
const DAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const OFF_WORDS: [&str; 4] = ["off", "clear", "none", "disable"];

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

fn is_off(text: &str) -> bool {
    OFF_WORDS.contains(&text.trim().to_lowercase().as_str())
}

fn slug_from_link(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// One work-hours window: minutes since Monday 00:00, matching
/// `types.BusinessWeeklyOpen(start_minute, end_minute)`.
#[derive(Debug, Clone, Copy)]
pub struct WeeklyWindow {
    pub start_minute: u32,
    pub end_minute: u32,
}

fn parse_day_range(token: &str) -> std::result::Result<Vec<usize>, String> {
    let token = token.trim().to_lowercase();
    let (a, b) = match token.split_once('-') {
        Some((a, b)) => (a, if b.is_empty() { a } else { b }),
        None => (token.as_str(), token.as_str()),
    };
    let ai = DAY_NAMES
        .iter()
        .position(|d| *d == a)
        .ok_or_else(|| format!("Unknown day '{token}' (use mon/tue/wed/thu/fri/sat/sun)."))?;
    let bi = DAY_NAMES
        .iter()
        .position(|d| *d == b)
        .ok_or_else(|| format!("Unknown day '{token}' (use mon/tue/wed/thu/fri/sat/sun)."))?;
    if ai <= bi {
        Ok((ai..=bi).collect())
    } else {
        Ok((ai..7).chain(0..=bi).collect())
    }
}

fn parse_clock(token: &str) -> std::result::Result<u32, String> {
    let token = token.trim();
    let (hour_str, minute_str) = match token.split_once(':') {
        Some((h, m)) => (h, m),
        None => (token, ""),
    };
    let hour: u32 = hour_str
        .parse()
        .map_err(|_| format!("Bad time '{token}' (expected 'H' or 'H:MM')."))?;
    let minute: u32 = if minute_str.is_empty() {
        0
    } else {
        minute_str
            .parse()
            .map_err(|_| format!("Bad time '{token}' (expected 'H' or 'H:MM')."))?
    };
    if hour > 24 || minute >= 60 {
        return Err(format!("Bad time '{token}' (expected 'H' or 'H:MM')."));
    }
    Ok(hour * 60 + minute)
}

/// Port of `_parse_hours_spec` — `"mon-fri 9-17;sat 10-14"`.
pub fn parse_hours_spec(spec: &str) -> std::result::Result<Vec<WeeklyWindow>, String> {
    let mut windows = Vec::new();
    for group in spec.split(';') {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let mut parts = group.splitn(2, char::is_whitespace);
        let days_token = parts.next().unwrap_or("");
        let time_token = parts.next().ok_or_else(|| {
            format!("Bad hours group '{group}' — expected '<days> <start>-<end>'.")
        })?;
        let (start_token, end_token) = time_token
            .split_once('-')
            .ok_or_else(|| format!("Bad time range '{time_token}' — expected '<start>-<end>'."))?;
        let start_minute = parse_clock(start_token)?;
        let end_minute = parse_clock(end_token)?;
        if end_minute <= start_minute {
            return Err(format!(
                "End time must be after start time in '{time_token}'."
            ));
        }
        for day_index in parse_day_range(days_token)? {
            let base = (day_index as u32) * 1440;
            windows.push(WeeklyWindow {
                start_minute: base + start_minute,
                end_minute: base + end_minute,
            });
        }
    }
    if windows.is_empty() {
        return Err("No work-hours windows parsed — check your spec.".to_string());
    }
    Ok(windows)
}

fn text_from_lines(title: &str, lines: &[String]) -> String {
    let body = lines
        .iter()
        .filter(|l| !l.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if body.is_empty() {
        format!("**{title}**")
    } else {
        format!("**{title}**\n\n{body}")
    }
}

/// Port of `_respond`: a rich confirmation card when UI cards are enabled,
/// otherwise plain text. `ok_msg` is the id of a prior "Processing..."
/// message to edit (plain path); rich edits are not possible through
/// `Responder::edit`, so a rich card is sent as a new message and the
/// placeholder is deleted.
async fn respond(ctx: &Ctx, ok_msg: Option<i64>, title: &str, lines: &[String]) -> Result<()> {
    if ui_cards_enabled(ctx).await {
        let body = lines
            .iter()
            .filter(|l| !l.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let html = if body.is_empty() {
            format!("<h2>{title}</h2>")
        } else {
            format!("<h2>{title}</h2><p>{body}</p>")
        };
        let doc = RichDoc::html(html)?;
        match ctx.send_rich_or_plain(&doc).await {
            Ok(_) => {
                if let Some(id) = ok_msg {
                    let _ = ctx.delete(id).await;
                }
                return Ok(());
            }
            Err(e) => {
                tracing::debug!("business: rich confirmation card failed ({e}); falling back.");
            }
        }
    }
    let text = text_from_lines(title, lines);
    match ok_msg {
        Some(id) => ctx.edit(id, &text).await,
        None => ctx.reply(&text).await.map(|_| ()),
    }
}

/// Shared Premium-required card, used by every subcommand once the
/// Responder op exists (currently `show_overview` errors before it).
#[allow(dead_code)]
async fn premium_required(ctx: &Ctx, ok_msg: Option<i64>, feature: &str) -> Result<()> {
    respond(
        ctx,
        ok_msg,
        &ctx.get_str("biz_title_premium_required"),
        &[
            format!("`{feature}` requires Telegram Premium on this account."),
            ctx.get_str("biz_premium_only_note"),
        ],
    )
    .await
}

/// `# .biz` overview — port of `_show_overview`. Reads the account's full
/// user info; that probe is a raw TL call.
async fn show_overview(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): users.GetFullUserRequest(id="me") + is_premium
    // probe (Python: business.py:_show_overview -> business_profile.is_premium
    // + users.GetFullUserRequest).
    let _ = ctx;
    Err(todo_op(
        "users.GetFullUserRequest(id='me') + Premium probe for the business overview",
        "business.py:_show_overview",
    ))
}

/// Port of `_ensure_quick_reply`: save the text to Saved Messages tagged
/// with the shortcut, then resolve its shortcut id.
async fn ensure_quick_reply(ctx: &Ctx, shortcut: &str, text: &str) -> Result<i64> {
    let _ = (ctx, shortcut, text);
    // TODO(responder-op): send_message("me", text,
    // quick_reply_shortcut=InputQuickReplyShortcut(shortcut)) +
    // messages.GetQuickRepliesRequest(hash=0) lookup
    // (Python: business.py:_ensure_quick_reply).
    Err(todo_op(
        "save quick-reply shortcut + messages.GetQuickRepliesRequest lookup",
        "business.py:_ensure_quick_reply",
    ))
}

async fn cmd_greeting(ctx: &Ctx, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return ctx
            .reply("`Usage: .biz greeting <text>` or `.biz greeting off`.")
            .await
            .map(|_| ());
    }
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    if is_off(rest) {
        // TODO(responder-op): business_profile.clear_greeting_message
        // (Python: business.py:_cmd_greeting).
        return Err(todo_op(
            "business_profile.clear_greeting_message",
            "business.py:_cmd_greeting",
        ));
    }
    let _shortcut_id = ensure_quick_reply(ctx, GREETING_SHORTCUT, rest).await?;
    // TODO(responder-op): business_profile.set_greeting_message(shortcut_id)
    Err(todo_op(
        "business_profile.set_greeting_message",
        "business.py:_cmd_greeting",
    ))
}

async fn cmd_away(ctx: &Ctx, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return ctx
            .reply("`Usage: .biz away [outside|offline|custom ...] <text>` or `.biz away off`. See `.help business`.")
            .await
            .map(|_| ());
    }
    if is_off(rest) {
        let ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
        let _ = ok_msg;
        // TODO(responder-op): business_profile.clear_away_message
        // (Python: business.py:_cmd_away).
        return Err(todo_op(
            "business_profile.clear_away_message",
            "business.py:_cmd_away",
        ));
    }

    let tokens: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
    let modifier = if tokens.len() > 1 {
        tokens[0].to_lowercase()
    } else {
        String::new()
    };
    let (schedule, text, desc) = match modifier.as_str() {
        "outside" => (
            "outside working hours",
            tokens[1].to_string(),
            "outside working hours".to_string(),
        ),
        "offline" => (
            "always, offline-only",
            tokens[1].to_string(),
            "always, offline-only".to_string(),
        ),
        "custom" => {
            let sub: Vec<&str> = tokens[1].splitn(3, char::is_whitespace).collect();
            if sub.len() < 3 {
                return ctx
                    .reply("`Usage: .biz away custom <start_iso> <end_iso> <text>`")
                    .await
                    .map(|_| ());
            }
            let (start_iso, end_iso, text) = (sub[0], sub[1], sub[2]);
            // Validate the ISO-8601 timestamps exactly as Python's
            // `datetime.fromisoformat` does (chrono RFC 3339 parse).
            let parse = |s: &str| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .or_else(|_| {
                        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                            .map(|n| n.and_utc().into())
                    })
                    .map(|_| ())
            };
            if parse(start_iso).is_err() || parse(end_iso).is_err() {
                return ctx
                    .reply("`Dates must be ISO-8601, e.g. 2026-09-01T00:00:00+00:00`")
                    .await
                    .map(|_| ());
            }
            (
                "custom window",
                text.to_string(),
                format!("custom window {start_iso} → {end_iso}"),
            )
        }
        _ => ("always", rest.to_string(), "always".to_string()),
    };
    let _ = schedule;
    let _ = desc;
    let _shortcut_id = ensure_quick_reply(ctx, AWAY_SHORTCUT, &text).await?;
    // TODO(responder-op): business_profile.set_away_message(shortcut_id,
    // schedule, offline_only) (Python: business.py:_cmd_away).
    Err(todo_op(
        "business_profile.set_away_message",
        "business.py:_cmd_away",
    ))
}

async fn cmd_intro(ctx: &Ctx, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return ctx
            .reply("`Usage: .biz intro <title> | <description>` or `.biz intro off`.")
            .await
            .map(|_| ());
    }
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    if is_off(rest) {
        // TODO(responder-op): business_profile.clear_intro
        // (Python: business.py:_cmd_intro).
        return Err(todo_op(
            "business_profile.clear_intro",
            "business.py:_cmd_intro",
        ));
    }
    let Some((title, description)) = rest.split_once('|') else {
        return ctx
            .reply("`Usage: .biz intro <title> | <description>`")
            .await
            .map(|_| ());
    };
    let (title, description) = (title.trim(), description.trim());
    if title.is_empty() || description.is_empty() {
        return ctx
            .reply("`Both a title and a description are required.`")
            .await
            .map(|_| ());
    }
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): resolve the replied sticker to an
        // InputDocument for set_intro(sticker=...) (Python:
        // business.py:_cmd_intro -> tl_utils.get_input_document).
        tracing::debug!("business: replied sticker attach needs a Responder media op");
    }
    // TODO(responder-op): business_profile.set_intro(title, description,
    // sticker) (Python: business.py:_cmd_intro).
    Err(todo_op(
        "business_profile.set_intro",
        "business.py:_cmd_intro",
    ))
}

async fn cmd_location(ctx: &Ctx, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return ctx
            .reply("`Usage: .biz location <address>` or `.biz location off`.")
            .await
            .map(|_| ());
    }
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    if is_off(rest) {
        // TODO(responder-op): business_profile.clear_location
        // (Python: business.py:_cmd_location).
        return Err(todo_op(
            "business_profile.clear_location",
            "business.py:_cmd_location",
        ));
    }
    // TODO(responder-op): business_profile.set_location(address)
    // (Python: business.py:_cmd_location).
    Err(todo_op(
        "business_profile.set_location",
        "business.py:_cmd_location",
    ))
}

async fn cmd_hours(ctx: &Ctx, rest: &str) -> Result<()> {
    let rest = rest.trim();
    if rest.is_empty() {
        return ctx
            .reply(
                "`Usage: .biz hours <timezone> <spec>` or `.biz hours off`. See `.help business`.",
            )
            .await
            .map(|_| ());
    }
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    if is_off(rest) {
        // TODO(responder-op): business_profile.clear_work_hours
        // (Python: business.py:_cmd_hours).
        return Err(todo_op(
            "business_profile.clear_work_hours",
            "business.py:_cmd_hours",
        ));
    }
    let mut parts = rest.splitn(2, char::is_whitespace);
    let tz_id = parts.next().unwrap_or("");
    let Some(spec) = parts.next() else {
        return ctx
            .reply("`Usage: .biz hours <timezone> <spec>` — see `.help business`.")
            .await
            .map(|_| ());
    };
    let windows = match parse_hours_spec(spec) {
        Ok(w) => w,
        Err(e) => {
            return respond(
                ctx,
                Some(_ok_msg),
                &ctx.get_str("biz_title_error"),
                &[format!("`{e}`")],
            )
            .await;
        }
    };
    let _ = (tz_id, windows);
    // TODO(responder-op): business_profile.set_work_hours(schedule)
    // (Python: business.py:_cmd_hours -> business_profile.work_hours +
    // set_work_hours).
    Err(todo_op(
        "business_profile.set_work_hours",
        "business.py:_cmd_hours",
    ))
}

async fn cmd_links(ctx: &Ctx) -> Result<()> {
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): business_profile.get_chat_links
    // (Python: business.py:_cmd_links).
    Err(todo_op(
        "business_profile.get_chat_links",
        "business.py:_cmd_links",
    ))
}

async fn cmd_linknew(ctx: &Ctx, rest: &str) -> Result<()> {
    let mut text = rest.trim().to_string();
    if text.is_empty() {
        text = ctx.reply_to_text.clone().unwrap_or_default();
    }
    if text.is_empty() {
        return ctx
            .reply("`Usage: .biz linknew <text>` (or reply to a message).")
            .await
            .map(|_| ());
    }
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): business_profile.create_chat_link(text)
    // (Python: business.py:_cmd_linknew).
    Err(todo_op(
        "business_profile.create_chat_link",
        "business.py:_cmd_linknew",
    ))
}

async fn cmd_linkdel(ctx: &Ctx, rest: &str) -> Result<()> {
    let slug = rest.trim();
    if slug.is_empty() {
        return ctx.reply("`Usage: .biz linkdel <slug>`").await.map(|_| ());
    }
    let _ = slug_from_link(slug);
    let _ok_msg = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): business_profile.delete_chat_link(slug)
    // (Python: business.py:_cmd_linkdel).
    Err(todo_op(
        "business_profile.delete_chat_link",
        "business.py:_cmd_linkdel",
    ))
}

/// `.biz [subcommand ...]` — port of `business.py::biz_cmd`.
async fn biz(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        return show_overview(ctx).await;
    }
    let mut parts = raw.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("");
    match sub.as_str() {
        "greeting" => cmd_greeting(ctx, rest).await,
        "away" => cmd_away(ctx, rest).await,
        "intro" => cmd_intro(ctx, rest).await,
        "location" => cmd_location(ctx, rest).await,
        "hours" => cmd_hours(ctx, rest).await,
        "links" => cmd_links(ctx).await,
        "linknew" => cmd_linknew(ctx, rest).await,
        "linkdel" => cmd_linkdel(ctx, rest).await,
        _ => ctx
            .reply(&format!(
                "`Unknown .biz subcommand '{sub}'.` See `.help business`."
            ))
            .await
            .map(|_| ()),
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "biz",
            pattern: r"biz(?:\s+([\s\S]*))?$",
            help: "Show/set the Telegram Business profile (greeting, away, intro, location, hours, links).",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(biz(ctx)),
    );
}
