//! YouTube providers. Port of `vc/_youtube.py` (575 LOC).
//!
//! Two classes live here, exactly as in Python:
//!
//! * `YouTube` — the low-level helper (`exists`, `details`, `title`,
//!   `duration`, `thumbnail`, `video`, `playlist`, `track`,
//!   `formats`, `slider`, `download`). Its `VideosSearch` calls are
//!   ported as direct InnerTube `search` requests —
//!   `youtubesearchpython` 1.6.6 is itself just a thin wrapper over
//!   `POST https://www.youtube.com/youtubei/v1/search` with the WEB
//!   client (`clientVersion 2.20210224.06.00`, key
//!   `AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8`), parsing
//!   `videoRenderer` components; that request and parse are
//!   reproduced here (`videos_search`). Its `yt_dlp.YoutubeDL`
//!   library calls are ported as `yt-dlp` binary invocations with
//!   the equivalent CLI flags (`-J` for `extract_info`, `-g` for
//!   URL printing, `-f`/`-o` for downloads).
//! * `YouTubeData` — the `MusicService` provider
//!   (`is_valid`/`get_info`/`search`/`get_recommendations`/
//!   `get_track`/`download_track`), same query contract as
//!   `ApiData`/`JiosaavnData`.
//!
//! The legacy helper's `download` outtmpl is the literal
//! `downloads/` directory from Python (distinct from
//! `DOWNLOADS_DIR` = `./temp` used by `YouTubeData`).
//!
//! A missing `yt-dlp` binary is an honest error naming `yt-dlp`,
//! never a fabricated result.

use crate::dataclass::{MusicTrack, PlatformTracks, TrackInfo};
use crate::downloader::MusicService;
use crate::tools::DOWNLOADS_DIR;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;
use tracing::error;
use ultroid_core::error::{Error, Result};

/// Legacy download directory used by `YouTube::download`
/// (mirrors the literal `"downloads/"` in `_youtube.py`).
pub const LEGACY_DOWNLOADS_DIR: &str = "downloads";

const WATCH_BASE: &str = "https://www.youtube.com/watch?v=";
const LIST_BASE: &str = "https://youtube.com/playlist?list=";

/// InnerTube search: public key + WEB client from
/// `youtubesearchpython` 1.6.6 `core/constants.py` (verified against
/// the released wheel).
const SEARCH_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const SEARCH_CLIENT_VERSION: &str = "2.20210224.06.00";

fn yt_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:youtube\.com/(?:watch\?.*v=|shorts/)|youtu\.be/)([A-Za-z0-9_-]{11})")
            .expect("static YouTube URL regex")
    })
}

fn yt_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\w-]{11}$").expect("static YouTube id regex"))
}

fn any_yt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"youtube\.com|youtu\.be").expect("static any-YouTube regex"))
}

/// Parse a `H:MM:SS` / `MM:SS` duration string (mirrors
/// `time_to_seconds`: `sum(int(x) * 60**i for reversed split)`).
pub fn time_to_seconds(value: &str) -> Result<u64> {
    let mut total: u64 = 0;
    let mut factor: u64 = 1;
    for part in value.split(':').rev() {
        let n: u64 = part
            .parse()
            .map_err(|_| Error::Other(format!("bad duration part {part:?} in {value:?}")))?;
        total = total
            .checked_add(
                n.checked_mul(factor)
                    .ok_or_else(|| Error::Other(format!("duration overflow in {value:?}")))?,
            )
            .ok_or_else(|| Error::Other(format!("duration overflow in {value:?}")))?;
        factor = factor.saturating_mul(60);
    }
    Ok(total)
}

/// Parse a duration string, `0` for `None`/unparseable (mirrors
/// `_youtube._duration_to_seconds`).
pub fn duration_to_seconds(value: Option<&str>) -> u64 {
    match value {
        Some(v) => time_to_seconds(v).unwrap_or(0),
        None => 0,
    }
}

