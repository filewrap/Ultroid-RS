//! Utilities — port of `plugins/utilities.py` (1,108 LOC).
//!
//! Fully real: `.date` (calendar + fixed-offset timezones — see
//! [`timezone_offset_minutes`]), `.paste` (SpaceBin HTTP),
//! `.telegraph` text pages (telegra.ph account/page HTTP),
//! `.ipinfo` (ipinfo.io HTTP), `.json` (a JSON view of the command
//! context), `.cpy`/`.pst` (text clipboard), `.suggest`/`.quiz`
//! (poll argument parsing/validation), `.rmbg` for a local file path
//! (remove.bg multipart HTTP), `get_video_duration` / `get_thumbnail`
//! (`ffprobe`/`ffmpeg` subprocesses).
//!
//! Telegram operations with no `Responder` equivalent — leaving a
//! channel, dialogs/stats iteration, entity/full-user fetches, invites,
//! media downloads, poll sends, fetching arbitrary messages — return
//! the honest `TODO(responder-op)` error at their call sites.

use std::sync::{Mutex, OnceLock};

use chrono::{Datelike, Local, NaiveDate, Utc};
use serde_json::{Value, json};
use ultroid_core::{ChatType, CommandDef, Ctx, Error, Registry, Result};

use crate::util::{db_get, def, reg_add};

pub const CAPTION_LIMIT: usize = 1024;

// ── Timezones ────────────────────────────────────────────────────────────

/// Fixed UTC offset in minutes for a named zone. `chrono` ships no IANA
/// database in this dependency set, so named zones resolve through this
/// table (no DST handling) and unknown names fall back to UTC. `UTC`,
/// `GMT` and `UTC±H[:MM]` forms parse directly.
pub fn timezone_offset_minutes(name: &str) -> Option<i32> {
    let upper = name.trim().to_uppercase();
    if upper == "UTC" || upper == "GMT" || upper == "Z" {
        return Some(0);
    }
    if let Some(rest) = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
    {
        if let Some(off) = parse_utc_offset(rest) {
            return Some(off);
        }
    }
    let offset = match upper.as_str() {
        "ASIA/KOLKATA" | "ASIA/CALCUTTA" => 330,
        "ASIA/DUBAI" => 240,
        "ASIA/KARACHI" => 300,
        "ASIA/DHAKA" => 360,
        "ASIA/KATHMANDU" => 345,
        "ASIA/SINGAPORE" => 480,
        "ASIA/SHANGHAI" | "ASIA/BEIJING" | "ASIA/CHONGQING" => 480,
        "ASIA/HONG_KONG" => 480,
        "ASIA/TOKYO" => 540,
        "ASIA/SEOUL" => 540,
        "ASIA/JAKARTA" => 420,
        "ASIA/BANGKOK" => 420,
        "ASIA/MANILA" => 480,
        "ASIA/TEHRAN" => 210,
        "ASIA/BAGHDAD" => 180,
        "ASIA/JERUSALEM" | "ASIA/TEL_AVIV" => 120,
        "EUROPE/LONDON" | "EUROPE/DUBLIN" => 0,
        "EUROPE/PARIS" | "EUROPE/BERLIN" | "EUROPE/MADRID" | "EUROPE/ROME" | "EUROPE/AMSTERDAM"
        | "EUROPE/VIENNA" | "EUROPE/BRUSSELS" | "EUROPE/WARSAW" | "EUROPE/STOCKHOLM" => 60,
        "EUROPE/MOSCOW" => 180,
        "EUROPE/ATHENS" | "EUROPE/HELSINKI" => 120,
        "AMERICA/NEW_YORK" | "AMERICA/TORONTO" | "AMERICA/MONTREAL" => -300,
        "AMERICA/CHICAGO" | "AMERICA/MEXICO_CITY" => -360,
        "AMERICA/DENVER" | "AMERICA/PHOENIX" => -420,
        "AMERICA/LOS_ANGELES" | "AMERICA/VANCOUVER" => -480,
        "AMERICA/SAO_PAULO" => -180,
        "AMERICA/BOGOTA" | "AMERICA/LIMA" => -300,
        "AFRICA/CAIRO" => 120,
        "AFRICA/JOHANNESBURG" => 120,
        "AFRICA/LAGOS" => 60,
        "AUSTRALIA/SYDNEY" | "AUSTRALIA/MELBOURNE" => 600,
        "AUSTRALIA/PERTH" => 480,
        "AUSTRALIA/BRISBANE" => 600,
        "PACIFIC/AUCKLAND" => 720,
        _ => return None,
    };
    Some(offset)
}

fn parse_utc_offset(rest: &str) -> Option<i32> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Some(0);
    }
    let (sign, body) = match rest.strip_prefix('+') {
        Some(b) => (1i32, b),
        None => match rest.strip_prefix('-') {
            Some(b) => (-1i32, b),
            None => return None,
        },
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None => (body.parse::<i32>().ok()?, 0),
    };
    if !(0..=14).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some(sign * (h * 60 + m))
}

