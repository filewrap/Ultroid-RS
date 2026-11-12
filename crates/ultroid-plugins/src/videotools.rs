//! Video tools — port of `plugins/videotools.py` (1,315 LOC).
//!
//! `.sample` / `.vshots` / `.vtrim` / `.vmerge` / `.vtext` / `.vbg` /
//! `.vconvert` / `.vspeed` / `.vreverse` / `.vextract` / `.vwatermark` /
//! `.vrotate` / `.vresize` / `.vformat` / `.vconcat`.
//!
//! All ffmpeg/ffprobe work is real subprocess work via
//! `tokio::process::Command` (Python runs the same binaries through
//! `fns.bash` / `subprocess.run`): the exact ffmpeg command lines from
//! the Python source are reproduced, and metadata comes from a real
//! `ffprobe -print_format json` parse. A missing `ffmpeg`/`ffprobe`
//! binary surfaces as an honest error naming the binary.
//!
//! The one gate is the Telegram side: downloading the replied video
//! (`_download_video`) now goes through the Responder (`download_media`),
//! while listing the account's recent own videos for `.vmerge` /
//! `.vconcat` still has no Responder op and fails honestly at that point
//! (`TODO(responder-op)`); the ffmpeg + upload code after it is real and
//! runs unchanged. Uploads go through
//! `Ctx::send_file` (the `fast_uploader` progress/attributes/thumb
//! options have no `Ctx` equivalent and are not faked).
//!
//! The assistant-bot `/sample` … `/vconcat` stubs ("Use on user
//! account") are not registered: the registry already serves both `.`
//! and `/` prefixes, so they would collide with the real commands.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;
use tokio::process::Command;
use ultroid_core::fns::humanbytes;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

