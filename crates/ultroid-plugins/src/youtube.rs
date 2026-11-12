//! Port of `plugins/youtube.py` — YouTube downloads.
//!
//! `.yta <link>` / `.ytv <link>` — download audio / video.
//! `.ytsa <query>` / `.ytsv <query>` — search, then download audio / video.
//! `.ytdl <link|query|playlist>` — smart downloader.
//! `.ytpl <playlist link>` — download a full playlist.
//!
//! Downloads are real `yt-dlp` invocations over
//! `tokio::process::Command`; files are sent with `send_file` and
//! deleted afterwards, like the Python version. Search uses
//! `yt-dlp "ytsearchN:..." --get-id` / `-j` metadata (real results,
//! never invented).
//!
//! Difference from the Python version: the inline quality keyboard
//! (`.ytdl`'s Audio/360p/720p/1080p/Best buttons and the `uyt:`
//! callback) has no surface on `Ctx`/`Responder` — inline keyboards
//! cannot be sent from a plugin. `.ytdl` on a link therefore replies
//! with the quality options as plain text and points at `.yta` / `.ytv`
//! for the actual download; `.ytdl` on a search query lists the
//! results as text with their video ids.

use crate::util::{def, reg_add};
use std::path::PathBuf;
use tokio::process::Command as ProcCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "youtube",
            "yt(a|v|sa|sv) ?(.*)",
            "YouTube download: `.yta/.ytv <link>`, `.ytsa/.ytsv <query>`.",
        ),
        |ctx| Box::pin(download_from_youtube(ctx)),
    );
    reg_add(
        registry,
        def(
            "ytdl",
            "ytdl ?(.*)",
            "Smart YouTube downloader: `.ytdl <link|query|playlist>`.",
        ),
        |ctx| Box::pin(ytdl_smart(ctx)),
    );
    reg_add(
        registry,
        def(
            "ytpl",
            "ytpl ?(.*)",
            "Download a full playlist: `.ytpl <playlist link>`.",
        ),
        |ctx| Box::pin(ytdl_playlist(ctx)),
    );
}

const YT_ID_RE: &str = r"(?:youtube\.com/(?:watch\?v=|shorts/|embed/|v/)|youtu\.be/)([\w-]{11})";

fn extract_yt_id(url: &str) -> Option<String> {
    regex::Regex::new(YT_ID_RE)
        .ok()?
        .captures(url)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// Search YouTube (real `yt-dlp`) and return up to `limit` results.
async fn search_videos(query: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
    let out = ProcCommand::new("yt-dlp")
        .arg(format!("ytsearch{limit}:{query}"))
        .args(["-j", "--no-playlist", "--no-warnings", "--flat-playlist"])
        .output()
        .await
        .map_err(|e| Error::Other(format!("yt-dlp failed to run ({e}); is yt-dlp installed?")))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect())
}

