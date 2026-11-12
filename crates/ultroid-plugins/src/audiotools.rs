//! Audio tools — port of `plugins/audiotools.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.makevoice` — create a voice note from audio/video (reply).
//! - `.atrim <start>-<end>` — trim audio (reply).
//! - `.extractaudio` — extract audio from video (reply).
//! - `.audioinfo` — show detailed audio information.
//! - `.audioconvert <format>` — convert audio format (mp3, wav, ogg, aac, flac).
//! - `.audiosplit <timestamp>` — split audio at timestamp.
//! - `.audioreverse` — reverse the audio.
//! - `.audiobpm` / `.audionormalize` / `.audioch` — listed in the Python
//!   docstring but have NO handler in the Python file, so none is registered
//!   here either.
//!
//! Every command works on a local file path given as an argument, and on
//! replied-to Telegram media, downloaded via the Responder
//! (`download_media`).

use std::path::{Path, PathBuf};
use tokio::process::Command as ProcCommand;
use ultroid_core::fns::{humanbytes, time_formatter};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "makevoice",
            "makevoice$",
            "Create a voice note from audio/video (reply).",
        ),
        |ctx| Box::pin(makevoice(ctx)),
    );
    reg_add(
        registry,
        def(
            "atrim",
            r"atrim(?:\s+(.*))?$",
            "Trim audio: .atrim <start>-<end> (reply).",
        ),
        |ctx| Box::pin(atrim(ctx)),
    );
    reg_add(
        registry,
        def(
            "extractaudio",
            "extractaudio$",
            "Extract audio from video (reply).",
        ),
        |ctx| Box::pin(extractaudio(ctx)),
    );
    reg_add(
        registry,
        def(
            "audioinfo",
            "audioinfo$",
            "Show detailed audio information (reply).",
        ),
        |ctx| Box::pin(audioinfo(ctx)),
    );
    reg_add(
        registry,
        def(
            "audioconvert",
            r"audioconvert(?:\s+(.*))?$",
            "Convert audio format: mp3, wav, ogg, aac, flac, opus (reply).",
        ),
        |ctx| Box::pin(audioconvert(ctx)),
    );
    reg_add(
        registry,
        def(
            "audiosplit",
            r"audiosplit(?:\s+(.*))?$",
            "Split audio at timestamp: .audiosplit <mm:ss> (reply).",
        ),
        |ctx| Box::pin(audiosplit(ctx)),
    );
    reg_add(
        registry,
        def(
            "audioreverse",
            "audioreverse$",
            "Reverse the audio (reply).",
        ),
        |ctx| Box::pin(audioreverse(ctx)),
    );
    reg_add(
        registry,
        def("audiomerge", "audiomerge$", "Merge multiple audio files."),
        |ctx| Box::pin(audiomerge(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// Resolve the input file: a local path in args, else the replied media
/// (downloaded via the Responder), else a usage error reply.
/// First whitespace token of the args that names an existing local file.
fn local_input(ctx: &Ctx) -> Option<PathBuf> {
    ctx.args
        .split_whitespace()
        .next()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

async fn resolve_input(ctx: &Ctx, _python_ref: &str) -> Result<Option<PathBuf>> {
    if let Some(p) = local_input(ctx) {
        return Ok(Some(p));
    }
    if let Some(reply_id) = ctx.reply_to_id {
        let dest = tmp_file("input");
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        return Ok(Some(path));
    }
    Ok(None)
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

async fn run_ffmpeg(args: &[&str]) -> Result<()> {
    let out = ProcCommand::new("ffmpeg")
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

/// ffprobe duration in seconds (Python `fns.genss`).
async fn probe_duration(path: &Path) -> Result<f64> {
    let out = ProcCommand::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            path.to_str().unwrap_or(""),
        ])
        .output()
        .await
        .map_err(|e| Error::Other(format!("ffprobe not found or failed to run: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(data
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0))
}

/// Seconds -> `H:MM:SS` (Python `fns.stdr`).
fn stdr(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".to_string())
}

async fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = tokio::fs::remove_file(p).await;
    }
}

// ── commands ──────────────────────────────────────────────────────────────

async fn makevoice(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "audiotools.py:makevoice").await? else {
        ctx.reply("Make Voice — Error: Reply to audio or video")
            .await?;
        return Ok(());
    };
    let msg = ctx
        .reply("Make Voice — Status: Converting to voice...")
        .await?;
    let out = tmp_file("voice.opus");
    let r = run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-map",
        "0:a",
        "-codec:a",
        "libopus",
        "-b:a",
        "100k",
        "-vbr",
        "on",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await;
    match r {
        Ok(()) => {
            ctx.send_file(&out, "voice note").await?;
            ctx.delete(msg).await?;
        }
        Err(e) => {
            ctx.edit(
                msg,
                &format!("Make Voice — Error: Failed to convert to voice ({e})"),
            )
            .await?;
            return Err(e);
        }
    }
    cleanup(&[&out]).await;
    Ok(())
}

async fn atrim(ctx: &Ctx) -> Result<()> {
    let sec = ctx.args.trim();
    if sec.is_empty() || !sec.contains('-') {
        ctx.reply("Audio Trim — Usage: .atrim <start>-<end>")
            .await?;
        return Ok(());
    }
    let (a_s, b_s) = sec.split_once('-').unwrap();
    let (a, b): (i64, i64) = match (a_s.trim().parse(), b_s.trim().parse()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            ctx.reply("Audio Trim — Error: Start/end must be numbers")
                .await?;
            return Ok(());
        }
    };
    if a >= b {
        ctx.reply("Audio Trim — Error: Start must be less than end")
            .await?;
        return Ok(());
    }
    let Some(input) = resolve_input(ctx, "audiotools.py:atrim").await? else {
        ctx.reply("Audio Trim — Error: Reply to audio/video")
            .await?;
        return Ok(());
    };
    if (b as f64) > probe_duration(&input).await? {
        ctx.reply("Audio Trim — Error: End exceeds audio duration")
            .await?;
        return Ok(());
    }
    let (ss, dd) = (stdr(a), stdr(b));
    let out = tmp_file(&format!("{}_trimmed.aac", stem_of(&input)));
    let msg = ctx
        .reply(&format!("Audio Trim — Status: Trimming {ss} → {dd}..."))
        .await?;
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-preset",
        "ultrafast",
        "-ss",
        &ss,
        "-to",
        &dd,
        "-vn",
        "-acodec",
        "copy",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, &format!("Trimmed audio {ss} → {dd}"))
        .await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn extractaudio(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "audiotools.py:extractaudio").await? else {
        ctx.reply("Extract Audio — Error: Reply to a video").await?;
        return Ok(());
    };
    let msg = ctx.reply("Extract Audio — Status: Extracting...").await?;
    let out = tmp_file(&format!("{}.aac", stem_of(&input)));
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-vn",
        "-acodec",
        "copy",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, "extracted audio").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn audioinfo(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "audiotools.py:audioinfo").await? else {
        ctx.reply("Audio Info — Error: Reply to audio").await?;
        return Ok(());
    };
    let msg = ctx.reply("Audio Info — Status: Fetching info...").await?;
    let out = ProcCommand::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            input.to_str().unwrap_or(""),
        ])
        .output()
        .await
        .map_err(|e| Error::Other(format!("ffprobe not found or failed to run: {e}")))?;
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let fmt = data.get("format").cloned().unwrap_or_default();
    let audio_stream = data
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|ss| {
            ss.iter()
                .find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some("audio"))
                .cloned()
        })
        .unwrap_or_default();
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let duration: f64 = fmt
        .get("duration")
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let size: u64 = fmt
        .get("size")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let bit_rate: u64 = fmt
        .get("bit_rate")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = format!(
        "Audio Info\n  File: {name}\n  Duration: {duration:.2}s\n  Size: {}\n  Format: {}\n  Codec: {}\n  Sample Rate: {} Hz\n  Channels: {}\n  Bit Rate: {} kbps",
        humanbytes(size),
        fmt.get("format_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
        audio_stream
            .get("codec_name")
            .and_then(|v| v.as_str())
            .unwrap_or("none"),
        audio_stream
            .get("sample_rate")
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
        audio_stream
            .get("channels")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        bit_rate / 1000,
    );
    ctx.edit(msg, &body).await?;
    Ok(())
}