const HELP: &str = "✘ Commands Available\n\n\
• `.sample <duration>` — short sample of a video (reply to video).\n\
• `.vshots <count>` — screenshots of a video (reply to video).\n\
• `.vtrim <start>-<end>` — trim a video (reply to video).\n\
• `.vmerge` — merge videos (reply to multiple videos).\n\
• `.vtext <text>` — text overlay on a video (reply to video).\n\
• `.vbg <color>` — change background color of a video (reply to video).\n\
• `.vconvert <format>` — convert video to another format (reply to video).\n\
• `.vspeed <speed>` — change playback speed (reply to video).\n\
• `.vreverse` — reverse a video (reply to video).\n\
• `.vextract` — extract audio from a video (reply to video).\n\
• `.vwatermark <text>` — add watermark to a video (reply to video).\n\
• `.vrotate <degrees>` — rotate a video (reply to video).\n\
• `.vresize <width>x<height>` — resize a video (reply to video).\n\
• `.vformat` — detailed format info of a video (reply to video).\n\
• `.vconcat` — concatenate multiple videos (reply to videos).";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("sample", "sample( (.*)|$)", HELP), |ctx| {
        Box::pin(sample_cmd(ctx))
    });
    reg_add(registry, def("vshots", "vshots( (.*)|$)", HELP), |ctx| {
        Box::pin(vshots_cmd(ctx))
    });
    reg_add(registry, def("vtrim", "vtrim( (.*)|$)", HELP), |ctx| {
        Box::pin(vtrim_cmd(ctx))
    });
    reg_add(registry, def("vmerge", "vmerge$", HELP), |ctx| {
        Box::pin(vmerge_cmd(ctx))
    });
    reg_add(registry, def("vtext", r"vtext(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(vtext_cmd(ctx))
    });
    reg_add(registry, def("vbg", r"vbg(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(vbg_cmd(ctx))
    });
    reg_add(
        registry,
        def("vconvert", r"vconvert(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(vconvert_cmd(ctx)),
    );
    reg_add(
        registry,
        def("vspeed", r"vspeed(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(vspeed_cmd(ctx)),
    );
    reg_add(registry, def("vreverse", "vreverse$", HELP), |ctx| {
        Box::pin(vreverse_cmd(ctx))
    });
    reg_add(registry, def("vextract", "vextract$", HELP), |ctx| {
        Box::pin(vextract_cmd(ctx))
    });
    reg_add(
        registry,
        def("vwatermark", r"vwatermark(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(vwatermark_cmd(ctx)),
    );
    reg_add(
        registry,
        def("vrotate", r"vrotate(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(vrotate_cmd(ctx)),
    );
    reg_add(
        registry,
        def("vresize", r"vresize(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(vresize_cmd(ctx)),
    );
    reg_add(registry, def("vformat", "vformat$", HELP), |ctx| {
        Box::pin(vformat_cmd(ctx))
    });
    reg_add(registry, def("vconcat", "vconcat$", HELP), |ctx| {
        Box::pin(vconcat_cmd(ctx))
    });
}

// ── fmtmsg (port of `custom/fmt.py:fmtmsg` for a dict) ───────────────────

fn fmtmsg(head: &str, rows: &[(&str, String)]) -> String {
    let padd = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = format!("<b>{head}</b>");
    for (k, v) in rows {
        out.push_str(&format!("\n  <code>{:<padd$}</code> : {v}", k, padd = padd));
    }
    out
}

// ── subprocess helpers (port of `fns.bash` + `_run_ffmpeg`) ───────────────

/// Run a shell command string (Python `fns.bash` runs `bash -c <cmd>`).
/// Returns `(stdout, stderr, success)`.
async fn run_bash(cmd: &str) -> Result<(String, String, bool)> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    ))
}

/// Run an ffmpeg command. Python's `_run_ffmpeg` fails when `"Error"`
/// appears in stderr; a non-zero exit is also a failure here (ffmpeg
/// reports real failures both ways).
async fn run_ffmpeg(cmd: &str) -> (bool, String) {
    match run_bash(cmd).await {
        Ok((out, err, success)) => {
            if !success || err.contains("Error") {
                (false, if err.is_empty() { out } else { err })
            } else {
                (true, out)
            }
        }
        Err(e) => (false, e.to_string()),
    }
}

// ── ffprobe metadata (port of `_get_video_info`) ──────────────────────────

struct VideoInfo {
    filename: String,
    duration: f64,
    size: u64,
    format_name: String,
    video_codec: String,
    width: u64,
    height: u64,
    frame_rate: String,
    audio_codec: String,
    audio_sample_rate: String,
    audio_channels: String,
    error: Option<String>,
}

async fn video_info(path: &Path) -> VideoInfo {
    let fallback = |error: &str| VideoInfo {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        duration: 0.0,
        size: 0,
        format_name: "unknown".into(),
        video_codec: "none".into(),
        width: 0,
        height: 0,
        frame_rate: "0/0".into(),
        audio_codec: "none".into(),
        audio_sample_rate: "0".into(),
        audio_channels: "0".into(),
        error: Some(error.to_string()),
    };
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .await;
    let Ok(out) = out else {
        return fallback("Could not read metadata");
    };
    let Ok(data) = serde_json::from_slice::<Value>(&out.stdout) else {
        return fallback("Could not read metadata");
    };
    let streams = data
        .get("streams")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some("video"))
        .cloned()
        .unwrap_or(Value::Null);
    let audio = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some("audio"))
        .cloned()
        .unwrap_or(Value::Null);
    let fmt = data.get("format").cloned().unwrap_or(Value::Null);
    // Free fn (not a closure): the returned &str borrows `v`, which a
    // closure's inferred lifetimes tie to the wrong parameter.
    fn get<'a>(v: &'a Value, k: &str) -> &'a str {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("")
    }
    VideoInfo {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        duration: fmt
            .get("duration")
            .and_then(|d| d.as_str())
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0),
        size: fmt
            .get("size")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        format_name: get(&fmt, "format_name").to_string(),
        video_codec: get(&video, "codec_name").to_string(),
        width: video.get("width").and_then(|w| w.as_u64()).unwrap_or(0),
        height: video.get("height").and_then(|h| h.as_u64()).unwrap_or(0),
        frame_rate: get(&video, "r_frame_rate").to_string(),
        audio_codec: get(&audio, "codec_name").to_string(),
        audio_sample_rate: get(&audio, "sample_rate").to_string(),
        audio_channels: audio
            .get("channels")
            .and_then(|c| c.as_u64())
            .map(|c| c.to_string())
            .unwrap_or_default(),
        error: None,
    }
}