/// `calendar.month(year, month)` — Monday-first, same layout as Python.
pub fn calendar_month(year: i32, month: u32) -> String {
    let month_name = NaiveDate::from_ymd_opt(year, month, 1)
        .map(|d| d.format("%B").to_string())
        .unwrap_or_default();
    let mut out = format!(
        "{:^20}\nMo Tu We Th Fr Sa Su",
        format!("{month_name} {year}")
    );
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let days = days_in_month(year, month);
    let mut weekday = first.weekday().num_days_from_monday() as usize; // 0 = Mon
    let mut line = String::new();
    for _ in 0..weekday {
        line.push_str("   ");
    }
    for day in 1..=days {
        line.push_str(&format!("{day:>2} "));
        weekday += 1;
        if weekday == 7 {
            out.push('\n');
            out.push_str(line.trim_end());
            line.clear();
            weekday = 0;
        }
    }
    if !line.trim().is_empty() {
        out.push('\n');
        out.push_str(line.trim_end());
    }
    out
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next = NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid month");
    let this = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    (next - this).num_days() as u32
}

// ── SpaceBin paste ───────────────────────────────────────────────────────

/// Port of `fns.get_paste`: POST to SpaceBin, retry once with the last
/// 400,000 chars when the service rejects the length. Returns
/// `(done, key_or_error)`.
pub async fn get_paste(ctx: &Ctx, data: &str) -> (bool, String) {
    let payload = json!({ "content": data, "extension": "txt" });
    let resp = match ctx
        .http
        .post("https://spaceb.in/api/")
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (false, e.to_string()),
    };
    let value: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (false, e.to_string()),
    };
    if let Some(id) = value
        .get("payload")
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
    {
        return (true, id.to_string());
    }
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        if err.contains("the length must be between 2 and 400000.") {
            let tail: String = data
                .chars()
                .rev()
                .take(400_000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            // Recurse once, exactly like Python.
            return Box::pin(get_paste(ctx, &tail)).await;
        }
        return (false, err.to_string());
    }
    (false, value.to_string())
}

// ── Telegraph ────────────────────────────────────────────────────────────

async fn telegraph_token(ctx: &Ctx) -> Result<String> {
    if let Some(token) = db_get(ctx, "_TELEGRAPH_TOKEN")
        .await
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
    {
        return Ok(token);
    }
    let domain = db_get(ctx, "GRAPH_DOMAIN")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "graph.org".to_string());
    let resp: Value = ctx
        .http
        .post(format!("https://api.{domain}/createAccount"))
        .json(&json!({ "short_name": "Ultroid" }))
        .send()
        .await
        .map_err(Error::Http)?
        .json()
        .await
        .map_err(Error::Http)?;
    let token = resp
        .get("result")
        .and_then(|r| r.get("access_token"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("Telegraph createAccount failed: {resp}")))?
        .to_string();
    let _ = crate::util::db_set(ctx, "_TELEGRAPH_TOKEN", &token).await;
    Ok(token)
}

async fn telegraph_create_page(ctx: &Ctx, title: &str, content: &str) -> Result<String> {
    let token = telegraph_token(ctx).await?;
    let domain = db_get(ctx, "GRAPH_DOMAIN")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "graph.org".to_string());
    let resp: Value = ctx
        .http
        .post(format!("https://api.{domain}/createPage"))
        .json(&json!({
            "access_token": token,
            "title": title,
            "content": [{ "tag": "p", "children": [content] }],
        }))
        .send()
        .await
        .map_err(Error::Http)?
        .json()
        .await
        .map_err(Error::Http)?;
    let url = resp
        .get("result")
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("Telegraph createPage failed: {resp}")))?
        .to_string();
    Ok(url)
}

// ── ffprobe / ffmpeg helpers ─────────────────────────────────────────────

/// Port of `get_video_duration`: `ffprobe` duration in seconds.
pub async fn get_video_duration(file_path: &str) -> Option<f64> {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            file_path,
        ])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    text.parse::<f64>().ok()
}

/// Port of `get_thumbnail`: `ffmpeg` frame at 00:00:04.
pub async fn get_thumbnail(file_path: &str, thumbnail_path: &str) -> bool {
    match tokio::process::Command::new("ffmpeg")
        .args([
            "-i",
            file_path,
            "-ss",
            "00:00:04",
            "-vframes",
            "1",
            thumbnail_path,
        ])
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(e) => {
            tracing::error!("Error extracting thumbnail: {e}");
            false
        }
    }
}

// ── t.me link parsing ────────────────────────────────────────────────────

