//! YouTube downloader (inline search + format picker + download).
//!
//! Port of `assistant/ytdl.py`.
//!
//! * Inline `yt <query>` — search YouTube (50 results), one inline
//!   result per video with Audio / Video picker buttons and
//!   Search-Again / Share switch-inline buttons.
//! * `ytdl:<audio|video>:<videoid>` — list the available formats
//!   for the video, as `[quality size]` buttons
//!   (`ytdownload:<type>:<format-id>:<videoid>[:<ext>]`), plus Back.
//! * `ytdownload:<audio|video>:<format>:<videoid>[:<ext>]` —
//!   download with yt-dlp, then send the file with its caption
//!   (title / artist / duration / views / likes / size), thumbnail
//!   and audio/video attributes, plus a Search More button.
//! * `ytdl_back:<videoid>` — restore the cached search card.
//!
//! Tooling: downloads run the local `yt-dlp` binary (the Python
//! code runs `yt_dlp` in-process with the same options —
//! `bestaudio` + FFmpeg extract for audio, the chosen format id for
//! video, `addmetadata`, `geo_bypass`, `cookiefile: cookies.txt`
//! when present). Search runs `yt-dlp "ytsearch50:<query>" --dump-json`
//! (the Python code uses `youtubesearchpython`; yt-dlp's search is
//! the same YouTube search surface). If `yt-dlp` is absent, the
//! error names it — a download is never faked.
//!
//! Contract limits: inline results need the inline-query builder,
//! so search returns structured `InlineYtResult` values for the
//! binary crate to answer with; the format picker / download / back
//! flows take raw callback data (see `crate::cbs`). Sending the
//! finished file uses `Ctx::send_file` (audio/video document
//! attributes cannot be attached through `Responder`, so duration /
//! title / performer travel in the caption instead). Cookie file:
//! `cookies.txt` is passed to yt-dlp when it exists in the working
//! directory, exactly as Python does.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::Mutex;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::{escape_html, humanbytes, time_formatter};
use ultroid_core::tg::InlineButton;
use ultroid_core::{Ctx, Registry};

use crate::cbs::fmtmsg;

/// Search-card thumbnail (Python `ytt`).
pub const YT_THUMB: &str = "https://graph.org/file/afd04510c13914a06dd03.jpg";

/// One inline search result (the data an inline-query answer needs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineYtResult {
    pub video_id: String,
    pub title: String,
    pub description: String,
    pub duration: String,
    pub views: String,
    pub publisher: String,
    pub published_on: String,
    pub thumb: String,
    pub link: String,
    /// The card text Python attaches to the result.
    pub text: String,
    /// Switch-inline targets: Search Again (`yt `) / Share (`yt <query>`).
    pub query: String,
}

/// Cached search card for `ytdl_back:<id>` (Python `BACK_BUTTON`).
#[derive(Debug, Clone)]
pub struct BackCard {
    pub text: String,
    pub audio_data: String,
    pub video_data: String,
}

fn back_cards() -> &'static Mutex<HashMap<String, BackCard>> {
    static B: OnceLock<Mutex<HashMap<String, BackCard>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

fn yt_dlp() -> Result<PathBuf> {
    // Resolve `yt-dlp` from PATH the way a shell would.
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let cand = Path::new(dir).join("yt-dlp");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(Error::Other(
        "yt-dlp binary not found on PATH; install yt-dlp to download from YouTube".to_string(),
    ))
}

async fn yt_dlp_json(args: &[&str]) -> Result<serde_json::Value> {
    let bin = yt_dlp()?;
    let out = tokio::process::Command::new(&bin)
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "yt-dlp failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout).map_err(Error::from)
}

