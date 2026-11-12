//! Video/audio/image compression — port of `plugins/compressor.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.compress <reply to video> [crf] [stream]` — compress video with x265.
//! - `.compressaudio <reply to audio> [bitrate]` — compress audio.
//! - `.compressimage <reply to image> [quality]` — compress image.
//! - `.compressinfo` — show compression details.
//!
//! The assistant-bot `/compress*` replies and the `@assistant compress`
//! inline mode from the Python file are assistant-crate surface
//! (`asst_cmd` / `in_pattern` / `@callback`), not user-plugin commands,
//! so they are not registered here.
//!
//! Each command works on a local file path given as the last argument
//! (compress) or as an argument, and on replied-to Telegram media once it
//! is on disk. Downloading the replied media is the single
//! TODO(responder-op) site per command.

use std::path::{Path, PathBuf};
use tokio::process::Command as ProcCommand;
use ultroid_core::fns::{humanbytes, time_formatter};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "compress",
            r"compress(?:\s+(.*))?$",
            "Compress video using x265: .compress [crf] [stream] (reply to video).",
        ),
        |ctx| Box::pin(compress(ctx)),
    );
    reg_add(
        registry,
        def(
            "compressaudio",
            r"compressaudio(?:\s+(.*))?$",
            "Compress audio: .compressaudio [bitrate] (reply to audio/video).",
        ),
        |ctx| Box::pin(compress_audio(ctx)),
    );
    reg_add(
        registry,
        def(
            "compressimage",
            r"compressimage(?:\s+(.*))?$",
            "Compress image: .compressimage [quality] (reply to image).",
        ),
        |ctx| Box::pin(compress_image(ctx)),
    );
    reg_add(
        registry,
        def(
            "compressinfo",
            "compressinfo$",
            "Show compression details and formats.",
        ),
        |ctx| Box::pin(compress_info(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// TODO(responder-op): download replied media to a local file.
fn todo_download(python_ref: &str) -> Error {
    Error::Other(format!(
        "download replied media requires a binary Responder op (Python: {python_ref})"
    ))
}

/// Last whitespace token of the args that names an existing local file.
fn local_input(ctx: &Ctx) -> Option<PathBuf> {
    ctx.args
        .split_whitespace()
        .rev()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

async fn resolve_input(ctx: &Ctx, python_ref: &str) -> Result<Option<PathBuf>> {
    if let Some(p) = local_input(ctx) {
        return Ok(Some(p));
    }
    if ctx.reply_to_id.is_some() {
        return Err(todo_download(python_ref));
    }
    Ok(None)
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string())
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

async fn file_size(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

async fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = tokio::fs::remove_file(p).await;
    }
}

/// Total frame count: `mediainfo --fullscan` like Python, ffprobe fallback.
async fn total_frames(path: &Path) -> Option<u64> {
    let path_s = path.to_str().unwrap_or("");
    if let Ok(out) = ProcCommand::new("mediainfo")
        .args(["--fullscan", path_s])
        .output()
        .await
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("Frame count") {
                if let Some(v) = line.split(':').nth(1) {
                    if let Ok(n) = v.trim().parse::<u64>() {
                        return Some(n);
                    }
                }
            }
        }
    }
    if let Ok(out) = ProcCommand::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_frames",
            "-of",
            "csv=p=0",
            path_s,
        ])
        .output()
        .await
    {
        if let Ok(n) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
            return Some(n);
        }
    }
    None
}

// ── commands ──────────────────────────────────────────────────────────────