/// Run a shell command, returning stdout — or stderr when there is
/// any, unless it only says unavailable videos are hidden (mirrors
/// `shell_cmd`).
pub async fn shell_cmd(cmd: &str) -> String {
    let output = Command::new("sh").arg("-c").arg(cmd).output().await;
    match output {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if !err.is_empty() {
                if err.to_lowercase().contains("unavailable videos are hidden") {
                    out
                } else {
                    err
                }
            } else {
                out
            }
        }
        Err(e) => format!("shell_cmd failed: {e}"),
    }
}

/// Run the `yt-dlp` binary. A missing binary is an honest error
/// naming `yt-dlp`.
pub(crate) async fn run_ytdlp(args: &[&str]) -> Result<Output> {
    let output = Command::new("yt-dlp")
        .args(args)
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Other(
                    "yt-dlp binary not found in PATH (required for YouTube extraction/download)"
                        .to_string(),
                )
            } else {
                Error::Io(e)
            }
        })?;
    Ok(output)
}

/// Run `yt-dlp -J …` and parse the JSON info dict. Non-zero exits
/// become an error carrying yt-dlp's stderr.
pub(crate) async fn ytdlp_json(args: &[&str]) -> Result<serde_json::Value> {
    let mut full: Vec<&str> = vec!["-J"];
    full.extend_from_slice(args);
    let output = run_ytdlp(&full).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Other(format!("yt-dlp failed: {}", stderr.trim())));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(value)
}

// ── InnerTube search (youtubesearchpython VideosSearch) ─────────────────

/// One video row from an InnerTube search response (the component
/// `youtubesearchpython` builds from `videoRenderer`).
#[derive(Debug, Clone, Default)]
pub struct SearchRow {
    pub id: String,
    pub title: String,
    pub duration: Option<String>,
    pub thumbnails: Vec<String>,
    pub channel_name: String,
    pub link: String,
}