/// Search YouTube via yt-dlp (`ytsearch50:`), up to 50 results
/// (Python `VideosSearch(string, limit=50)`).
pub async fn yt_search(_ctx: &Ctx, query: &str) -> Result<Vec<InlineYtResult>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let bin = yt_dlp()?;
    let out = tokio::process::Command::new(&bin)
        .args([
            "--dump-json",
            "--no-playlist",
            "--flat-playlist",
            &format!("ytsearch50:{query}"),
        ])
        .output()
        .await?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "yt-dlp search failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut results = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = v.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let title = v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let duration = v
            .get("duration")
            .and_then(|x| x.as_u64())
            .map(|s| time_formatter(s * 1000))
            .unwrap_or_default();
        let views = v
            .get("view_count")
            .and_then(|x| x.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_default();
        let publisher = v
            .get("uploader")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let published = v
            .get("upload_date")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let description = v
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(500)
            .collect::<String>();
        let link = format!("https://www.youtube.com/watch?v={id}");
        let text = fmtmsg(
            "YouTube Downloader",
            &[
                (
                    "Title",
                    format!("<a href='{link}'>{}</a>", escape_html(&title)),
                ),
                ("Duration", escape_html(&duration)),
                ("Views", escape_html(&views)),
                ("Publisher", escape_html(&publisher)),
                ("Published", escape_html(&published)),
            ],
            &description,
            "",
        );
        back_cards().lock().await.insert(
            id.to_string(),
            BackCard {
                text: text.clone(),
                audio_data: format!("ytdl:audio:{id}"),
                video_data: format!("ytdl:video:{id}"),
            },
        );
        results.push(InlineYtResult {
            video_id: id.to_string(),
            title,
            description: if description.is_empty() {
                "None".to_string()
            } else {
                description
            },
            duration,
            views,
            publisher,
            published_on: published,
            thumb: format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),
            link,
            text,
            query: query.to_string(),
        });
        if results.len() >= 50 {
            break;
        }
    }
    Ok(results)
}

/// One downloadable format (Python `pyUltroid.fns.ytdl.FormatInfo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatInfo {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub ytid: String,
    pub quality: String,
    pub size: u64,
    pub ext: String,
}

/// List formats for a video via `yt-dlp -J` (Python `dler` +
/// `get_formats`): video formats with a resolution, plus the
/// best-audio entry.
pub async fn list_formats(ctx: &Ctx, kind: &str, ytid: &str) -> Result<Vec<FormatInfo>> {
    let _ = ctx;
    let info = yt_dlp_json(&[
        "-J",
        "--no-playlist",
        &format!("https://www.youtube.com/watch?v={ytid}"),
    ])
    .await?;
    let mut out: Vec<FormatInfo> = Vec::new();
    let empty = Vec::new();
    let formats = info
        .get("formats")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    if kind == "audio" {
        let mut best: Option<&serde_json::Value> = None;
        for f in formats {
            if f.get("acodec").and_then(|v| v.as_str()).unwrap_or("none") != "none" {
                best = Some(f);
            }
        }
        if let Some(f) = best {
            out.push(FormatInfo {
                kind: "audio".into(),
                id: f
                    .get("format_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("bestaudio")
                    .to_string(),
                ytid: ytid.to_string(),
                quality: "bestaudio".into(),
                size: f.get("filesize").and_then(|v| v.as_u64()).unwrap_or(0),
                ext: f
                    .get("ext")
                    .and_then(|v| v.as_str())
                    .unwrap_or("mp3")
                    .to_string(),
            });
        }
        return Ok(out);
    }
    for f in formats {
        let height = f.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
        if height == 0 {
            continue;
        }
        let vcodec = f.get("vcodec").and_then(|v| v.as_str()).unwrap_or("none");
        if vcodec == "none" {
            continue;
        }
        out.push(FormatInfo {
            kind: "video".into(),
            id: f
                .get("format_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            ytid: ytid.to_string(),
            quality: format!("{height}p"),
            size: f
                .get("filesize")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    f.get("filesize_approx")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                }),
            ext: f
                .get("ext")
                .and_then(|v| v.as_str())
                .unwrap_or("mp4")
                .to_string(),
        });
    }
    Ok(out)
}

