//! Special tools — port of `plugins/specialtools.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.getaudio <reply to an audio>` — stage audio to put in a video/GIF.
//! - `.addaudio <reply to Video/gif>` — put the staged audio onto the
//!   replied video/gif.
//! - `.dob <date of birth>` — dd/mm/yyyy age + horoscope.
//! - `.sticker <query>` — search stickers on combot.org.
//! - `.wall <query>` — search HD wallpapers.
//! - `.q <color-optional>` / `.q @username` / `.q r <color-optional>` /
//!   `.q count` — create quote images (Quotly).
//!
//! The docstring also lists `.wspr <username>` ("send secret message"),
//! but no `wspr` handler exists in the Python file, so none is
//! registered here.
//!
//! `getaudio`/`addaudio` work on local file paths given as arguments and
//! on replied-to Telegram media once it is on disk. Downloading the
//! replied media is the single TODO(responder-op) site per command.
//!
//! Note: Python's `addaudio` ends with `File.clear()` then
//! `os.remove(File[0])` — an `IndexError` on every successful run.
//! That bug is not ported: the staged file is removed before clearing.

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use regex::Regex;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "getaudio",
            "getaudio$",
            "Stage audio for addaudio: .getaudio (reply to audio).",
        ),
        |ctx| Box::pin(getaudio(ctx)),
    );
    reg_add(
        registry,
        def(
            "addaudio",
            "addaudio$",
            "Add staged audio to video: .addaudio (reply to video/gif).",
        ),
        |ctx| Box::pin(addaudio(ctx)),
    );
    reg_add(
        registry,
        def(
            "dob",
            r"dob( (.*)|$)",
            "Age + horoscope: .dob <dd/mm/yyyy>.",
        ),
        |ctx| Box::pin(dob(ctx)),
    );
    reg_add(
        registry,
        def(
            "sticker",
            "sticker( (.*)|$)",
            "Search stickers: .sticker <query>.",
        ),
        |ctx| Box::pin(sticker_search(ctx)),
    );
    reg_add(
        registry,
        def(
            "wall",
            "wall( (.*)|$)",
            "Search HD wallpapers: .wall <query>.",
        ),
        |ctx| Box::pin(wall(ctx)),
    );
    reg_add(
        registry,
        def(
            "q",
            "q( (.*)|$)",
            "Create a quote image: .q [color|@user|r|count] (reply to message).",
        ),
        |ctx| Box::pin(quote(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// TODO(responder-op): download replied media to a local file.
fn todo_download(python_ref: &str) -> Error {
    Error::Other(format!(
        "download replied media requires a binary Responder op (Python: {python_ref})"
    ))
}

/// Staged audio file from `.getaudio` (Python's global `File` list).
static STAGED_AUDIO: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

fn staged_audio() -> &'static Mutex<Vec<PathBuf>> {
    STAGED_AUDIO.get_or_init(|| Mutex::new(Vec::new()))
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

/// Local file path among the args, if any.
fn local_input(ctx: &Ctx) -> Option<PathBuf> {
    ctx.args
        .split_whitespace()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

async fn run_ffmpeg(args: &[&str]) -> Result<()> {
    let out = tokio::process::Command::new("ffmpeg")
        .args(args)
        .output()
        .await
        .map_err(|e| Error::Other(format!("ffmpeg not found or failed to run: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

// ── getaudio / addaudio ───────────────────────────────────────────────────

async fn getaudio(ctx: &Ctx) -> Result<()> {
    let input = match local_input(ctx) {
        Some(p) => p,
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_download("specialtools.py:daudtoid"));
            }
            ctx.reply("Reply to an audio/video to stage it").await?;
            return Ok(());
        }
    };
    let msg = ctx.reply("Processing...").await?;
    staged_audio().lock().unwrap().push(input);
    ctx.edit(
        msg,
        "Audio staged — reply to a video with `.addaudio` to use it.",
    )
    .await?;
    Ok(())
}

async fn addaudio(ctx: &Ctx) -> Result<()> {
    let video = match local_input(ctx) {
        Some(p) => p,
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_download("specialtools.py:adaudroid"));
            }
            ctx.reply("Reply to a video/gif to add audio").await?;
            return Ok(());
        }
    };
    let staged = staged_audio().lock().unwrap().first().cloned();
    let Some(audio) = staged.filter(|p| p.exists()) else {
        ctx.reply("First stage an audio with `.getaudio` (reply to an audio)")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let out = tmp_file("output.mp4");
    run_ffmpeg(&[
        "-i",
        video.to_str().unwrap_or(""),
        "-i",
        audio.to_str().unwrap_or(""),
        "-shortest",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, "video with audio").await?;
    ctx.delete(msg).await?;
    let _ = tokio::fs::remove_file(&out).await;
    let _ = tokio::fs::remove_file(&audio).await;
    staged_audio().lock().unwrap().clear();
    Ok(())
}

// ── dob ───────────────────────────────────────────────────────────────────

fn zodiac(month: &str, day: i64) -> &'static str {
    match month {
        "01" => {
            if day < 20 {
                "Capricorn"
            } else {
                "Aquarius"
            }
        }
        "02" => {
            if day < 19 {
                "Aquarius"
            } else {
                "Pisces"
            }
        }
        "03" => {
            if day < 21 {
                "Pisces"
            } else {
                "Aries"
            }
        }
        "04" => {
            if day < 20 {
                "Aries"
            } else {
                "Taurus"
            }
        }
        "05" => {
            if day < 21 {
                "Taurus"
            } else {
                "Gemini"
            }
        }
        "06" => {
            if day < 21 {
                "Gemini"
            } else {
                "Cancer"
            }
        }
        "07" => {
            if day < 23 {
                "Cancer"
            } else {
                "Leo"
            }
        }
        "08" => {
            if day < 23 {
                "Leo"
            } else {
                "Virgo"
            }
        }
        "09" => {
            if day < 23 {
                "Virgo"
            } else {
                "Libra"
            }
        }
        "10" => {
            if day < 23 {
                "Libra"
            } else {
                "Scorpio"
            }
        }
        "11" => {
            if day < 22 {
                "Scorpio"
            } else {
                "Sagittarius"
            }
        }
        _ => {
            if day < 22 {
                "Sagittarius"
            } else {
                "Capricorn"
            }
        }
    }
}

/// Horoscope API (Python `tfhoro`): POST the TF natal function with the
/// same bearer token embedded in the Python source.
async fn horoscope(ctx: &Ctx, sign: &str) -> Result<serde_json::Value> {
    let resp = ctx
        .http
        .post("https://us-central1-tf-natal.cloudfunctions.net/horoscopeapi_test")
        .header("Authorization", "Bearer mmEUtLATc8w_UNnHuR2")
        .json(&serde_json::json!({"sign": sign, "date": "today"}))
        .send()
        .await?;
    let val: serde_json::Value = resp.json().await?;
    Ok(val)
}

async fn dob(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim();
    if input.is_empty() {
        ctx.reply("Give a date of birth: `.dob dd/mm/yyyy` (e.g. `.dob 01/01/1999`)")
            .await?;
        return Ok(());
    }
    let birth = match NaiveDate::parse_from_str(input, "%d/%m/%Y") {
        Ok(d) => d,
        Err(_) => {
            ctx.reply("Give a date of birth: `.dob dd/mm/yyyy` (e.g. `.dob 01/01/1999`)")
                .await?;
            return Ok(());
        }
    };
    let parts: Vec<&str> = input.split('/').collect();
    let (day, month_s) = (parts[0].parse::<i64>().unwrap_or(0), parts[1]);
    // Asia/Kolkata "now", like Python's pytz Asia/Kolkata.
    let now_utc = Utc::now();
    let kolkata = chrono::FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
    let now = now_utc.with_timezone(&kolkata);
    let birth_dt = kolkata
        .from_local_datetime(&birth.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap_or_else(|| {
            kolkata
                .from_local_datetime(&birth.and_hms_opt(0, 0, 0).unwrap())
                .earliest()
                .unwrap()
        });
    let lived_secs = (now - birth_dt).num_seconds() as f64;
    let years_f = lived_secs / (365.242 * 24.0 * 3600.0);
    let years = years_f as i64;
    let months_f = (years_f - years as f64) * 12.0;
    let months = months_f as i64;
    let days_f = (months_f - months as f64) * (365.242 / 12.0);
    let days = days_f as i64;
    let hours_f = (days_f - days as f64) * 24.0;
    let hours = hours_f as i64;
    let mins_f = (hours_f - hours as f64) * 60.0;
    let mins = mins_f as i64;
    let secs = ((mins_f - mins as f64) * 60.0) as i64;
    // Next birthday, exactly like Python (birth year + age + 1, same
    // month/day; Feb 29 falls back to Feb 28). Days-left is computed from
    // this year's birthday, like Python's `cm - today + 1`.
    let this_bday = NaiveDate::from_ymd_opt(now.year(), birth.month(), birth.day())
        .or_else(|| NaiveDate::from_ymd_opt(now.year(), birth.month(), 28))
        .unwrap();
    let days_left = (this_bday - now.date_naive()).num_days() + 1;
    let hp = if days_left == 0 {
        "`Happy BirthDay To U🎉🎊`".to_string()
    } else if days_left < 0 {
        format!("{} Days Left 🥳", 365 + days_left)
    } else {
        format!("{days_left} Days Left 🥳")
    };
    let sign = zodiac(month_s, day);
    let horo = horoscope(ctx, sign)
        .await
        .unwrap_or(serde_json::Value::Null);
    let get = |k: &str| {
        horo.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    // Sender name is not exposed through Ctx; fall back to the sender id.
    let name = format!("User {}", ctx.sender_id);
    ctx.delete(ctx.message_id).await.unwrap_or(());
    ctx.reply(&format!(
        "**Nᴀᴍᴇ** -: {name}\n\n**D.O.B** -:  `{input}`\n\n**Lɪᴠᴇᴅ** -:  `{years}`ʏʀ, `{months}`ᴍ, `{days}`ᴅ, `{hours}`ʜʀ, `{mins}`ᴍɪɴ, `{secs}`sᴇᴄ\n\n**Bɪʀᴛʜᴅᴀʏ** -: `{hp}`\n\n**Zᴏᴅɪᴀᴄ** -: {sign}\n\n⌬**Hᴏʀᴏsᴄᴏᴘᴇ Oɴ {} **\n╰•__{}__\n\n\n    **Lᴜᴄᴋʏ Tɪᴍᴇ**  :-        `{}`\n    **Lᴜᴄᴋʏ Nᴜᴍʙᴇʀ** :-   `{}`\n    **Lᴜᴄᴋʏ Cᴏʟᴏʀ** :-        {}\n    **Mᴏᴏᴅ** :-                   {}",
        get("current_date"),
        get("description"),
        get("lucky_time"),
        get("lucky_number"),
        get("color"),
        get("mood"),
    ))
    .await?;
    Ok(())
}

// ── sticker search (combot.org) ───────────────────────────────────────────

async fn sticker_search(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim();
    if query.is_empty() {
        ctx.reply("`Give something to search`").await?;
        return Ok(());
    }
    let msg = ctx.reply("Processing...").await?;
    let url = format!("https://combot.org/telegram/stickers?q={query}");
    let html = ctx.http.get(&url).send().await?.text().await?;
    // Parse `sticker-pack__header` blocks with regex (no HTML-parser
    // crate in the workspace deps): href + `sticker-pack__title` text.
    let re = Regex::new(
        r#"(?s)<div class="sticker-pack__header".*?<a[^>]*href="([^"]+)".*?<div class="sticker-pack__title"[^>]*>(.*?)</div>"#,
    )?;
    let mut lines = String::from("SᴛɪᴄᴋEʀs Aᴠᴀɪʟᴀʙʟᴇ ~\n\n");
    let mut found = 0;
    for cap in re.captures_iter(&html) {
        let href = cap[1].trim();
        let title = cap[2]
            .replace(|c: char| c.is_whitespace(), " ")
            .trim()
            .to_string();
        let title = html_unescape(&title);
        lines.push_str(&format!("<a href={href}>{title}</a>\n"));
        found += 1;
    }
    if found == 0 {
        ctx.edit(msg, "No sticker packs found for that query")
            .await?;
        return Ok(());
    }
    ctx.edit(msg, &lines).await?;
    Ok(())
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

// ── wallpaper search ──────────────────────────────────────────────────────

async fn wall(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim();
    if query.is_empty() {
        ctx.reply("`Give me something to search..`").await?;
        return Ok(());
    }
    let msg = ctx.reply("Processing...").await?;
    // Python `fns.get_google_images`: scrape Google Images for direct
    // image URLs (`"ou":"..."` entries) and send the first five.
    let url = format!(
        "https://www.google.com/search?tbm=isch&q={}",
        urlencoding(&format!("hd {query}"))
    );
    let html = ctx
        .http
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .send()
        .await?
        .text()
        .await?;
    let re = Regex::new(r#""ou":"(https?://[^"]+)""#)?;
    let mut urls: Vec<String> = Vec::new();
    for cap in re.captures_iter(&html) {
        let u = cap[1].to_string();
        if !urls.contains(&u) {
            urls.push(u);
        }
        if urls.len() >= 5 {
            break;
        }
    }
    if urls.is_empty() {
        ctx.edit(msg, "No wallpapers found for that query").await?;
        return Ok(());
    }
    for u in &urls {
        if let Ok(resp) = ctx.http.get(u).send().await {
            if resp.status().is_success() {
                if let Ok(bytes) = resp.bytes().await {
                    let path = tmp_file("wall.jpg");
                    if tokio::fs::write(&path, &bytes).await.is_ok() {
                        let _ = ctx.send_file(&path, "wallpaper").await;
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }
        }
    }
    ctx.delete(msg).await?;
    Ok(())
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── quotly ────────────────────────────────────────────────────────────────

async fn quote(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to Message..`").await?;
        return Ok(());
    }
    let raw = ctx.args.trim();
    // Parse the optional argument: `r`/`reply`, a count 1-20, `@user`/id,
    // or a background colour (`random` picks one).
    let mut count: usize = 1;
    let mut rest = raw;
    if !raw.is_empty() {
        let mut parts = raw.splitn(2, ' ');
        let first = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        if first == "r" || first == "reply" {
            rest = tail;
        } else if let Ok(n) = first.parse::<usize>() {
            if (1..=20).contains(&n) {
                count = n;
                rest = tail;
            } else {
                rest = tail;
            }
        } else if first.starts_with('@') || first.parse::<i64>().is_ok() {
            // A different sender — resolving the user needs the client;
            // the replied message's sender is used instead.
            rest = tail;
        } else {
            rest = raw;
        }
    }
    if count > 1 {
        // Fetching the previous `count` messages needs the client.
        // TODO(responder-op): get_messages window (Python: specialtools.py:quott_).
        return Err(Error::Other(
            "fetching multiple messages for a multi-quote requires a binary Responder op (Python: specialtools.py:quott_)".into(),
        ));
    }
    let bg = match rest.trim() {
        "" => "#1b1429".to_string(),
        "random" => {
            // Python picks from the shared colour list (see beautify.rs).
            let colors = [
                "AliceBlue",
                "AntiqueWhite",
                "Aqua",
                "Aquamarine",
                "Azure",
                "Beige",
                "Bisque",
                "Black",
                "BlanchedAlmond",
                "Blue",
                "BlueViolet",
                "Brown",
                "BurlyWood",
                "CadetBlue",
                "Chartreuse",
                "Chocolate",
                "Coral",
                "CornflowerBlue",
                "Cornsilk",
                "Crimson",
                "Cyan",
                "DarkBlue",
                "DarkCyan",
                "DarkGoldenRod",
                "DarkGray",
                "DarkGreen",
                "DarkKhaki",
                "DarkMagenta",
                "DarkOliveGreen",
                "DarkOrange",
                "DarkOrchid",
                "DarkRed",
                "DarkSalmon",
                "DarkSeaGreen",
                "DarkSlateBlue",
                "DarkSlateGray",
                "DarkTurquoise",
                "DarkViolet",
                "DeepPink",
                "DeepSkyBlue",
                "DimGray",
                "DodgerBlue",
                "FireBrick",
                "FloralWhite",
                "ForestGreen",
                "Fuchsia",
                "Gainsboro",
                "GhostWhite",
                "Gold",
                "GoldenRod",
                "Gray",
                "Green",
                "GreenYellow",
                "HoneyDew",
                "HotPink",
                "IndianRed",
                "Indigo",
                "Ivory",
                "Khaki",
                "Lavender",
                "LavenderBlush",
                "LawnGreen",
                "LemonChiffon",
                "LightBlue",
            ];
            colors[rand::random::<usize>() % colors.len()].to_string()
        }
        other => other.trim_start_matches('#').to_string(),
    };
    let Some(text) = ctx.reply_to_text.clone().filter(|t| !t.is_empty()) else {
        ctx.reply("`Reply to Message..`").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    // Quotly API (Python `Quotly.create_quotly`): POST the quote payload;
    // `OQAPI` in udB switches to the alternate endpoint.
    let use_oqapi = db_get(ctx, "OQAPI").await?.is_some_and(|v| !v.is_empty());
    let url = if use_oqapi {
        "https://quoteampi.onrender.com/generate"
    } else {
        "https://bot.lyo.su/quote/generate"
    };
    // Only the replied text + sender id are exposed through Ctx; the
    // sender display name/avatar/entities need the client, so the
    // payload carries the sender id as the name fallback.
    let payload = serde_json::json!({
        "type": "quote",
        "format": "webp",
        "backgroundColor": bg,
        "width": 512,
        "height": 768,
        "scale": 2,
        "messages": [{
            "text": text,
            "from": {
                "id": ctx.sender_id,
                "name": format!("User {}", ctx.sender_id),
            },
            "entities": [],
        }],
    });
    let resp = ctx.http.post(url).json(&payload).send().await?;
    let val: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if val.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        ctx.edit(msg, &format!("Quotly failed: {val}")).await?;
        return Err(Error::Other(format!("quotly API failed: {val}")));
    }
    let b64 = val
        .get("result")
        .and_then(|r| r.get("image"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("quotly API bad response: {val}")))?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
        .map_err(|e| Error::Other(format!("quotly base64 decode failed: {e}")))?;
    let out = tmp_file("quote.webp");
    tokio::fs::write(&out, &bytes).await?;
    ctx.send_file(&out, "Quotly by Ultroid").await?;
    ctx.delete(msg).await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}