fn text_of(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    if let Some(s) = v.get("simpleText").and_then(|s| s.as_str()) {
        return Some(s.to_string());
    }
    v.get("runs")
        .and_then(|r| r.as_array())
        .and_then(|runs| runs.first())
        .and_then(|r| r.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

fn row_from_renderer(video: &serde_json::Value) -> Option<SearchRow> {
    let id = video.get("videoId")?.as_str()?.to_string();
    let title = text_of(video.get("title")).unwrap_or_default();
    let duration = video
        .get("lengthText")
        .and_then(|l| l.get("simpleText"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let thumbnails = video
        .get("thumbnail")
        .and_then(|t| t.get("thumbnails"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let channel_name = video
        .get("ownerText")
        .and_then(|o| text_of(Some(o)))
        .unwrap_or_default();
    let link = format!("{WATCH_BASE}{id}");
    Some(SearchRow {
        id,
        title,
        duration,
        thumbnails,
        channel_name,
        link,
    })
}

/// Extract video rows from one page's parsed `contents` list
/// (section list or rich grid), following the same branches
/// `youtubesearchpython` does: `videoRenderer`, `shelfRenderer`,
/// `richItemRenderer`.
fn rows_from_contents(contents: &[serde_json::Value], out: &mut Vec<SearchRow>) {
    for element in contents {
        if let Some(video) = element.get("videoRenderer") {
            if let Some(row) = row_from_renderer(video) {
                out.push(row);
            }
        } else if let Some(shelf) = element.get("shelfRenderer") {
            if let Some(items) = shelf
                .get("content")
                .and_then(|c| c.get("horizontalListRenderer"))
                .and_then(|h| h.get("items"))
                .and_then(|i| i.as_array())
            {
                for item in items {
                    if let Some(video) = item.get("videoRenderer") {
                        if let Some(row) = row_from_renderer(video) {
                            out.push(row);
                        }
                    }
                }
            }
        } else if let Some(rich) = element.get("richItemRenderer") {
            if let Some(video) = rich.get("content").and_then(|c| c.get("videoRenderer")) {
                if let Some(row) = row_from_renderer(video) {
                    out.push(row);
                }
            }
        }
    }
}

/// Continuation token in a section list, if any.
fn continuation_token(contents: &[serde_json::Value]) -> Option<String> {
    for element in contents {
        if let Some(tok) = element
            .get("continuationItemRenderer")
            .and_then(|c| c.get("continuationEndpoint"))
            .and_then(|c| c.get("continuationCommand"))
            .and_then(|c| c.get("token"))
            .and_then(|t| t.as_str())
        {
            return Some(tok.to_string());
        }
    }
    None
}

async fn search_page(
    client: &reqwest::Client,
    query: &str,
    continuation: Option<&str>,
) -> Result<serde_json::Value> {
    let mut body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "WEB",
                "clientVersion": SEARCH_CLIENT_VERSION,
                "newVisitorCookie": true
            },
            "user": { "lockedSafetyMode": false }
        },
        "query": query,
        "client": { "hl": "en", "gl": "US" }
    });
    if let Some(tok) = continuation {
        body["continuation"] = serde_json::Value::String(tok.to_string());
    }
    let resp = client
        .post(format!(
            "https://www.youtube.com/youtubei/v1/search?key={SEARCH_KEY}"
        ))
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/70.0.3538.77 Safari/537.36")
        .json(&body)
        .send()
        .await?;
    let resp = resp.error_for_status()?;
    let json: serde_json::Value = resp.json().await?;
    Ok(json)
}

/// Search YouTube (port of `VideosSearch(query, limit=…)`), returning
/// up to `limit` video rows. Follows continuation pages until the
/// limit is met, like `VideosSearch.next()`.
pub async fn videos_search(query: &str, limit: usize) -> Vec<SearchRow> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut rows: Vec<SearchRow> = Vec::new();
    let mut continuation: Option<String> = None;
    for _ in 0..5 {
        let page = match search_page(&client, query, continuation.as_deref()).await {
            Ok(p) => p,
            Err(e) => {
                error!("YouTube search failed for {query:?}: {e}");
                return rows;
            }
        };
        if continuation.is_none() {
            // First page: contents.twoColumnSearchResultsRenderer
            // .primaryContents.sectionListRenderer.contents (fallback:
            // richGridRenderer.contents).
            let primary = page
                .get("contents")
                .and_then(|c| c.get("twoColumnSearchResultsRenderer"))
                .and_then(|c| c.get("primaryContents"));
            if let Some(contents) = primary
                .and_then(|p| p.get("sectionListRenderer"))
                .and_then(|s| s.get("contents"))
                .and_then(|c| c.as_array())
            {
                for element in contents {
                    if let Some(items) = element
                        .get("itemSectionRenderer")
                        .and_then(|i| i.get("contents"))
                        .and_then(|c| c.as_array())
                    {
                        rows_from_contents(items, &mut rows);
                    }
                }
                continuation = continuation_token(contents);
            } else if let Some(contents) = primary
                .and_then(|p| p.get("richGridRenderer"))
                .and_then(|r| r.get("contents"))
                .and_then(|c| c.as_array())
            {
                rows_from_contents(contents, &mut rows);
            }
        } else if let Some(items) = page
            .get("onResponseReceivedCommands")
            .and_then(|c| c.as_array())
            .and_then(|cmds| cmds.first())
            .and_then(|c| c.get("appendContinuationItemsAction"))
            .and_then(|a| a.get("continuationItems"))
            .and_then(|c| c.as_array())
        {
            rows_from_contents(items, &mut rows);
            continuation = continuation_token(items);
        } else {
            break;
        }
        if rows.len() >= limit || continuation.is_none() {
            break;
        }
    }
    rows.truncate(limit);
    rows
}

// ── legacy YouTube helper ────────────────────────────────────────────────

/// `details()` return: title, duration text, duration seconds,
/// thumbnail, video id.
#[derive(Debug, Clone)]
pub struct VideoDetails {
    pub title: String,
    pub duration_min: String,
    pub duration_sec: u64,
    pub thumbnail: String,
    pub vidid: String,
}

/// `track()` return: title, link, video id, duration text, thumbnail.
#[derive(Debug, Clone)]
pub struct TrackDetails {
    pub title: String,
    pub link: String,
    pub vidid: String,
    pub duration_min: String,
    pub thumb: String,
}

/// One entry of `formats()`.
#[derive(Debug, Clone, Deserialize)]
pub struct FormatEntry {
    pub format: String,
    pub filesize: Option<i64>,
    pub format_id: String,
    pub ext: String,
    pub format_note: Option<String>,
    #[serde(default)]
    pub yturl: String,
}

/// `download()` return: Python returns a bare path for song
/// audio/video, and a `(path, direct)` tuple otherwise.
#[derive(Debug, Clone)]
pub enum YouTubeDownload {
    File(String),
    Stream { file: String, direct: Option<bool> },
}

/// Low-level YouTube helper (port of the `YouTube` class).
pub struct YouTube;

impl Default for YouTube {
    fn default() -> Self {
        YouTube
    }
}

impl YouTube {
    pub fn new() -> Self {
        YouTube
    }

    /// Normalise a link: prepend the watch base for a bare id, then
    /// strip `&…` and `?si=…` (mirrors Python, in that order).
    fn normalise(link: &str, videoid: bool, base: &str) -> String {
        let mut link = if videoid {
            format!("{base}{link}")
        } else {
            link.to_string()
        };
        if link.contains('&') {
            link = link.split('&').next().unwrap_or("").to_string();
        }
        if link.contains("?si=") {
            link = link.split("?si=").next().unwrap_or("").to_string();
        }
        link
    }

    pub async fn exists(&self, link: &str, videoid: bool) -> bool {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        any_yt_re().is_match(&link)
    }

    pub async fn details(&self, link: &str, videoid: bool) -> Option<VideoDetails> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        let row = videos_search(&link, 1).await.into_iter().next()?;
        let duration_min = row.duration.clone().unwrap_or_else(|| "None".to_string());
        let duration_sec = if duration_min == "None" {
            0
        } else {
            duration_to_seconds(Some(&duration_min))
        };
        Some(VideoDetails {
            title: row.title,
            duration_min,
            duration_sec,
            thumbnail: row
                .thumbnails
                .first()
                .cloned()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or("")
                .to_string(),
            vidid: row.id,
        })
    }

    pub async fn title(&self, link: &str, videoid: bool) -> Option<String> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        videos_search(&link, 1)
            .await
            .into_iter()
            .next()
            .map(|r| r.title)
    }

    pub async fn duration(&self, link: &str, videoid: bool) -> Option<String> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        videos_search(&link, 1)
            .await
            .into_iter()
            .next()
            .and_then(|r| r.duration)
    }

    pub async fn thumbnail(&self, link: &str, videoid: bool) -> Option<String> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        videos_search(&link, 1).await.into_iter().next().map(|r| {
            r.thumbnails
                .first()
                .cloned()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or("")
                .to_string()
        })
    }

    /// `yt-dlp -g -f best[height<=?720][width<=?1280]`: `(1, url)` on
    /// stdout, `(0, stderr)` otherwise.
    pub async fn video(&self, link: &str, videoid: bool) -> Result<(u8, String)> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        let output = run_ytdlp(&["-g", "-f", "best[height<=?720][width<=?1280]", &link]).await?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !stdout.trim().is_empty() {
            let url = stdout.lines().next().unwrap_or("").to_string();
            Ok((1, url))
        } else {
            Ok((0, String::from_utf8_lossy(&output.stderr).into_owned()))
        }
    }

    pub async fn playlist(
        &self,
        link: &str,
        limit: usize,
        _user_id: i64,
        videoid: bool,
    ) -> Result<Vec<String>> {
        let link = Self::normalise(link, videoid, LIST_BASE);
        let out = shell_cmd(&format!(
            "yt-dlp -i --get-id --flat-playlist --playlist-end {limit} --skip-download {link}"
        ))
        .await;
        Ok(out
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect())
    }

    pub async fn track(&self, link: &str, videoid: bool) -> Option<(TrackDetails, String)> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        let row = videos_search(&link, 1).await.into_iter().next()?;
        let details = TrackDetails {
            title: row.title.clone(),
            link: row.link.clone(),
            vidid: row.id.clone(),
            duration_min: row.duration.clone().unwrap_or_default(),
            thumb: row
                .thumbnails
                .first()
                .cloned()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or("")
                .to_string(),
        };
        let vidid = row.id;
        Some((details, vidid))
    }

    /// `yt-dlp -J`: formats without `dash` in the format string,
    /// requiring format/filesize/format_id/ext/format_note keys.
    pub async fn formats(&self, link: &str, videoid: bool) -> Result<(Vec<FormatEntry>, String)> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        let info = ytdlp_json(&["--no-playlist", "-q", "--no-warnings", &link]).await?;
        let mut out = Vec::new();
        if let Some(formats) = info.get("formats").and_then(|f| f.as_array()) {
            for format in formats {
                let fmt_str = match format.get("format").and_then(|f| f.as_str()) {
                    Some(s) => s,
                    None => continue,
                };
                if fmt_str.to_lowercase().contains("dash") {
                    continue;
                }
                // Python accesses all five keys and skips on KeyError.
                if !(format.get("format").is_some()
                    && format.get("filesize").is_some()
                    && format.get("format_id").is_some()
                    && format.get("ext").is_some()
                    && format.get("format_note").is_some())
                {
                    continue;
                }
                let filesize = format.get("filesize").and_then(|f| f.as_i64());
                let format_id = format
                    .get("format_id")
                    .and_then(|f| f.as_str())
                    .unwrap_or_default()
                    .to_string();
                let ext = format
                    .get("ext")
                    .and_then(|f| f.as_str())
                    .unwrap_or_default()
                    .to_string();
                let format_note = format
                    .get("format_note")
                    .and_then(|f| f.as_str())
                    .map(|s| s.to_string());
                out.push(FormatEntry {
                    format: fmt_str.to_string(),
                    filesize,
                    format_id,
                    ext,
                    format_note,
                    yturl: link.clone(),
                });
            }
        }
        Ok((out, link))
    }

    pub async fn slider(
        &self,
        link: &str,
        query_type: usize,
        videoid: bool,
    ) -> Option<(String, String, String, String)> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        let rows = videos_search(&link, 10).await;
        let row = rows.get(query_type)?;
        Some((
            row.title.clone(),
            row.duration.clone().unwrap_or_default(),
            row.thumbnails
                .first()
                .cloned()
                .unwrap_or_default()
                .split('?')
                .next()
                .unwrap_or("")
                .to_string(),
            row.id.clone(),
        ))
    }

    /// Download via `yt-dlp` (library opts → CLI flags). Returns the
    /// file path (song audio/video) or the `(file, direct)` stream
    /// result; `None` when the `video` branch yields no URL.
    pub async fn download(
        &self,
        link: &str,
        video: bool,
        videoid: bool,
        songaudio: bool,
        songvideo: bool,
        format_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<Option<YouTubeDownload>> {
        let link = Self::normalise(link, videoid, WATCH_BASE);
        if songvideo {
            let format = format!("{}+140", format_id.unwrap_or_default());
            let fpath = format!("{}/{}", LEGACY_DOWNLOADS_DIR, title.unwrap_or_default());
            run_ytdlp(&[
                "-f",
                &format,
                "-o",
                &fpath,
                "--geo-bypass",
                "--no-check-certificates",
                "-q",
                "--no-warnings",
                "--prefer-ffmpeg",
                "--merge-output-format",
                "mp4",
                "--no-playlist",
                &link,
            ])
            .await?;
            return Ok(Some(YouTubeDownload::File(format!(
                "{}/{}.mp4",
                LEGACY_DOWNLOADS_DIR,
                title.unwrap_or_default()
            ))));
        }
        if songaudio {
            let fpath = format!(
                "{}/{}.%(ext)s",
                LEGACY_DOWNLOADS_DIR,
                title.unwrap_or_default()
            );
            run_ytdlp(&[
                "-f",
                format_id.unwrap_or_default(),
                "-o",
                &fpath,
                "--geo-bypass",
                "--no-check-certificates",
                "-q",
                "--no-warnings",
                "--prefer-ffmpeg",
                "--extract-audio",
                "--audio-format",
                "mp3",
                "--audio-quality",
                "192K",
                "--no-playlist",
                &link,
            ])
            .await?;
            return Ok(Some(YouTubeDownload::File(format!(
                "{}/{}.mp3",
                LEGACY_DOWNLOADS_DIR,
                title.unwrap_or_default()
            ))));
        }
        if video {
            let output =
                run_ytdlp(&["-g", "-f", "best[height<=?720][width<=?1280]", &link]).await?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if !stdout.trim().is_empty() {
                let file = stdout.lines().next().unwrap_or("").to_string();
                return Ok(Some(YouTubeDownload::Stream { file, direct: None }));
            }
            return Ok(None);
        }
        // Audio download: extract info for id/ext, reuse an existing
        // file, else download bestaudio.
        let info = ytdlp_json(&[
            "--no-playlist",
            "--geo-bypass",
            "--no-check-certificates",
            "-q",
            "--no-warnings",
            &link,
        ])
        .await?;
        let id = info.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        let ext = info.get("ext").and_then(|e| e.as_str()).unwrap_or_default();
        let xyz = format!("{LEGACY_DOWNLOADS_DIR}/{id}.{ext}");
        if Path::new(&xyz).exists() {
            return Ok(Some(YouTubeDownload::Stream {
                file: xyz,
                direct: Some(true),
            }));
        }
        run_ytdlp(&[
            "-f",
            "bestaudio/best",
            "-o",
            &format!("{LEGACY_DOWNLOADS_DIR}/%(id)s.%(ext)s"),
            "--geo-bypass",
            "--no-check-certificates",
            "-q",
            "--no-warnings",
            "--no-playlist",
            &link,
        ])
        .await?;
        Ok(Some(YouTubeDownload::Stream {
            file: xyz,
            direct: Some(true),
        }))
    }
}