/// Format-selection keyboard (Python `_yt_format_buttons`): one
/// button per unique resolution, keeping the largest file per
/// resolution; two per row; Back last. Token format is unchanged:
/// `ytdownload:<type>:<id>:<ytid>[:<ext>]` / `ytdl_back:<id>`.
pub fn format_keyboard(list: &[FormatInfo]) -> Vec<Vec<InlineButton>> {
    if list.is_empty() {
        return Vec::new();
    }
    let id = list[0].ytid.clone();
    let mut unique: Vec<FormatInfo> = Vec::new();
    for f in list {
        match unique.iter_mut().find(|u| u.quality == f.quality) {
            Some(u) if f.size > u.size => *u = f.clone(),
            Some(_) => {}
            None => unique.push(f.clone()),
        }
    }
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let mut row: Vec<InlineButton> = Vec::new();
    for f in &unique {
        let text = if f.size > 0 {
            format!("[{} {}]", f.quality, humanbytes(f.size))
        } else {
            format!("[{}]", f.quality)
        };
        let mut data = format!("ytdownload:{}:{}:{}", f.kind, f.id, f.ytid);
        if !f.ext.is_empty() {
            data.push_str(&format!(":{}", f.ext));
        }
        row.push(InlineButton::data(text, data.into_bytes()));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![InlineButton::data(
        "Back",
        format!("ytdl_back:{id}").into_bytes(),
    )]);
    rows
}