/// Port of `fns.get_chat_and_msgid`: parse a t.me message link into
/// `(chat, message_id)` where chat is a username or a `-100…` id.
pub fn get_chat_and_msgid(link: &str) -> (Option<String>, Option<i64>) {
    let link = link.trim();
    let stripped = link
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("t.me/")
        .trim_start_matches("telegram.me/")
        .trim_end_matches('/');
    let parts: Vec<&str> = stripped.split('/').collect();
    if parts.len() < 2 {
        return (None, None);
    }
    if parts[0] == "c" && parts.len() >= 3 {
        let chat = format!("-100{}", parts[1]);
        let msg = parts[2].parse::<i64>().ok();
        return (Some(chat), msg);
    }
    let chat = parts[0].to_string();
    let msg = parts[1].parse::<i64>().ok();
    (Some(chat), msg)
}

// ── Clipboard (cpy/pst) ──────────────────────────────────────────────────

/// Copied text (Python stores the Telethon message object, including
/// media and formatting; the command context carries text only, so the
/// clipboard holds the replied message's text).
fn clipboard() -> &'static Mutex<Option<String>> {
    static CLIP: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    CLIP.get_or_init(|| Mutex::new(None))
}

// ── Quiz parsing ─────────────────────────────────────────────────────────

/// Classify one `|`-separated part of a `.quiz` command.
pub fn quiz_directive(part: &str) -> (&'static str, String) {
    let lower = part.trim().to_lowercase();
    if let Some(rest) = lower.strip_prefix("answer=").or_else(|| {
        lower
            .strip_prefix("ans=")
            .or_else(|| lower.strip_prefix("correct="))
    }) {
        return ("answer", rest.trim().to_string());
    }
    if let Some(rest) = part.trim().strip_prefix("solution=") {
        return ("solution", rest.trim().to_string());
    }
    if let Some(rest) = part.trim().strip_prefix("Solution=") {
        return ("solution", rest.trim().to_string());
    }
    ("option", part.trim().to_string())
}

pub struct QuizParsed {
    pub question: String,
    pub options: Vec<String>,
    pub answer: usize,
    pub solution: Option<String>,
}

/// Parse + validate a `.quiz` argument string (question | options |
/// answer=N | solution=text). Errors are user-facing usage messages.
pub fn parse_quiz(raw: &str) -> Result<QuizParsed> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::Other(
            "`Usage: .quiz <question> | <opt1> | <opt2> [| ...] [| answer=N] [| solution=text]`"
                .to_string(),
        ));
    }
    let parts: Vec<String> = raw
        .split('|')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(Error::Other(
            "`Usage: .quiz <question> | <opt1> | <opt2> ...`".to_string(),
        ));
    }
    let question = parts[0].clone();
    let mut options: Vec<String> = Vec::new();
    let mut answer_n: Option<usize> = None;
    let mut solution: Option<String> = None;
    for part in &parts[1..] {
        // answer= is case-insensitive; solution= keeps its original case.
        let low = part.to_lowercase();
        let mut matched = false;
        for prefix in ["answer=", "ans=", "correct="] {
            if let Some(rest) = low.strip_prefix(prefix) {
                answer_n = rest.trim().parse::<usize>().ok();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if low.starts_with("solution=") {
            solution = Some(
                part[part.find('=').map(|i| i + 1).unwrap_or(0)..]
                    .trim()
                    .to_string(),
            );
            continue;
        }
        options.push(part.clone());
    }
    if options.len() < 2 {
        return Err(Error::Other(
            "`Need at least 2 options, e.g. .quiz <question> | <opt1> | <opt2>`".to_string(),
        ));
    }
    if options.len() > 10 {
        return Err(Error::Other(
            "`Quiz polls support at most 10 options.`".to_string(),
        ));
    }
    let answer = answer_n.unwrap_or(1);
    if answer < 1 || answer > options.len() {
        return Err(Error::Other(format!(
            "`answer=N must be between 1 and {} (got {answer}).`",
            options.len()
        )));
    }
    if let Some(s) = &solution {
        if s.chars().count() > 200 {
            return Err(Error::Other(
                "`solution=... must be at most 200 characters.`".to_string(),
            ));
        }
    }
    Ok(QuizParsed {
        question,
        options,
        answer,
        solution,
    })
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_kickme(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): channels.LeaveChannelRequest
    // (Python: plugins/utilities.py leave via LeaveChannelRequest(chat_id)).
    let _ = ctx;
    Err(Error::Other(
        "leaving the chat requires a binary Responder op (Python: plugins/utilities.py leave via channels.LeaveChannelRequest)".to_string(),
    ))
}

async fn cmd_date(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        let now = Local::now();
        let cal = calendar_month(now.year(), now.month());
        let d = now.format("Date - %B %d, %Y\nTime- %H:%M:%S").to_string();
        ctx.reply(&format!("`{cal}\n\n{d}`")).await?;
        return Ok(());
    }
    let offset = timezone_offset_minutes(arg);
    let now_utc = Utc::now();
    let (label, text) = match offset {
        Some(off) => {
            let shifted = now_utc + chrono::Duration::minutes(off as i64);
            let cal = calendar_month(shifted.year(), shifted.month());
            let d = shifted
                .format("Date - %B %d, %Y\nTime- %H:%M:%S")
                .to_string();
            (format!(" ({arg})"), format!("`{cal}\n\n{d}`"))
        }
        None => {
            let cal = calendar_month(now_utc.year(), now_utc.month());
            let d = now_utc
                .format("Date - %B %d, %Y\nTime- %H:%M:%S")
                .to_string();
            (
                format!(" ({arg} — unknown timezone, showing UTC)"),
                format!("`{cal}\n\n{d}`"),
            )
        }
    };
    ctx.reply(&format!("{text}{label}")).await?;
    Ok(())
}