// ── YouTubeData (MusicService provider) ──────────────────────────────────

/// Last yt-dlp extraction failure, kept so callers can classify it
/// instead of seeing a bare `None` (mirrors the class attribute
/// `YouTubeData.last_extract_error`).
static LAST_EXTRACT_ERROR: Mutex<String> = Mutex::new(String::new());

pub fn last_extract_error() -> String {
    LAST_EXTRACT_ERROR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn set_last_extract_error(value: &str) {
    if let Ok(mut slot) = LAST_EXTRACT_ERROR.lock() {
        *slot = value.to_string();
    }
}

/// YouTube music service implementing the `MusicService` interface.
pub struct YouTubeData {
    pub query: String,
}

impl YouTubeData {
    pub fn new(query: Option<&str>) -> Self {
        YouTubeData {
            query: query.unwrap_or_default().trim().to_string(),
        }
    }

    fn watch_url(video_id: &str) -> String {
        format!("{WATCH_BASE}{video_id}")
    }

    /// Extract a video id from a URL or a bare id.
    pub fn extract_id(query: &str) -> Option<String> {
        if let Some(c) = yt_url_re().captures(query) {
            return Some(c[1].to_string());
        }
        if yt_id_re().is_match(query) {
            return Some(query.to_string());
        }
        None
    }

    fn row_to_track(row: &SearchRow) -> MusicTrack {
        let cover = row
            .thumbnails
            .first()
            .cloned()
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or("")
            .to_string();
        let artist = if row.channel_name.is_empty() {
            "YouTube".to_string()
        } else {
            row.channel_name.clone()
        };
        MusicTrack {
            info: TrackInfo {
                url: row.link.clone(),
                title: row.title.clone(),
                artist,
                id: row.id.clone(),
                year: 0,
                thumbnail: Some(cover),
                duration_secs: duration_to_seconds(row.duration.as_deref()),
                platform: "youtube".to_string(),
                ..Default::default()
            },
            stream_url: row.link.clone(),
            file_path: None,
        }
    }

    async fn search_rows(query: &str, limit: usize) -> Vec<SearchRow> {
        videos_search(query, limit).await
    }

    /// `yt-dlp -J` metadata extraction (cookieless). On failure the
    /// error is recorded in `last_extract_error` and `None` returned.
    async fn ydl_extract_async(url: &str) -> Option<serde_json::Value> {
        match ytdlp_json(&[
            "--no-playlist",
            "--no-check-certificates",
            "--geo-bypass",
            "-q",
            "--no-warnings",
            "--socket-timeout",
            "20",
            url,
        ])
        .await
        {
            Ok(info) => {
                set_last_extract_error("");
                Some(info)
            }
            Err(e) => {
                error!("yt-dlp metadata extraction failed for {url:?}: {e}");
                set_last_extract_error(&format!("{e}"));
                None
            }
        }
    }

    fn platform_tracks(tracks: Vec<MusicTrack>, query: &str) -> Option<PlatformTracks> {
        if tracks.is_empty() {
            return None;
        }
        let platform = tracks
            .first()
            .map(|t| t.info.platform.clone())
            .unwrap_or_default();
        Some(PlatformTracks {
            platform,
            query: query.to_string(),
            tracks: tracks.into_iter().map(|t| t.info).collect(),
        })
    }
}

#[async_trait]
impl MusicService for YouTubeData {
    fn service_name(&self) -> &'static str {
        "youtube"
    }

    fn is_valid(&self, url: &str) -> bool {
        !url.is_empty() && yt_url_re().is_match(url)
    }

    async fn get_info(&self) -> Option<PlatformTracks> {
        if self.query.is_empty() || !self.is_valid(&self.query) {
            return None;
        }
        let rows = Self::search_rows(&self.query, 1).await;
        let Some(row) = rows.into_iter().next() else {
            return None;
        };
        Self::platform_tracks(vec![Self::row_to_track(&row)], &self.query)
    }

    async fn search(&self) -> Option<PlatformTracks> {
        if self.query.is_empty() {
            return None;
        }
        if self.is_valid(&self.query) {
            return self.get_info().await;
        }
        let rows = Self::search_rows(&self.query, 10).await;
        let tracks: Vec<MusicTrack> = rows
            .iter()
            .filter(|r| !r.id.is_empty())
            .map(Self::row_to_track)
            .collect();
        Self::platform_tracks(tracks, &self.query)
    }

    async fn get_recommendations(&self) -> Option<PlatformTracks> {
        // YouTube has no recommendations endpoint here; API-backed
        // services (ApiData) own recommendation duty. None by design.
        None
    }

    async fn get_track(&self) -> Option<TrackInfo> {
        if self.query.is_empty() {
            return None;
        }
        let video_id = match Self::extract_id(&self.query) {
            Some(id) => id,
            None => {
                let rows = Self::search_rows(&self.query, 1).await;
                let row = rows.into_iter().next()?;
                if row.id.is_empty() {
                    return None;
                }
                row.id
            }
        };
        let info = Self::ydl_extract_async(&Self::watch_url(&video_id)).await?;
        let id = info
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or(&video_id)
            .to_string();
        Some(TrackInfo {
            url: Self::watch_url(&id),
            cdnurl: String::new(),
            key: String::new(),
            title: info
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            artist: info
                .get("uploader")
                .and_then(|u| u.as_str())
                .or_else(|| info.get("channel").and_then(|c| c.as_str()))
                .unwrap_or("YouTube")
                .to_string(),
            album: String::new(),
            id: id.clone(),
            thumbnail: info
                .get("thumbnail")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string()),
            lyrics: String::new(),
            duration_secs: info.get("duration").and_then(|d| d.as_u64()).unwrap_or(0),
            year: 0,
            platform: "youtube".to_string(),
            ..Default::default()
        })
    }

    async fn download_track(&self, track: &TrackInfo, video: bool) -> Option<PathBuf> {
        if track.id.is_empty() {
            return None;
        }
        let dir = Path::new(DOWNLOADS_DIR);
        if tokio::fs::create_dir_all(dir).await.is_err() {
            return None;
        }
        let url = if track.url.is_empty() {
            Self::watch_url(&track.id)
        } else {
            track.url.clone()
        };
        if let Some(existing) = glob_prefixed(dir, &track.id).into_iter().next() {
            return Some(existing);
        }
        let format = if video {
            "(bestvideo[height<=?720][ext=mp4]+bestaudio[ext=m4a])/best"
        } else {
            "bestaudio/best"
        };
        let outtmpl = dir.join(format!("{}.%(ext)s", track.id));
        let mut args: Vec<&str> = vec![
            "-f",
            format,
            "-o",
            outtmpl.to_str().unwrap_or(""),
            "-q",
            "--no-warnings",
            "--no-check-certificates",
            "--geo-bypass",
            "--no-playlist",
            "--socket-timeout",
            "30",
        ];
        if video {
            args.push("--merge-output-format");
            args.push("mp4");
        }
        args.push(&url);
        if let Err(e) = run_ytdlp(&args).await {
            error!("YouTube download failed for {}: {e}", track.id);
            return None;
        }
        glob_prefixed(dir, &track.id).into_iter().next()
    }
}