const AUDIO_CONVERT_FORMATS: [&str; 6] = ["mp3", "wav", "ogg", "aac", "flac", "opus"];

fn codec_for(fmt: &str) -> &'static str {
    match fmt {
        "mp3" => "libmp3lame",
        "wav" => "pcm_s16le",
        "ogg" => "libvorbis",
        "aac" => "aac",
        "flac" => "flac",
        "opus" => "libopus",
        _ => "libmp3lame",
    }
}

async fn audioconvert(ctx: &Ctx) -> Result<()> {
    let fmt = ctx.args.trim().to_lowercase();
    if !AUDIO_CONVERT_FORMATS.contains(&fmt.as_str()) {
        ctx.reply(&format!(
            "Audio Convert — Usage: .audioconvert <format>\nFormats: {}",
            AUDIO_CONVERT_FORMATS.join(", ")
        ))
        .await?;
        return Ok(());
    }
    let Some(input) = resolve_input(ctx, "audiotools.py:audioconvert").await? else {
        ctx.reply("Audio Convert — Error: Reply to audio/video")
            .await?;
        return Ok(());
    };
    let msg = ctx
        .reply(&format!(
            "Audio Convert — Status: Converting to {}...",
            fmt.to_uppercase()
        ))
        .await?;
    let out = tmp_file(&format!("{}.{}", stem_of(&input), fmt));
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-codec:a",
        codec_for(&fmt),
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, &format!("converted to {fmt}")).await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn audiosplit(ctx: &Ctx) -> Result<()> {
    let ts = ctx.args.trim();
    if ts.is_empty() || !ts.contains(':') {
        ctx.reply("Audio Split — Usage: .audiosplit <mm:ss>")
            .await?;
        return Ok(());
    }
    let Some(input) = resolve_input(ctx, "audiotools.py:audiosplit").await? else {
        ctx.reply("Audio Split — Error: Reply to audio").await?;
        return Ok(());
    };
    let (m_s, s_s) = ts.split_once(':').unwrap();
    let split_at: i64 = match (m_s.trim().parse::<i64>(), s_s.trim().parse::<i64>()) {
        (Ok(m), Ok(s)) => m * 60 + s,
        _ => {
            ctx.reply("Audio Split — Error: Invalid time format")
                .await?;
            return Ok(());
        }
    };
    if (split_at as f64) >= probe_duration(&input).await? {
        ctx.reply("Audio Split — Error: Split point exceeds duration")
            .await?;
        return Ok(());
    }
    let msg = ctx
        .reply(&format!("Audio Split — Status: Splitting at {ts}..."))
        .await?;
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp3".to_string());
    let out1 = tmp_file(&format!("{}_part1.{ext}", stem_of(&input)));
    let out2 = tmp_file(&format!("{}_part2.{ext}", stem_of(&input)));
    let split_str = split_at.to_string();
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-to",
        &split_str,
        "-c",
        "copy",
        out1.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-ss",
        &split_str,
        "-c",
        "copy",
        out2.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    for out in [&out1, &out2] {
        if out.exists() {
            ctx.send_file(out, "split part").await?;
        }
    }
    ctx.delete(msg).await?;
    cleanup(&[&out1, &out2]).await;
    Ok(())
}