async fn cmd_listreserved(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): channels.GetAdminedPublicChannelsRequest
    // (Python: plugins/utilities.py listreserved).
    let _ = ctx;
    Err(Error::Other(
        "listing reserved usernames requires a binary Responder op (Python: plugins/utilities.py listreserved via channels.GetAdminedPublicChannelsRequest)".to_string(),
    ))
}

async fn cmd_stats(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): iter_dialogs + GetBlockedRequest + GetAllStickersRequest
    // (Python: plugins/utilities.py stats).
    let _ = ctx;
    Err(Error::Other(
        "collecting dialog stats requires a binary Responder op (Python: plugins/utilities.py stats via client.iter_dialogs / contacts.GetBlockedRequest / messages.GetAllStickersRequest)".to_string(),
    ))
}

async fn cmd_paste(ctx: &Ctx) -> Result<()> {
    let status = ctx.reply("` 《 Pasting... 》 `").await?;
    let mut message: Option<String> = None;
    if !ctx.args.trim().is_empty() {
        message = Some(ctx.args.trim().to_string());
    } else if ctx.reply_to_id.is_some() {
        if let Some(text) = &ctx.reply_to_text {
            message = Some(text.clone());
        } else {
            // Reply to a file: download it and paste its text.
            let reply_id = ctx.reply_to_id.expect("checked above");
            let dest = std::env::temp_dir().join(format!(
                "ultroid-paste-{}",
                uuid::Uuid::new_v4().as_simple()
            ));
            let path = ctx
                .responder
                .download_media(ctx.chat_id, reply_id as i32, &dest)
                .await?;
            let bytes = tokio::fs::read(&path).await.map_err(Error::Io)?;
            let _ = tokio::fs::remove_file(&path).await;
            message = Some(
                String::from_utf8(bytes)
                    .map_err(|_| Error::Other("the replied file is not text".to_string()))?,
            );
        }
    }
    let Some(message) = message.filter(|m| !m.trim().is_empty()) else {
        ctx.edit(
            status,
            "`Reply to a Message/Document or Give me Some Text !`",
        )
        .await?;
        return Ok(());
    };
    let (done, key) = get_paste(ctx, &message).await;
    if !done {
        ctx.edit(status, &format!("`{key}`")).await?;
        return Ok(());
    }
    let link = format!("https://spaceb.in/{key}");
    let raw = format!("https://spaceb.in/api/{key}/raw");
    let reply_text =
        format!("• **Pasted to SpaceBin :** [Space]({link})\n• **Raw Url :** : [Raw]({raw})");
    ctx.edit(status, &reply_text).await?;
    Ok(())
}