fn fmt_video_info(info: &VideoInfo) -> String {
    if let Some(err) = &info.error {
        return format!("<b>Video Info</b>\n<code>{err}</code>");
    }
    let rows = [
        ("File", info.filename.clone()),
        ("Duration", format!("{:.2}s", info.duration)),
        ("Size", humanbytes(info.size)),
        ("Format", info.format_name.clone()),
        ("Video Codec", info.video_codec.clone()),
        ("Resolution", format!("{}x{}", info.width, info.height)),
        ("Frame Rate", info.frame_rate.clone()),
        (
            "Audio Codec",
            if info.audio_codec.is_empty() || info.audio_codec == "none" {
                "N/A".to_string()
            } else {
                info.audio_codec.clone()
            },
        ),
        (
            "Audio Rate",
            if info.audio_sample_rate.is_empty() || info.audio_sample_rate == "0" {
                "N/A".to_string()
            } else {
                info.audio_sample_rate.clone()
            },
        ),
        (
            "Channels",
            if info.audio_channels.is_empty() || info.audio_channels == "0" {
                "N/A".to_string()
            } else {
                info.audio_channels.clone()
            },
        ),
    ];
    fmtmsg(
        "🎬 Video Format",
        &rows
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>(),
    )
}

/// Port of `fns.stdr` (`pyUltroid/fns/tools.py:415`): `HH:MM:SS`.
fn stdr(seconds: i64) -> String {
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn out_name(file: &Path, suffix: &str) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_string());
    file.with_file_name(format!("{stem}{suffix}"))
}

// ── Telegram gates ────────────────────────────────────────────────────────

/// Validate that the command replies to *something*, then download the
/// replied video. The media type check (`fns.mediainfo` — video / video
/// as doc / gif) needs the message itself, so it lives behind the same
/// gate. Returns `Ok(None)` when the no-reply error was already sent.
async fn need_video_reply(ctx: &Ctx, title: &str) -> Result<Option<PathBuf>> {
    if ctx.reply_to_id.is_none() {
        ctx.reply_html(&fmtmsg(title, &[("Error", "Reply to a video".to_string())]))
            .await?;
        return Ok(None);
    }
    let reply_id = ctx.reply_to_id.unwrap_or(0);
    let dest = std::env::temp_dir().join(format!("ultroid-video-{}", uuid::Uuid::new_v4()));
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    Ok(Some(path))
}

/// `.vmerge` / `.vconcat`: the account's recent own videos in this chat
/// (`iter_messages(chat, from_user="me", limit=10)` filtered by
/// `fns.mediainfo`).
async fn need_own_recent_videos(_ctx: &Ctx) -> Result<Vec<PathBuf>> {
    // TODO(responder-op): iterate own messages in this chat and download
    // the video ones (Python: videotools.py:vmerge / vconcat).
    Err(Error::Other(
        "iterate own messages + download recent videos requires a binary Responder op (Python: videotools.py:vmerge)"
            .to_string(),
    ))
}

/// Send the finished file (Python: `fast_uploader` + `send_file` with
/// caption), delete the local output and the status message.
async fn finish(ctx: &Ctx, msg_id: i64, out: &Path, caption: String) -> Result<()> {
    ctx.send_file(out, &caption).await?;
    let _ = tokio::fs::remove_file(out).await;
    ctx.delete(msg_id).await?;
    Ok(())
}

async fn fail(ctx: &Ctx, msg_id: i64, title: &str, err: &str) -> Result<()> {
    ctx.edit(msg_id, &fmtmsg(title, &[("Error", err.to_string())]))
        .await?;
    Ok(())
}

// ── commands ──────────────────────────────────────────────────────────────

async fn sample_cmd(ctx: &Ctx) -> Result<()> {
    let sec = ctx.args.trim();
    let stime: u64 = if sec.is_empty() {
        30
    } else {
        sec.parse().unwrap_or(30)
    };
    let Some(file) = need_video_reply(ctx, "Sample").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_sample.mkv");
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Sample",
            &[("Status", format!("Generating {stime}s sample..."))],
        ),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -preset ultrafast -ss 0 -t {stime} -c copy -map 0 \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Sample", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Sample",
            &[
                ("Status", format!("Sample of {stime}s created")),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vshots_cmd(ctx: &Ctx) -> Result<()> {
    let ss = ctx.args.trim();
    let shot: u32 = if ss.is_empty() {
        5
    } else {
        ss.parse().unwrap_or(5)
    };
    let Some(file) = need_video_reply(ctx, "Screenshots").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Screenshots",
            &[("Status", format!("Generating {shot} screenshots..."))],
        ),
    )
    .await?;
    let work = PathBuf::from("ss");
    let _ = tokio::fs::remove_dir_all(&work).await;
    tokio::fs::create_dir_all(&work).await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf fps=0.009 -vframes {shot} \"ss/pic%01d.png\"",
        file.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        let _ = tokio::fs::remove_dir_all(&work).await;
        return fail(ctx, msg_id, "Screenshots", &result).await;
    }
    let mut pics: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&work) {
        for e in rd.flatten() {
            pics.push(e.path());
        }
    }
    pics.sort();
    let text = if pics.is_empty() {
        "❌ Failed to take screenshots".to_string()
    } else {
        format!("✅ Uploaded {}/{} screenshots", pics.len(), shot)
    };
    if pics.is_empty() {
        ctx.edit(msg_id, &fmtmsg("Screenshots", &[("Status", text)]))
            .await?;
    } else {
        // Python: `e.respond(text, file=pics)` — one message, text + files.
        // `Ctx` sends one file per call, so send each shot and then the
        // summary text.
        for pic in &pics {
            let _ = ctx.send_file(pic, "").await;
        }
        ctx.reply(&text).await?;
    }
    let _ = tokio::fs::remove_dir_all(&work).await;
    ctx.delete(msg_id).await?;
    Ok(())
}