/// Download `url` with `yt-dlp` (audio or video) into a temp dir, send
/// the resulting file, and clean up. Returns the sent message id.
async fn download_yt(ctx: &Ctx, status_id: i64, url: &str, audio: bool) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ultroid-yt-{}", uuid::Uuid::new_v4().as_simple()));
    tokio::fs::create_dir_all(&dir).await?;
    let out_tpl = dir.join("%(title)s.%(ext)s");
    let mut cmd = ProcCommand::new("yt-dlp");
    cmd.arg("--no-playlist").arg("--no-warnings");
    if audio {
        cmd.args(["-x", "--audio-format", "m4a", "--audio-quality", "192K"]);
    } else {
        cmd.args(["-f", "bestvideo+bestaudio/best"]);
    }
    cmd.arg("-o").arg(&out_tpl).arg(url);
    let out = cmd
        .output()
        .await
        .map_err(|e| Error::Other(format!("yt-dlp failed to run ({e}); is yt-dlp installed?")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        ctx.edit(status_id, &format!("`Download failed: {detail}`"))
            .await?;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut file: Option<PathBuf> = None;
    while let Some(e) = entries.next_entry().await? {
        let p = e.path();
        if p.is_file() {
            file = Some(p);
            break;
        }
    }
    let Some(file) = file else {
        ctx.edit(status_id, "`Download failed: no file produced.`")
            .await?;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Ok(());
    };
    let title = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ctx.delete(status_id).await?;
    ctx.send_file(&file, &format!("`{title}`")).await?;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    Ok(())
}

async fn download_from_youtube(ctx: &Ctx) -> Result<()> {
    let opt = ctx.captures.get(1).cloned().unwrap_or_default();
    let arg = ctx.captures.get(2).cloned().unwrap_or_default();
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    let audio = matches!(opt.as_str(), "a" | "sa");
    match opt.as_str() {
        "a" | "v" => {
            let url = arg.trim();
            if url.is_empty() {
                let key = if audio { "youtube_1" } else { "youtube_3" };
                ctx.edit(status, &ctx.get_str(key)).await?;
                return Ok(());
            }
            // The Python version HEADs the URL first; `yt-dlp` reports
            // unreachable URLs itself, so no separate probe is needed.
            download_yt(ctx, status, url, audio).await?;
        }
        "sa" | "sv" => {
            let query = ctx.text.splitn(2, ' ').nth(1).unwrap_or("").trim();
            if query.is_empty() {
                let key = if audio { "youtube_5" } else { "youtube_7" };
                ctx.edit(status, &ctx.get_str(key)).await?;
                return Ok(());
            }
            let Some(id) = search_videos(query, 1)
                .await?
                .into_iter()
                .next()
                .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
            else {
                ctx.edit(status, &ctx.get_str("unspl_1")).await?;
                return Ok(());
            };
            ctx.edit(
                status,
                &ctx.get_str(if audio { "youtube_6" } else { "youtube_8" }),
            )
            .await?;
            let url = format!("https://www.youtube.com/watch?v={id}");
            download_yt(ctx, status, &url, audio).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn ytdl_smart(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim().to_string();
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    if input.is_empty() {
        ctx.edit(
            status,
            "`Give me a YouTube link or a search query.\nUsage: .ytdl <link|query>  |  .ytdl <playlist link>`",
        )
        .await?;
        return Ok(());
    }
    let first = input.split_whitespace().next().unwrap_or("");
    if first.contains("list=") {
        ctx.edit(status, "`Fetching playlist...`").await?;
        return download_playlist(ctx, status, first).await;
    }
    if first.contains("youtu") {
        let Some(vid) = extract_yt_id(first) else {
            ctx.edit(status, "`Could not parse the YouTube link.`")
                .await?;
            return Ok(());
        };
        // No inline-keyboard surface on `Ctx`: show the quality options
        // as text instead of the Python version's `uyt:` buttons.
        ctx.edit(
            status,
            &format!(
                "`Select quality to download:`\n\nVideo: https://www.youtube.com/watch?v={vid}\n• Audio only — `.yta https://www.youtube.com/watch?v={vid}`\n• Video (best) — `.ytv https://www.youtube.com/watch?v={vid}`\n• Video 360p/720p/1080p — not selectable without inline buttons; `.ytv` takes the best available quality."
            ),
        )
        .await?;
        return Ok(());
    }
    ctx.edit(status, &format!("`Searching YouTube for: {input}...`"))
        .await?;
    let results = search_videos(&input, 10).await?;
    if results.is_empty() {
        ctx.edit(status, &ctx.get_str("unspl_1")).await?;
        return Ok(());
    }
    let mut text = format!("`YouTube search: {input}\nDownload with .yta / .ytv <link>:`");
    for r in &results {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("video");
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let dur = r.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
        let mins = dur / 60;
        let secs = dur % 60;
        let short = if title.chars().count() > 35 {
            format!("{}...", title.chars().take(32).collect::<String>())
        } else {
            title.to_string()
        };
        text.push_str(&format!(
            "\n• {short} ({mins}:{secs:02}) — https://youtu.be/{id}"
        ));
    }
    ctx.edit(status, &text).await?;
    Ok(())
}

async fn ytdl_playlist(ctx: &Ctx) -> Result<()> {
    let link = ctx.args.trim().to_string();
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    if link.is_empty() {
        ctx.edit(status, "`Give me a YouTube playlist link.`")
            .await?;
        return Ok(());
    }
    if !link.contains("list=") {
        ctx.edit(status, "`That doesn't look like a YouTube playlist link.`")
            .await?;
        return Ok(());
    }
    ctx.edit(status, "`Fetching playlist...`").await?;
    download_playlist(ctx, status, &link).await
}

async fn download_playlist(ctx: &Ctx, status: i64, link: &str) -> Result<()> {
    // List the playlist entries with real `yt-dlp`, then download each
    // with `download_yt` (progress is the per-video status edit).
    let out = ProcCommand::new("yt-dlp")
        .args(["-j", "--flat-playlist", "--no-warnings", link])
        .output()
        .await
        .map_err(|e| Error::Other(format!("yt-dlp failed to run ({e}); is yt-dlp installed?")))?;
    if !out.status.success() {
        ctx.edit(status, "`Could not fetch the playlist.`").await?;
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ids: Vec<String> = stdout
        .lines()
        .filter_map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()?
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    if ids.is_empty() {
        ctx.edit(status, &ctx.get_str("unspl_1")).await?;
        return Ok(());
    }
    ctx.edit(
        status,
        &format!("`Playlist: {} video(s) — downloading...`", ids.len()),
    )
    .await?;
    for (i, id) in ids.iter().enumerate() {
        let url = format!("https://www.youtube.com/watch?v={id}");
        // One status message per video: `download_yt` deletes its status
        // id after a successful send, so the playlist status stays alive.
        let st = ctx
            .reply(&format!("`Downloading {}/{}: {id}...`", i + 1, ids.len()))
            .await?;
        download_yt(ctx, st, &url, false).await?;
    }
    ctx.delete(status).await?;
    ctx.reply(&format!("`Playlist done: {} video(s).`", ids.len()))
        .await?;
    Ok(())
}

/// Is the given path a YouTube URL / id? (Python `youtube.py:_YT_RE`.)
pub fn is_youtube_url(s: &str) -> bool {
    extract_yt_id(s).is_some()
}