async fn cmd_info(ctx: &Ctx) -> Result<()> {
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim();
    let target: i64 = if !arg.is_empty() {
        match arg.trim_start_matches('@').parse::<i64>() {
            Ok(id) => id,
            Err(_) => ctx.responder.resolve_user(arg).await?.id,
        }
    } else if ctx.reply_to_id.is_some() {
        match ctx.reply_to_sender_id {
            Some(id) => id,
            None => {
                ctx.edit(
                    status,
                    "`Could not determine the sender of the replied message.`",
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        ctx.chat_id
    };
    // NOTE: `user_full` returns the `UserSummary` (id, names, username,
    // premium, bot) — no bio, DC id, photo count, restricted/verified
    // flags, common chats or last-online, so those Python fields are
    // omitted here rather than faked.
    match ctx.responder.user_full(target).await {
        Ok(user) => {
            let caption = format!(
                "**Exᴛʀᴀᴄᴛᴇᴅ Dᴀᴛᴀ Fʀᴏᴍ Tᴇʟᴇɢʀᴀᴍ's Dᴀᴛᴀʙᴀsᴇ**\n**••Tᴇʟᴇɢʀᴀᴍ ID**: `{}`\n**••Pᴇʀᴍᴀɴᴇɴᴛ Lɪɴᴋ**: [Click Here](tg://user?id={})\n**••Fɪʀsᴛ Nᴀᴍᴇ**: `{}`\n**••Sᴇᴄᴏɴᴅ Nᴀᴍᴇ**: `{}`\n**••Usᴇʀɴᴀᴍᴇ**: `{}`\n**••Is Pʀᴇᴍɪᴜᴍ**: `{}`\n**••Is A Bᴏᴛ**: `{}`",
                user.id,
                user.id,
                user.first_name,
                user.last_name.as_deref().unwrap_or("Last Name not found"),
                user.username
                    .as_deref()
                    .map(|u| format!("@{u}"))
                    .unwrap_or_else(|| "No username".to_string()),
                user.premium,
                user.bot,
            );
            ctx.edit(status, &caption).await?;
        }
        Err(_) => {
            // Not a user — render the chat/channel card instead.
            let chat = ctx.responder.chat_info(target).await?;
            let kind = format!("{:?}", chat.kind);
            let caption = format!(
                "**Chat Info**\n**••ID**: `{}`\n**••Title**: `{}`\n**••Usᴇʀɴᴀᴍᴇ**: `{}`\n**••Kind**: `{kind}`",
                chat.id,
                chat.title,
                chat.username
                    .as_deref()
                    .map(|u| format!("@{u}"))
                    .unwrap_or_else(|| "No username".to_string()),
            );
            ctx.edit(status, &caption).await?;
        }
    }
    Ok(())
}

async fn cmd_invite(ctx: &Ctx) -> Result<()> {
    let users = ctx.args.trim();
    if users.is_empty() {
        ctx.reply("Give me a username/id to add, e.g. `.invite @username`")
            .await?;
        return Ok(());
    }
    if ctx.chat_type != ChatType::Group && ctx.chat_type != ChatType::Supergroup {
        ctx.reply("`Use me in a group.`").await?;
        return Ok(());
    }
    // TODO(responder-op): messages.AddChatUserRequest / channels.InviteToChannelRequest
    // (Python: plugins/utilities.py invite).
    Err(Error::Other(
        "inviting users requires a binary Responder op (Python: plugins/utilities.py invite via messages.AddChatUserRequest / channels.InviteToChannelRequest)".to_string(),
    ))
}

async fn cmd_rmbg(ctx: &Ctx) -> Result<()> {
    let api_key = db_get(ctx, "RMBG_API")
        .await
        .ok()
        .flatten()
        .filter(|k| !k.is_empty());
    let Some(api_key) = api_key else {
        ctx.reply("Get your API key from [here](https://www.remove.bg/) for this plugin to work.")
            .await?;
        return Ok(());
    };
    let arg = ctx.args.trim();
    let local_path: Option<String>;
    if !arg.is_empty() && std::path::Path::new(arg).exists() {
        local_path = Some(arg.to_string());
    } else if ctx.reply_to_id.is_some() {
        let reply_id = ctx.reply_to_id.expect("checked above");
        let dest = std::env::temp_dir().join(format!(
            "ultroid-rmbg-reply-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        let downloaded = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        local_path = Some(normalise_image_path(&downloaded).await);
    } else {
        ctx.reply("Use `.rmbg` as reply to a pic to remove its background.")
            .await?;
        return Ok(());
    }
    let Some(path) = local_path else {
        ctx.reply("Use `.rmbg` as reply to a pic to remove its background.")
            .await?;
        return Ok(());
    };
    let lower = path.to_lowercase();
    if !(lower.ends_with("webp")
        || lower.ends_with("jpg")
        || lower.ends_with("png")
        || lower.ends_with("jpeg"))
    {
        let _ = tokio::fs::remove_file(&path).await;
        ctx.reply(&ctx.get_str("com_4")).await?;
        return Ok(());
    }
    // webp → png first (Python converts before upload).
    let mut upload_path = path.clone();
    if lower.ends_with("webp") {
        let bytes = tokio::fs::read(&path).await.map_err(Error::Io)?;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| Error::Other(format!("cannot read webp: {e}")))?;
        let png_path = format!("{path}.png");
        img.save(&png_path)
            .map_err(|e| Error::Other(format!("cannot convert webp: {e}")))?;
        let _ = tokio::fs::remove_file(&path).await;
        upload_path = png_path;
    }
    let status = ctx.reply("`Sending to remove.bg`").await?;
    let file_bytes = tokio::fs::read(&upload_path).await.map_err(Error::Io)?;
    let boundary = format!("----ultroid-{}", rand::random::<u64>());
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image_file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
    body.extend_from_slice(&file_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = ctx
        .http
        .post("https://api.remove.bg/v1.0/removebg")
        .header("X-Api-Key", api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(Error::Http)?;
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.map_err(Error::Http)?;
    let _ = tokio::fs::remove_file(&upload_path).await;
    if !content_type.contains("image") {
        let err_json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let first = err_json
            .get("errors")
            .and_then(|e| e.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let title = first
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let detail = first.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        ctx.edit(status, &format!("**ERROR ~** `{title}`,\n`{detail}`"))
            .await?;
        return Ok(());
    }
    let out_path = std::env::temp_dir().join(format!("ult-rmbg-{}.png", rand::random::<u32>()));
    tokio::fs::write(&out_path, &bytes)
        .await
        .map_err(Error::Io)?;
    // Convert to RGB webp like Python (best effort; the PNG is the deliverable).
    let mut sent = false;
    if let Ok(img) = image::load_from_memory(&bytes) {
        let rgb = img.to_rgb8();
        let webp_path =
            std::env::temp_dir().join(format!("ult-rmbg-{}.webp", rand::random::<u32>()));
        if rgb.save(&webp_path).is_ok() {
            if ctx.send_file(&webp_path, "").await.is_ok() {
                sent = true;
            }
            let _ = tokio::fs::remove_file(&webp_path).await;
        }
        let _ = rgb;
    }
    if ctx.send_file(&out_path, "").await.is_ok() {
        sent = true;
    }
    let _ = tokio::fs::remove_file(&out_path).await;
    let _ = ctx.delete(status).await;
    if !sent {
        return Err(Error::Other(
            "remove.bg returned an image but the send failed".to_string(),
        ));
    }
    Ok(())
}

/// If `path` has no recognised image extension, try decoding it and
/// re-saving as PNG; otherwise return `path` unchanged. Used for a
/// downloaded reply in `.rmbg`, where the download has no filename.
async fn normalise_image_path(path: &std::path::Path) -> String {
    let lower = path.to_string_lossy().to_lowercase();
    if lower.ends_with("webp")
        || lower.ends_with("jpg")
        || lower.ends_with("png")
        || lower.ends_with("jpeg")
    {
        return path.to_string_lossy().into_owned();
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return path.to_string_lossy().into_owned();
    };
    let Ok(img) = image::load_from_memory(&bytes) else {
        return path.to_string_lossy().into_owned();
    };
    let png_path = std::env::temp_dir().join(format!(
        "ultroid-rmbg-reply-{}.png",
        uuid::Uuid::new_v4().as_simple()
    ));
    if img.save(&png_path).is_ok() {
        let _ = tokio::fs::remove_file(path).await;
        return png_path.to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

async fn cmd_telegraph(ctx: &Ctx) -> Result<()> {
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    let title = if ctx.args.trim().is_empty() {
        "Ultroid".to_string()
    } else {
        ctx.args.trim().to_string()
    };
    let Some(reply_text) = ctx.reply_to_text.clone() else {
        ctx.edit(status, "`Reply to Message.`").await?;
        return Ok(());
    };
    // Media replies need a download; a plain-text reply is pasted as-is.
    // (Python distinguishes reply.media; here reply_to_text is only set
    // for text replies, so a media reply also lands here with no text —
    // distinguish by checking whether the text is empty.)
    if reply_text.trim().is_empty() {
        // TODO(responder-op): download replied media for Telegraph upload
        // (Python: plugins/utilities.py telegraphcmd via reply.download_media()).
        return Err(Error::Other(
            "uploading replied media to Telegraph requires a binary Responder op (Python: plugins/utilities.py telegraphcmd via reply.download_media())".to_string(),
        ));
    }
    match telegraph_create_page(ctx, &title, reply_text.trim()).await {
        Ok(url) => {
            ctx.edit(status, &format!("Pasted to Telegraph : [Telegraph]({url})"))
                .await?;
        }
        Err(e) => {
            ctx.edit(status, &format!("`Failed to create Telegraph page: {e}`"))
                .await?;
        }
    }
    Ok(())
}

async fn cmd_json(ctx: &Ctx) -> Result<()> {
    let match_arg = ctx.args.trim();
    let obj = json!({
        "text": ctx.text,
        "chat_id": ctx.chat_id,
        "chat_type": format!("{:?}", ctx.chat_type),
        "sender_id": ctx.sender_id,
        "message_id": ctx.message_id,
        "reply_to_id": ctx.reply_to_id,
        "reply_to_text": ctx.reply_to_text,
        "command": ctx.command,
        "is_owner": ctx.is_owner,
        "is_sudo": ctx.is_sudo,
        "is_dev": ctx.is_dev,
        "is_guest": ctx.is_guest,
        "via_bot": ctx.via_bot,
        "client_kind": format!("{:?}", ctx.client_kind),
    });
    // `.json <field>` drills into one field; `-t` keeps only truthy keys.
    let mut msg = if !match_arg.is_empty() {
        let field = match_arg.split_whitespace().next().unwrap_or("");
        match obj.get(field) {
            Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            None => serde_json::to_string_pretty(&obj).unwrap_or_default(),
        }
    } else {
        serde_json::to_string_pretty(&obj).unwrap_or_default()
    };
    if match_arg.contains("-t") {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&msg) {
            let kept: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(_, v)| {
                    !v.is_null() && v != &Value::Bool(false) && v != &Value::String(String::new())
                })
                .collect();
            msg = serde_json::to_string_pretty(&Value::Object(kept)).unwrap_or_default();
        }
    }
    if msg.len() > 4096 {
        let path = std::env::temp_dir().join("json-ult.txt");
        tokio::fs::write(&path, msg.as_bytes())
            .await
            .map_err(Error::Io)?;
        let r = ctx.send_file(&path, "").await;
        let _ = tokio::fs::remove_file(&path).await;
        let _ = ctx.delete(ctx.message_id).await;
        r?;
    } else {
        ctx.reply(&format!("```{}```", msg)).await?;
    }
    Ok(())
}

async fn cmd_suggest(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let mut text: Option<String> = if !arg.is_empty() {
        Some(arg.to_string())
    } else {
        None
    };
    if ctx.reply_to_id.is_some() && text.is_none() {
        text = match ctx.reply_to_text.clone() {
            Some(t) if !t.trim().is_empty() && t.chars().count() < 35 => Some(t.trim().to_string()),
            _ => Some("Do you Agree to Replied Suggestion ?".to_string()),
        };
    }
    let Some(text) = text else {
        ctx.reply("`Please reply to a message to make a suggestion poll!`")
            .await?;
        return Ok(());
    };
    // TODO(responder-op): send InputMediaPoll (Yes/No)
    // (Python: plugins/utilities.py sugg via send_file(file=InputMediaPoll)).
    let _ = text;
    Err(Error::Other(
        "sending a suggestion poll requires a binary Responder op (Python: plugins/utilities.py sugg via InputMediaPoll)".to_string(),
    ))
}

async fn cmd_quiz(ctx: &Ctx) -> Result<()> {
    let parsed = match parse_quiz(&ctx.args) {
        Ok(p) => p,
        Err(e) => {
            ctx.reply(&e.to_string()).await?;
            return Ok(());
        }
    };
    // TODO(responder-op): send InputMediaPoll (quiz)
    // (Python: plugins/utilities.py quiz_poll via send_file(file=InputMediaPoll(quiz=True))).
    let _ = parsed;
    Err(Error::Other(
        "sending a quiz poll requires a binary Responder op (Python: plugins/utilities.py quiz_poll via InputMediaPoll)".to_string(),
    ))
}

async fn cmd_ipinfo(ctx: &Ctx) -> Result<()> {
    let parts: Vec<&str> = ctx.text.split_whitespace().collect();
    let ipaddr = if parts.len() > 1 {
        format!("/{}", parts[1])
    } else {
        String::new()
    };
    let det: Value = match ctx
        .http
        .get(format!("https://ipinfo.io{ipaddr}/geo"))
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or(Value::Null),
        Err(e) => {
            ctx.reply(&format!("`Failed to fetch IP info: {e}`"))
                .await?;
            return Ok(());
        }
    };
    if det.is_null() || det.get("error").is_some() {
        let err = det.get("error").cloned().unwrap_or_default();
        ctx.reply(&format!(
            "**ERROR:**\n`{}`\n`{}`",
            err.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown"),
            err.get("message").and_then(|v| v.as_str()).unwrap_or("")
        ))
        .await?;
        return Ok(());
    }
    let get = |k: &str, d: &str| det.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
    ctx.reply(&format!(
        "\n**IP Details Fetched.**\n\n**IP:** `{}`\n**City:** `{}`\n**Region:** `{}`\n**Country:** `{}`\n**Co-ordinates:** `{}`\n**Postal Code:** `{}`\n**Time Zone:** `{}`\n",
        get("ip", "Unknown"),
        get("city", "Unknown"),
        get("region", "Unknown"),
        get("country", "Unknown"),
        get("loc", "Unknown"),
        get("postal", "None"),
        get("timezone", "Unknown"),
    ))
    .await?;
    Ok(())
}

async fn cmd_cpy(ctx: &Ctx) -> Result<()> {
    let Some(text) = ctx.reply_to_text.clone() else {
        ctx.reply("Use `.cpy` as reply to a message!").await?;
        return Ok(());
    };
    if text.trim().is_empty() {
        ctx.reply("Use `.cpy` as reply to a message!").await?;
        return Ok(());
    }
    *clipboard().lock().expect("clipboard lock") = Some(text);
    ctx.reply("Copied. Use `.pst` to paste!").await?;
    Ok(())
}

async fn cmd_pst(ctx: &Ctx) -> Result<()> {
    let stored = clipboard().lock().expect("clipboard lock").clone();
    let Some(text) = stored else {
        ctx.reply("Nothing was copied! Use `.cpy` as reply to a message first!")
            .await?;
        return Ok(());
    };
    ctx.reply(&text).await?;
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

async fn cmd_thumb(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply(&ctx.get_str("th_1")).await?;
        return Ok(());
    }
    // TODO(responder-op): download the replied file's thumbnail
    // (Python: plugins/utilities.py thumb_dl via reply.download_media(thumb=-1)).
    Err(Error::Other(
        "downloading a thumbnail requires a binary Responder op (Python: plugins/utilities.py thumb_dl via reply.download_media(thumb=-1))".to_string(),
    ))
}

async fn cmd_getmsg(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if arg.is_empty() {
        ctx.reply("`Please provide a link!`").await?;
        return Ok(());
    }
    let _status = ctx.reply("`Loading...`").await?;
    let (chat, msg) = get_chat_and_msgid(arg);
    if chat.is_none() || msg.is_none() {
        ctx.reply(
            "Invalid link!\nEg: `https://t.me/TeamUltroid/3` or `https://t.me/c/1313492028/3`",
        )
        .await?;
        return Ok(());
    }
    // TODO(responder-op): fetch the message + download/re-upload its media
    // (Python: plugins/utilities.py get_restricted_msg via get_messages +
    // fast_downloader/fast_uploader).
    Err(Error::Other(
        "fetching a message from another chat requires a binary Responder op (Python: plugins/utilities.py get_restricted_msg via client.get_messages)".to_string(),
    ))
}

async fn cmd_lastpm(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): iter_dialogs + iter_messages
    // (Python: plugins/utilities.py get_last_private_message).
    let _ = ctx;
    Err(Error::Other(
        "finding the last received PM requires a binary Responder op (Python: plugins/utilities.py get_last_private_message via client.iter_dialogs/iter_messages)".to_string(),
    ))
}

pub fn register(registry: &mut Registry) {
    let help = "Commands Available -\n• `.kickme` : Leaves the group.\n• `.date [timezone]` : Show Calender (optionally in a given timezone, e.g. `.date Asia/Kolkata`).\n• `.listreserved` : List all usernames (channels/groups) you own.\n• `.stats` : See your profile stats.\n• `.paste` - Include long text / Reply to text file.\n• `.info <username/userid/chatid>` : Reply to someone's msg.\n• `.invite <username/userid>` : Add user to the chat.\n• `.rmbg <reply to pic>` : Remove background from that picture.\n• `.telegraph <reply to media/text>` : Upload media/text to telegraph.\n• `.json <reply to msg>` : Get the json encoding of the message.\n• `.suggest <reply to message> or <poll title>` : Create a Yes/No poll for the replied suggestion.\n• `.quiz <question> | <opt1> | <opt2> [| ...] [| answer=N] [| solution=text]` : Send a quiz-mode poll.\n• `.ipinfo <ipAddress>` : Get info about that IP address.\n• `.cpy <reply to message>` : Copy the replied message, with formatting. Expires in 24hrs.\n• `.pst` : Paste the copied message, with formatting.\n• `.thumb <reply file>` : Download the thumbnail of the replied file.\n• `.getmsg <message link>` : Get messages from chats with forward/copy restrictions.\n• `.lastpm` : Get details of the last pm you recieved";
    reg_add(
        registry,
        CommandDef {
            name: "kickme",
            pattern: "kickme$",
            help,
            fullsudo: true,
            ..def("kickme", "kickme$", help)
        },
        |ctx| Box::pin(cmd_kickme(ctx)),
    );
    reg_add(registry, def("date", "date( (.*)|$)", help), |ctx| {
        Box::pin(cmd_date(ctx))
    });
    reg_add(
        registry,
        def("listreserved", "listreserved$", help),
        |ctx| Box::pin(cmd_listreserved(ctx)),
    );
    reg_add(registry, def("stats", "stats$", help), |ctx| {
        Box::pin(cmd_stats(ctx))
    });
    reg_add(registry, def("paste", "paste( (.*)|$)", help), |ctx| {
        Box::pin(cmd_paste(ctx))
    });
    reg_add(registry, def("info", "info( (.*)|$)", help), |ctx| {
        Box::pin(cmd_info(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "invite",
            pattern: "invite( (.*)|$)",
            help,
            groups_only: true,
            ..def("invite", "invite( (.*)|$)", help)
        },
        |ctx| Box::pin(cmd_invite(ctx)),
    );
    reg_add(registry, def("rmbg", "rmbg($| (.*))", help), |ctx| {
        Box::pin(cmd_rmbg(ctx))
    });
    reg_add(
        registry,
        def("telegraph", "telegraph( (.*)|$)", help),
        |ctx| Box::pin(cmd_telegraph(ctx)),
    );
    reg_add(registry, def("json", "json( (.*)|$)", help), |ctx| {
        Box::pin(cmd_json(ctx))
    });
    reg_add(registry, def("suggest", "suggest( (.*)|$)", help), |ctx| {
        Box::pin(cmd_suggest(ctx))
    });
    reg_add(registry, def("quiz", "quiz( (.*)|$)", help), |ctx| {
        Box::pin(cmd_quiz(ctx))
    });
    reg_add(registry, def("ipinfo", "ipinfo( (.*)|$)", help), |ctx| {
        Box::pin(cmd_ipinfo(ctx))
    });
    reg_add(registry, def("cpy", "cpy$", help), |ctx| {
        Box::pin(cmd_cpy(ctx))
    });
    reg_add(registry, def("pst", "pst$", help), |ctx| {
        Box::pin(cmd_pst(ctx))
    });
    reg_add(registry, def("thumb", "thumb$", help), |ctx| {
        Box::pin(cmd_thumb(ctx))
    });
    reg_add(registry, def("getmsg", "getmsg( ?(.*)|$)", help), |ctx| {
        Box::pin(cmd_getmsg(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            name: "lastpm",
            pattern: "lastpm$",
            help,
            fullsudo: true,
            ..def("lastpm", "lastpm$", help)
        },
        |ctx| Box::pin(cmd_lastpm(ctx)),
    );
}