async fn vtrim_cmd(ctx: &Ctx) -> Result<()> {
    let sec = ctx.args.trim();
    let Some((a, b)) = sec.split_once('-') else {
        ctx.reply_html(&fmtmsg(
            "Trim",
            &[("Usage", ".vtrim <start>-<end>".to_string())],
        ))
        .await?;
        return Ok(());
    };
    let (a, b): (i64, i64) = match (a.trim().parse(), b.trim().parse()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            ctx.reply_html(&fmtmsg(
                "Trim",
                &[("Error", "Start/end must be numbers".to_string())],
            ))
            .await?;
            return Ok(());
        }
    };
    if a >= b {
        ctx.reply_html(&fmtmsg(
            "Trim",
            &[("Error", "Start must be less than end".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let Some(file) = need_video_reply(ctx, "Trim").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_trimmed.mkv");
    let dur = video_info(&file).await.duration;
    if b as f64 > dur {
        let _ = tokio::fs::remove_file(&file).await;
        return fail(ctx, msg_id, "Trim", "End time exceeds video duration").await;
    }
    let (ss, dd) = (stdr(a), stdr(b));
    ctx.edit(
        msg_id,
        &fmtmsg("Trim", &[("Status", format!("Trimming {ss} → {dd}..."))]),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -preset ultrafast -ss {ss} -to {dd} -c copy -map 0 \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Trim", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Trim",
            &[
                ("Status", "Trimmed".to_string()),
                ("Range", format!("{ss} → {dd}")),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn merge_like(
    ctx: &Ctx,
    title: &str,
    list_file: &str,
    out_name_s: &str,
    status_key: &str,
) -> Result<()> {
    let files = need_own_recent_videos(ctx).await?;
    if files.is_empty() {
        ctx.reply_html(&fmtmsg(
            title,
            &[("Error", "Reply to multiple videos to merge".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let msg_id = ctx
        .reply(&fmtmsg(
            title,
            &[("Status", "Downloading videos...".to_string())],
        ))
        .await?;
    ctx.edit(
        msg_id,
        &fmtmsg(
            title,
            &[("Status", format!("Merging {} videos...", files.len()))],
        ),
    )
    .await?;
    let mut list = String::new();
    for f in &files {
        list.push_str(&format!("file '{}'\n", f.display()));
    }
    tokio::fs::write(list_file, list).await?;
    let out = PathBuf::from(out_name_s);
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -f concat -safe 0 -i {list_file} -c copy \"{out_name_s}\" -y"
    ))
    .await;
    for f in &files {
        let _ = tokio::fs::remove_file(f).await;
    }
    let _ = tokio::fs::remove_file(list_file).await;
    if !ok {
        return fail(ctx, msg_id, title, &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            title,
            &[
                ("Status", status_key.to_string()),
                ("Files", format!("{} videos", files.len())),
                ("File", format!("`{out_name_s}`")),
            ],
        ),
    )
    .await
}

async fn vmerge_cmd(ctx: &Ctx) -> Result<()> {
    merge_like(ctx, "Merge", "merge_list.txt", "merged.mkv", "Merged").await
}

async fn vconcat_cmd(ctx: &Ctx) -> Result<()> {
    merge_like(
        ctx,
        "Concat",
        "concat_list.txt",
        "concatenated.mkv",
        "Concatenated",
    )
    .await
}

async fn vtext_cmd(ctx: &Ctx) -> Result<()> {
    let text = ctx.args.trim().to_string();
    if text.is_empty() {
        ctx.reply_html(&fmtmsg(
            "Text Overlay",
            &[("Usage", ".vtext <text>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let Some(file) = need_video_reply(ctx, "Text Overlay").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_text.mkv");
    let safe_text = text.replace('\'', "'\\''");
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Text Overlay",
            &[("Status", format!("Adding text: '{text}'..."))],
        ),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"drawtext=text='{safe_text}':fontcolor=white:fontsize=24:x=(w-text_w)/2:y=(h-text_h)/2\" -c:a copy \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Text Overlay", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Text Overlay",
            &[
                ("Status", "Text added".to_string()),
                ("Text", format!("`{text}`")),
            ],
        ),
    )
    .await
}

async fn vbg_cmd(ctx: &Ctx) -> Result<()> {
    let color = if ctx.args.trim().is_empty() {
        "black".to_string()
    } else {
        ctx.args.trim().to_string()
    };
    let Some(file) = need_video_reply(ctx, "Background").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_bg.mkv");
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Background",
            &[("Status", format!("Changing background to {color}..."))],
        ),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"color={color}:s=1920x1080[bg];[0:v][bg]overlay=(W-w)/2:(H-h)/2\" -c:a copy \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Background", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Background",
            &[
                ("Status", "Background changed".to_string()),
                ("Color", format!("`{color}`")),
            ],
        ),
    )
    .await
}

async fn vconvert_cmd(ctx: &Ctx) -> Result<()> {
    let fmt = ctx.args.trim().to_string();
    if fmt.is_empty() {
        ctx.reply_html(&fmtmsg(
            "Convert",
            &[("Usage", ".vconvert <mp4|mkv|avi|mov|webm>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let Some(file) = need_video_reply(ctx, "Convert").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, &format!(".{fmt}"));
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Convert",
            &[("Status", format!("Converting to {}...", fmt.to_uppercase()))],
        ),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -c:v libx264 -preset fast -c:a aac \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Convert", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Convert",
            &[
                ("Status", format!("Converted to {}", fmt.to_uppercase())),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vspeed_cmd(ctx: &Ctx) -> Result<()> {
    let speed = ctx.args.trim();
    let speed_val: f64 = match speed.parse() {
        Ok(v) => v,
        Err(_) => {
            ctx.reply_html(&fmtmsg(
                "Speed",
                &[("Error", "Speed must be a number".to_string())],
            ))
            .await?;
            return Ok(());
        }
    };
    if speed_val <= 0.0 {
        ctx.reply_html(&fmtmsg(
            "Speed",
            &[("Error", "Speed must be positive".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let Some(file) = need_video_reply(ctx, "Speed").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, &format!("_speed{speed}.mkv"));
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Speed",
            &[("Status", format!("Changing speed to {speed_val}x..."))],
        ),
    )
    .await?;
    // Python: 1.0 → `-c copy`; <1.0 → `setpts={speed}*PTS` + atempo;
    // >1.0 → `setpts={1/speed}*PTS` + atempo (with `-c:a copy`).
    let cmd = if speed_val == 1.0 {
        format!(
            "ffmpeg -i \"{}\" -c copy \"{}\" -y",
            file.display(),
            out.display()
        )
    } else if speed_val < 1.0 {
        format!(
            "ffmpeg -i \"{}\" -filter:v \"setpts={speed_val}*PTS\" -filter:a \"atempo={speed_val}\" -c:a copy \"{}\" -y",
            file.display(),
            out.display()
        )
    } else {
        format!(
            "ffmpeg -i \"{}\" -filter:v \"setpts={}*PTS\" -filter:a \"atempo={speed_val}\" -c:a copy \"{}\" -y",
            file.display(),
            1.0 / speed_val,
            out.display()
        )
    };
    let (ok, result) = run_ffmpeg(&cmd).await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Speed", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Speed",
            &[
                ("Status", format!("Speed changed to {speed_val}x")),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vreverse_cmd(ctx: &Ctx) -> Result<()> {
    let Some(file) = need_video_reply(ctx, "Reverse").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_reverse.mkv");
    ctx.edit(
        msg_id,
        &fmtmsg("Reverse", &[("Status", "Reversing video...".to_string())]),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"reverse\" -af \"areverse\" -c:v libx264 -preset fast -c:a aac \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Reverse", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Reverse",
            &[
                ("Status", "Video reversed".to_string()),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vextract_cmd(ctx: &Ctx) -> Result<()> {
    let Some(file) = need_video_reply(ctx, "Extract Audio").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, ".mp3");
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Extract Audio",
            &[("Status", "Extracting audio...".to_string())],
        ),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vn -acodec libmp3lame -q:a 2 \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Extract Audio", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Extract Audio",
            &[
                ("Status", "Audio extracted".to_string()),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vwatermark_cmd(ctx: &Ctx) -> Result<()> {
    let text = if ctx.args.trim().is_empty() {
        "Ultroid".to_string()
    } else {
        ctx.args.trim().to_string()
    };
    let Some(file) = need_video_reply(ctx, "Watermark").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, "_watermark.mkv");
    ctx.edit(
        msg_id,
        &fmtmsg(
            "Watermark",
            &[("Status", format!("Adding watermark: '{text}'..."))],
        ),
    )
    .await?;
    let safe_text = text.replace('\'', "'\\''");
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"drawtext=text='{safe_text}':fontcolor=white:fontsize=20:x=10:y=10:enable='between(t\\\\,0\\\\,30)'\" -c:a copy \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Watermark", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Watermark",
            &[
                ("Status", "Watermark added".to_string()),
                ("Text", format!("`{text}`")),
            ],
        ),
    )
    .await
}

async fn vrotate_cmd(ctx: &Ctx) -> Result<()> {
    let deg = if ctx.args.trim().is_empty() {
        "90".to_string()
    } else {
        ctx.args.trim().to_string()
    };
    let deg_val: i64 = match deg.parse() {
        Ok(v) => v,
        Err(_) => {
            ctx.reply_html(&fmtmsg(
                "Rotate",
                &[("Error", "Degrees must be a number".to_string())],
            ))
            .await?;
            return Ok(());
        }
    };
    let Some(file) = need_video_reply(ctx, "Rotate").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, &format!("_rot{deg}.mkv"));
    ctx.edit(
        msg_id,
        &fmtmsg("Rotate", &[("Status", format!("Rotating {deg}°..."))]),
    )
    .await?;
    // Python: `transpose={deg // 90 % 4}` (floor division, then mod).
    let transpose = deg_val.div_euclid(90).rem_euclid(4);
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"transpose={transpose}\" -c:a copy \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Rotate", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Rotate",
            &[
                ("Status", format!("Rotated {deg}°")),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vresize_cmd(ctx: &Ctx) -> Result<()> {
    let dims = ctx.args.trim().to_string();
    let Some((w, h)) = dims.split_once('x') else {
        ctx.reply_html(&fmtmsg(
            "Resize",
            &[("Usage", ".vresize <width>x<height>".to_string())],
        ))
        .await?;
        return Ok(());
    };
    let (w, h): (u32, u32) = match (w.trim().parse(), h.trim().parse()) {
        (Ok(w), Ok(h)) => (w, h),
        _ => {
            ctx.reply_html(&fmtmsg(
                "Resize",
                &[("Error", "Dimensions must be numbers".to_string())],
            ))
            .await?;
            return Ok(());
        }
    };
    let Some(file) = need_video_reply(ctx, "Resize").await? else {
        return Ok(());
    };
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let out = out_name(&file, &format!("_resized{w}x{h}.mkv"));
    ctx.edit(
        msg_id,
        &fmtmsg("Resize", &[("Status", format!("Resizing to {w}x{h}..."))]),
    )
    .await?;
    let (ok, result) = run_ffmpeg(&format!(
        "ffmpeg -i \"{}\" -vf \"scale={w}:{h}\" -c:a copy \"{}\" -y",
        file.display(),
        out.display()
    ))
    .await;
    let _ = tokio::fs::remove_file(&file).await;
    if !ok {
        return fail(ctx, msg_id, "Resize", &result).await;
    }
    finish(
        ctx,
        msg_id,
        &out,
        fmtmsg(
            "Resize",
            &[
                ("Status", format!("Resized to {w}x{h}")),
                (
                    "File",
                    format!(
                        "`{}`",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ),
                ),
            ],
        ),
    )
    .await
}

async fn vformat_cmd(ctx: &Ctx) -> Result<()> {
    let Some(file) = need_video_reply(ctx, "Format").await? else {
        return Ok(());
    };
    let _msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let info = video_info(&file).await;
    let html = fmt_video_info(&info);
    ctx.reply_html(&fmtmsg("Video Format", &[("Info", html)]))
        .await?;
    let _ = tokio::fs::remove_file(&file).await;
    Ok(())
}