/// Files in `dir` whose name starts with `{prefix}.`, sorted.
fn glob_prefixed(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let needle = format!("{prefix}.");
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&needle) {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_to_seconds_parses() {
        assert_eq!(time_to_seconds("3:24").unwrap(), 204);
        assert_eq!(time_to_seconds("1:02:03").unwrap(), 3723);
        assert_eq!(time_to_seconds("45").unwrap(), 45);
        assert!(time_to_seconds("None").is_err());
        assert!(time_to_seconds("").is_err());
    }

    #[test]
    fn duration_to_seconds_zero_on_bad() {
        assert_eq!(duration_to_seconds(None), 0);
        assert_eq!(duration_to_seconds(Some("3:24")), 204);
        assert_eq!(duration_to_seconds(Some("None")), 0);
    }

    #[test]
    fn extract_id_from_url_and_bare() {
        assert_eq!(
            YouTubeData::extract_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            YouTubeData::extract_id("https://youtu.be/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            YouTubeData::extract_id("dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(YouTubeData::extract_id("never gonna give you up"), None);
    }

    #[test]
    fn row_to_track_maps_fields() {
        let row = SearchRow {
            id: "dQw4w9WgXcQ".to_string(),
            title: "Never Gonna Give You Up".to_string(),
            duration: Some("3:24".to_string()),
            thumbnails: vec!["https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg?sqp=x".to_string()],
            channel_name: "Rick Astley".to_string(),
            link: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
        };
        let track = YouTubeData::row_to_track(&row);
        assert_eq!(track.info.id, "dQw4w9WgXcQ");
        assert_eq!(track.info.title, "Never Gonna Give You Up");
        assert_eq!(track.info.artist, "Rick Astley");
        assert_eq!(track.info.duration_secs, 204);
        assert_eq!(
            track.info.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
        );
        assert_eq!(track.info.platform, "youtube");
    }

    #[test]
    fn row_to_track_empty_channel_becomes_youtube() {
        let row = SearchRow {
            id: "abc".to_string(),
            channel_name: String::new(),
            ..Default::default()
        };
        assert_eq!(YouTubeData::row_to_track(&row).info.artist, "YouTube");
    }

    #[test]
    fn is_valid_matches_python_regex() {
        let svc = YouTubeData::new(None);
        assert!(svc.is_valid("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(svc.is_valid("https://youtu.be/dQw4w9WgXcQ"));
        assert!(svc.is_valid("https://www.youtube.com/shorts/dQw4w9WgXcQ"));
        assert!(!svc.is_valid("https://www.youtube.com/playlist?list=PL123"));
        assert!(!svc.is_valid(""));
    }
}