async fn compress(ctx: &Ctx) -> Result<()> {
    let mut crf: u32 = 27;
    let mut to_stream = false;
    for tok in ctx.args.split_whitespace() {
        if let Ok(v) = tok.parse::<u32>() {
            crf = v;
        } else if tok == "stream" {
            to_stream = true;
        }
    }
    let Some(input) = resolve_input(ctx, "compressor.py:compress").await? else {
        ctx.reply("Compress — Error: Reply to a video").await?;
        return Ok(());
    };
    let started = std::time::Instant::now();
    let msg = ctx.reply("Compress — Status: Downloading...").await?;
    let o_size = file_size(&input).await;
    let out = tmp_file(&format!("{}_compressed.mkv", stem_of(&input)));
    let total = total_frames(&input).await;
    ctx.edit(
        msg,
        &format!("Compress — Status: Compressing with CRF {crf}..."),
    )
    .await?;
    let progress_file = tmp_file("progress.txt");
    tokio::fs::write(&progress_file, b"").await?;
    let mut child = ProcCommand::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "quiet",
            "-progress",
            progress_file.to_str().unwrap_or(""),
            "-i",
            input.to_str().unwrap_or(""),
            "-preset",
            "ultrafast",
            "-vcodec",
            "libx265",
            "-crf",
            &crf.to_string(),
            "-c:a",
            "copy",
            out.to_str().unwrap_or(""),
            "-y",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Other(format!("ffmpeg not found or failed to run: {e}")))?;
    // Poll the progress file like Python (every 3s) until ffmpeg exits.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::Other("ffmpeg compression failed".into()));
                }
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(Error::Other(format!("ffmpeg wait failed: {e}"))),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Ok(text) = tokio::fs::read_to_string(&progress_file).await {
            let frames: Vec<u64> = text
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("frame=")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                })
                .collect();
            let sizes: Vec<u64> = text
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("total_size=")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                })
                .collect();
            if let (Some(&elapse), Some(&size)) = (frames.last(), sizes.last()) {
                let elapsed = started.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 {
                    elapse as f64 / elapsed
                } else {
                    0.0
                };
                if speed > 0.0 {
                    let (per, eta) = match total {
                        Some(t) if t > 0 => {
                            let p = elapse as f64 * 100.0 / t as f64;
                            let eta_ms = ((t - elapse) as f64 / speed * 1000.0) as u64;
                            (p, time_formatter(eta_ms))
                        }
                        _ => (0.0, "—".to_string()),
                    };
                    let bars = "●".repeat((per / 5.0).floor() as usize);
                    let e_size = format!(
                        "{} of ~{}",
                        humanbytes(size),
                        if per > 0.0 {
                            humanbytes((size as f64 / per * 100.0) as u64)
                        } else {
                            "—".to_string()
                        }
                    );
                    let _ = ctx
                        .edit(
                            msg,
                            &format!(
                                "Compress\nProgress: `{e_size}`\nBar: `[{bars}] {per:.2}%`\nETA: `~{eta}`"
                            ),
                        )
                        .await;
                }
            }
        }
    }
    cleanup(&[&progress_file]).await;
    let c_size = file_size(&out).await;
    let diff = time_formatter(started.elapsed().as_millis() as u64);
    let differ = 100.0 - (c_size as f64 / o_size.max(1) as f64 * 100.0);
    let caption = format!(
        "**Original Size:** `{}`\n**Compressed Size:** `{}`\n**Compression Ratio:** `{differ:.2}%`\n**Time Taken:** `{diff}`",
        humanbytes(o_size),
        humanbytes(c_size),
    );
    ctx.edit(msg, "Compress — Status: Uploading...").await?;
    // `stream` only changes the Telegram attributes in Python
    // (supports_streaming video attribute); the Responder's send_file
    // carries no attribute surface, so both paths send the file the same
    // way and the flag is recorded in the caption.
    let caption = if to_stream {
        format!("{caption}\n(stream)")
    } else {
        caption
    };
    ctx.send_file(&out, &caption).await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn compress_audio(ctx: &Ctx) -> Result<()> {
    let bitrate = if ctx.args.trim().is_empty() {
        "128k".to_string()
    } else {
        ctx.args.trim().to_string()
    };
    let Some(input) = resolve_input(ctx, "compressor.py:compressaudio").await? else {
        ctx.reply("Compress Audio — Error: Reply to audio/video")
            .await?;
        return Ok(());
    };
    let o_size = file_size(&input).await;
    let out = tmp_file(&format!("{}_compressed.mp3", stem_of(&input)));
    let msg = ctx
        .reply(&format!(
            "Compress Audio — Status: Compressing at {bitrate}..."
        ))
        .await?;
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-b:a",
        &bitrate,
        "-vn",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    let c_size = file_size(&out).await;
    let differ = 100.0 - (c_size as f64 / o_size.max(1) as f64 * 100.0);
    ctx.send_file(&out, "compressed audio").await?;
    ctx.edit(
        msg,
        &format!(
            "Compress Audio — Status: ✅ Done\nOriginal: {}\nCompressed: {}\nRatio: {differ:.2}%",
            humanbytes(o_size),
            humanbytes(c_size),
        ),
    )
    .await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn compress_image(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let quality: i64 = if raw.is_empty() {
        85
    } else {
        match raw.parse::<i64>() {
            Ok(q) => q.clamp(1, 100),
            Err(_) => {
                ctx.reply("Compress Image — Error: Quality must be a number (1-100)")
                    .await?;
                return Ok(());
            }
        }
    };
    let Some(input) = resolve_input(ctx, "compressor.py:compressimage").await? else {
        ctx.reply("Compress Image — Error: Reply to an image")
            .await?;
        return Ok(());
    };
    let o_size = file_size(&input).await;
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "jpg".to_string());
    let out = tmp_file(&format!("{}_compressed.{ext}", stem_of(&input)));
    let msg = ctx
        .reply(&format!(
            "Compress Image — Status: Compressing at quality {quality}..."
        ))
        .await?;
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-vf",
        "scale=iw*0.5:ih*0.5",
        "-q:v",
        &quality.to_string(),
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    if !out.exists() {
        ctx.reply("Compress Image — Error: Compression failed")
            .await?;
        return Ok(());
    }
    let c_size = file_size(&out).await;
    let differ = 100.0 - (c_size as f64 / o_size.max(1) as f64 * 100.0);
    ctx.send_file(&out, "compressed image").await?;
    ctx.edit(
        msg,
        &format!(
            "Compress Image — Status: ✅ Done\nOriginal: {}\nCompressed: {}\nRatio: {differ:.2}%\nQuality: {quality}%",
            humanbytes(o_size),
            humanbytes(c_size),
        ),
    )
    .await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn compress_info(ctx: &Ctx) -> Result<()> {
    ctx.reply(
        "Compress Info\nVideo: x265 CRF (default 27, range 0-51)\nAudio: Bitrate based (default 128k)\nImage: Quality based (default 85%, range 1-100)\nZip: Use ziptools plugin\nStream: Add 'stream' flag for video streaming",
    )
    .await?;
    Ok(())
}