async fn audioreverse(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "audiotools.py:audioreverse").await? else {
        ctx.reply("Audio Reverse — Error: Reply to audio/video")
            .await?;
        return Ok(());
    };
    let msg = ctx
        .reply("Audio Reverse — Status: Reversing audio...")
        .await?;
    let out = tmp_file(&format!("{}_reverse.aac", stem_of(&input)));
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-filter:a",
        "areverse",
        "-c:a",
        "copy",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, "reversed audio").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn audiomerge(ctx: &Ctx) -> Result<()> {
    // Python walks the last 10 own audio messages in the chat
    // (`e.client.iter_messages`) and downloads each — both steps need the
    // Telegram client. Local files may be passed as arguments instead.
    // TODO(responder-op): iter_messages + download replied media
    // (Python: audiotools.py:audiomerge).
    let files: Vec<PathBuf> = ctx
        .args
        .split_whitespace()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .collect();
    if files.is_empty() {
        if ctx.reply_to_id.is_some() || ctx.args.trim().is_empty() {
            return Err(Error::Other(
                "listing + downloading recent own audio messages requires a binary Responder op (Python: audiotools.py:audiomerge)".into(),
            ));
        }
        ctx.reply("Audio Merge — Error: Reply to multiple audio files to merge")
            .await?;
        return Ok(());
    }
    if files.len() < 2 {
        ctx.reply("Audio Merge — Error: Need at least 2 audio files")
            .await?;
        return Ok(());
    }
    let msg = ctx
        .reply(&format!(
            "Audio Merge — Status: Merging {} files...",
            files.len()
        ))
        .await?;
    let list_file = tmp_file("audio_list.txt");
    let mut list = String::new();
    for f in &files {
        list.push_str(&format!("file '{}'\n", f.to_string_lossy()));
    }
    tokio::fs::write(&list_file, list).await?;
    let out = tmp_file("merged_audio.mp3");
    let r = run_ffmpeg(&[
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        list_file.to_str().unwrap_or(""),
        "-c",
        "copy",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await;
    cleanup(&[&list_file]).await;
    r?;
    ctx.send_file(&out, "merged audio").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

/// `fns.time_formatter` re-export note: kept here so the import stays live.
#[allow(dead_code)]
fn _fmt_ms(ms: u64) -> String {
    time_formatter(ms)
}