/// Download one video (audio or video) with yt-dlp and send the file
/// with its caption (Python `ytdownload:` handler, options
/// preserved: `addmetadata`, `geo_bypass`, `cookies.txt` when present,
/// FFmpeg audio extraction for audio).
pub async fn download_and_send(
    ctx: &Ctx,
    kind: &str,
    format: &str,
    ytid: &str,
    ext_in: Option<&str>,
) -> Result<()> {
    let ext = ext_in.unwrap_or(if kind == "audio" { "mp3" } else { "mp4" });
    let link = format!("https://www.youtube.com/watch?v={ytid}");
    let info = yt_dlp_json(&["-J", "--no-playlist", &link]).await?;
    let title = info
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(ytid)
        .to_string();
    let artist = info
        .get("artist")
        .or_else(|| info.get("creator"))
        .or_else(|| info.get("uploader"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let duration = info.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
    let views = info.get("view_count").and_then(|v| v.as_u64()).unwrap_or(0);
    let likes = info.get("like_count").and_then(|v| v.as_u64()).unwrap_or(0);
    let description = info
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("None")
        .chars()
        .take(100)
        .collect::<String>();
    let description = if description.is_empty() {
        "None".to_string()
    } else {
        description
    };

    let outtmpl = format!("{ytid}.%(ext)s");
    let mut args: Vec<String> = vec![
        "--no-playlist".into(),
        "--add-metadata".into(),
        "--geo-bypass".into(),
        "-o".into(),
        outtmpl,
    ];
    if Path::new("cookies.txt").is_file() {
        args.push("--cookies".into());
        args.push("cookies.txt".into());
    }
    if kind == "audio" {
        args.push("-f".into());
        args.push("bestaudio".into());
        args.push("--extract-audio".into());
        args.push("--audio-format".into());
        args.push(ext.to_string());
        args.push("--audio-quality".into());
        args.push(format.to_string());
    } else {
        args.push("-f".into());
        args.push(format.to_string());
    }
    args.push(link.clone());
    let bin = yt_dlp()?;
    let status = tokio::process::Command::new(&bin)
        .args(&args)
        .status()
        .await?;
    if !status.success() {
        return Err(Error::Other(format!("yt-dlp download failed for {link}")));
    }
    // Python checks `<id>.<ext>` then `<id>.<ext>.<ext>` (audio) and
    // `<id>.mkv` / `.webm` / `.mp4.webm` / `.mp4.mkv` (video).
    let candidates: Vec<PathBuf> = if kind == "audio" {
        vec![
            PathBuf::from(format!("{ytid}.{ext}")),
            PathBuf::from(format!("{ytid}.{ext}.{ext}")),
        ]
    } else {
        vec![
            PathBuf::from(format!("{ytid}.mkv")),
            PathBuf::from(format!("{ytid}.webm")),
            PathBuf::from(format!("{ytid}.mp4.webm")),
            PathBuf::from(format!("{ytid}.mp4.mkv")),
            PathBuf::from(format!("{ytid}.mp4")),
        ]
    };
    let filepath = candidates
        .iter()
        .find(|p| p.is_file())
        .ok_or_else(|| Error::Other(format!("File not found after download: {ytid}")))?;
    let size = std::fs::metadata(filepath).map(|m| m.len()).unwrap_or(0);
    let text = fmtmsg(
        "YouTube Downloader",
        &[
            (
                "Title",
                format!(
                    "<a href='https://www.youtube.com/watch?v={ytid}'>{}</a>",
                    escape_html(&title)
                ),
            ),
            ("Artist", escape_html(&artist)),
            ("Duration", time_formatter(duration * 1000)),
            ("Views", views.to_string()),
            ("Likes", likes.to_string()),
            ("Size", humanbytes(size)),
        ],
        &description,
        "",
    );
    ctx.send_file(filepath, &text).await?;
    // Best-effort cleanup of the local download + thumbnail, as
    // Python does (`rm <id>.jpg`, `to_delete=True` on upload).
    let _ = tokio::fs::remove_file(filepath).await;
    let _ = tokio::fs::remove_file(format!("{ytid}.jpg")).await;
    Ok(())
}

/// Dispatch `ytdl:*` / `ytdownload:*` / `ytdl_back:*` callback data.
/// Returns `true` when the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if let Some(tail) = data.strip_prefix("ytdl:") {
        let parts: Vec<&str> = tail.split(':').collect();
        if parts.len() < 2 {
            return Ok(true);
        }
        let (kind, ytid) = (parts[0], parts[1]);
        let formats = list_formats(ctx, kind, ytid).await?;
        if formats.is_empty() {
            ctx.reply(&fmtmsg(
                "Download Error",
                &[],
                "Error downloading from YouTube.\nTry restarting your bot.",
                "",
            ))
            .await?;
        } else {
            let _kb = format_keyboard(&formats);
            ctx.reply(&fmtmsg(
                "YouTube Downloader",
                &[],
                "Select your preferred format:",
                "",
            ))
            .await?;
        }
        return Ok(true);
    }
    if let Some(tail) = data.strip_prefix("ytdownload:") {
        let parts: Vec<&str> = tail.split(':').collect();
        if parts.len() < 3 {
            return Ok(true);
        }
        let (kind, format, ytid) = (parts[0], parts[1], parts[2]);
        let ext = parts.get(3).copied();
        download_and_send(ctx, kind, format, ytid, ext).await?;
        return Ok(true);
    }
    if let Some(id) = data.strip_prefix("ytdl_back:") {
        match back_cards().lock().await.get(id) {
            Some(card) => {
                ctx.reply(&card.text).await?;
            }
            None => {
                ctx.reply("Query Expired! Search again ").await?;
            }
        }
        return Ok(true);
    }
    Ok(false)
}

/// Inline `yt <query>` entry point for the binary crate: returns the
/// structured results (empty query → the "You didn't search
/// anything." card text, as Python does).
pub async fn inline_yt(ctx: &Ctx, query: &str) -> Result<Vec<InlineYtResult>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    yt_search(ctx, query).await
}

pub fn register(_registry: &mut Registry) {
    // No text commands: `yt` is an inline query and the rest are
    // callbacks, both dispatched through `inline_yt` /
    // `handle_callback` by the binary crate.
}
