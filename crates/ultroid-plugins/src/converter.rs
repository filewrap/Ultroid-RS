//! Media converter — port of `plugins/converter.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.thumbnail <reply to image>` — set custom thumbnail.
//! - `.rename <new_name>` — rename a replied file.
//! - `.convert <format>` — convert media format
//!   (png, webp, gif, webm, mp4, mp3, wav, ogg, avi, mov).
//! - `.doc <filename>` — save a text message into a file.
//! - `.open <file_id>` — open and display a file's text content.
//! - `.mediainfo <file_id>` — show detailed media information.
//! - `.thumbset` — set uploaded thumbnail as bot's custom thumbnail.
//!
//! Each media command works on a local file path given as an argument and
//! on replied-to Telegram media once it is on disk. Downloading the
//! replied media (and resolving a saved-message id in `mediainfo`) is the
//! single TODO(responder-op) site per command.

use std::path::{Path, PathBuf};
use tokio::process::Command as ProcCommand;
use ultroid_core::fns::humanbytes;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, db_set, def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "thumbnail",
            "thumbnail$",
            "Set custom thumbnail (reply to image).",
        ),
        |ctx| Box::pin(thumbnail(ctx)),
    );
    reg_add(
        registry,
        def(
            "rename",
            r"rename(?:\s+(.*))?$",
            "Rename a replied file: .rename <new_name>.",
        ),
        |ctx| Box::pin(rename_file(ctx)),
    );
    reg_add(
        registry,
        def(
            "convert",
            r"convert(?:\s+(.*))?$",
            "Convert media format: png, webp, gif, webm, mp4, mp3, wav, ogg, avi, mov.",
        ),
        |ctx| Box::pin(uconverter(ctx)),
    );
    reg_add(
        registry,
        def(
            "doc",
            r"doc(?:\s+(.*))?$",
            "Save a text message into a file: .doc <filename> (reply to text).",
        ),
        |ctx| Box::pin(doc_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "open",
            r"open(?:\s+(.*))?$",
            "Open and display a file's text content.",
        ),
        |ctx| Box::pin(open_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "mediainfo",
            r"mediainfo(?:\s+(.*))?$",
            "Show detailed media information (reply to media or file path).",
        ),
        |ctx| Box::pin(mediainfo(ctx)),
    );
    reg_add(
        registry,
        def(
            "thumbset",
            "thumbset$",
            "Show the currently set custom thumbnail.",
        ),
        |ctx| Box::pin(thumbset(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// TODO(responder-op): download replied media to a local file.
fn todo_download(python_ref: &str) -> Error {
    Error::Other(format!(
        "download replied media requires a binary Responder op (Python: {python_ref})"
    ))
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

async fn run_ffmpeg(in_path: &Path, out_path: &Path) -> Result<()> {
    let out = ProcCommand::new("ffmpeg")
        .args([
            "-i",
            in_path.to_str().unwrap_or(""),
            "-y",
            out_path.to_str().unwrap_or(""),
        ])
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

async fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = tokio::fs::remove_file(p).await;
    }
}

/// Upload bytes to telegra.ph (`POST /upload`, multipart form, field
/// `file`). The reqwest build has no `multipart` feature, so the body is
/// assembled by hand. Returns the `https://telegra.ph/...` URL.
async fn telegraph_upload(ctx: &Ctx, bytes: &[u8], filename: &str) -> Result<String> {
    let boundary = format!("----ultroid{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = ctx
        .http
        .post("https://telegra.ph/upload")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await?;
    let val: serde_json::Value = resp.json().await?;
    let src = val
        .get(0)
        .and_then(|v| v.get("src"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("telegra.ph upload failed: {val}")))?;
    Ok(format!("https://telegra.ph{src}"))
}

/// Paste long text to spaceb.in (Python `fns.get_paste` fallback).
async fn spacebin_paste(ctx: &Ctx, text: &str) -> Result<String> {
    let resp = ctx
        .http
        .post("https://spaceb.in/api/v1/documents")
        .json(&serde_json::json!({"content": text, "extension": "txt"}))
        .send()
        .await?;
    let val: serde_json::Value = resp.json().await?;
    let id = val
        .get("payload")
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("spaceb.in paste failed: {val}")))?;
    Ok(id.to_string())
}

fn guess_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("opus") => "audio/opus",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

// ── commands ──────────────────────────────────────────────────────────────

async fn thumbnail(ctx: &Ctx) -> Result<()> {
    // Input: the replied image. A local image path may be passed as an arg.
    let input = match ctx
        .args
        .split_whitespace()
        .next()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        Some(p) => p,
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_download("converter.py:thumbnail"));
            }
            ctx.reply("Thumbnail — Error: Reply to a photo or media with thumb")
                .await?;
            return Ok(());
        }
    };
    let msg = ctx.reply("Thumbnail — Status: Uploading...").await?;
    let bytes = tokio::fs::read(&input).await?;
    let filename = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "thumb.jpg".to_string());
    let url = match telegraph_upload(ctx, &bytes, &filename).await {
        Ok(u) => u,
        Err(e) => {
            ctx.edit(msg, &format!("Thumbnail — Error: {e}")).await?;
            return Err(e);
        }
    };
    db_set(ctx, "CUSTOM_THUMBNAIL", &url).await?;
    // Python also replaces the bot's default thumbnail on disk.
    let dest = PathBuf::from("resources/extras/ultroid.jpg");
    if let Some(parent) = dest.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&dest, &bytes).await;
    ctx.edit(
        msg,
        &format!("Thumbnail — Status: ✅ Thumbnail set\nLink: <a href='{url}'>{url}</a>"),
    )
    .await?;
    Ok(())
}

async fn rename_file(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<&str> = ctx.args.split_whitespace().collect();
    let Some(&new_name) = tokens.first() else {
        ctx.reply("Rename — Usage: .rename <new_name>").await?;
        return Ok(());
    };
    if tokens.is_empty() || new_name.is_empty() {
        ctx.reply("Rename — Error: Provide a new name").await?;
        return Ok(());
    }
    // Local file may be passed as the second token; otherwise reply media.
    let input = match tokens.get(1).map(PathBuf::from).filter(|p| p.is_file()) {
        Some(p) => p,
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_download("converter.py:rename_file"));
            }
            ctx.reply("Rename — Usage: .rename <new_name>").await?;
            return Ok(());
        }
    };
    let msg = ctx.reply("Rename — Status: Renaming...").await?;
    let dir = input.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let new_path = dir.join(new_name);
    tokio::fs::rename(&input, &new_path).await?;
    ctx.send_file(&new_path, &format!("`{new_name}`")).await?;
    ctx.delete(msg).await?;
    cleanup(&[&new_path]).await;
    Ok(())
}

const CONV_KEYS: [(&str, &str); 19] = [
    ("png", "png"),
    ("jpg", "png"),
    ("jpeg", "png"),
    ("img", "png"),
    ("webp", "webp"),
    ("sticker", "webp"),
    ("image", "png"),
    ("gif", "gif"),
    ("webm", "webm"),
    ("json", "json"),
    ("tgs", "tgs"),
    ("mp4", "mp4"),
    ("avi", "avi"),
    ("mov", "mov"),
    ("mkv", "mkv"),
    ("mp3", "mp3"),
    ("wav", "wav"),
    ("ogg", "ogg"),
    ("aac", "aac"),
];

async fn uconverter(ctx: &Ctx) -> Result<()> {
    let tokens: Vec<&str> = ctx.args.split_whitespace().collect();
    let fmt = tokens.first().copied().unwrap_or("").to_lowercase();
    let Some(&(_, convert_to)) = CONV_KEYS.iter().find(|(k, _)| *k == fmt) else {
        ctx.reply(&format!(
            "Convert — Usage: .convert <format>\nFormats: {}",
            {
                let mut ks: Vec<&str> = CONV_KEYS.iter().map(|(k, _)| *k).collect();
                ks.sort_unstable();
                ks.join(", ")
            }
        ))
        .await?;
        return Ok(());
    };
    let input = match tokens.get(1).map(PathBuf::from).filter(|p| p.is_file()) {
        Some(p) => p,
        None => {
            if ctx.reply_to_id.is_some() {
                return Err(todo_download("converter.py:uconverter"));
            }
            ctx.reply("Convert — Error: Reply to media").await?;
            return Ok(());
        }
    };
    let msg = ctx.reply("Convert — Status: Downloading...").await?;
    let out_name = tmp_file(&format!(
        "{}.{}",
        {
            let base = input
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "converted".to_string());
            base
        },
        convert_to
    ));
    ctx.edit(
        msg,
        &format!(
            "Convert — Status: Converting to {}...",
            convert_to.to_uppercase()
        ),
    )
    .await?;
    run_ffmpeg(&input, &out_name).await?;
    if !out_name.exists() {
        ctx.edit(msg, "Convert — Error: Conversion failed").await?;
        return Ok(());
    }
    let caption = format!(
        "`{}`",
        out_name
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    ctx.send_file(&out_name, &caption).await?;
    ctx.delete(msg).await?;
    cleanup(&[&out_name]).await;
    Ok(())
}

async fn doc_cmd(ctx: &Ctx) -> Result<()> {
    let filename = ctx.args.trim();
    let Some(text) = ctx.reply_to_text.clone() else {
        ctx.reply("Doc — Usage: .doc <filename> | reply to text")
            .await?;
        return Ok(());
    };
    if filename.is_empty() {
        ctx.reply("Doc — Usage: .doc <filename> | reply to text")
            .await?;
        return Ok(());
    }
    let msg = ctx.reply("Doc — Status: Packing...").await?;
    tokio::fs::write(filename, &text).await?;
    ctx.edit(msg, &format!("Doc — Status: Packing into `{filename}`"))
        .await?;
    ctx.send_file(Path::new(filename), filename).await?;
    let _ = tokio::fs::remove_file(filename).await;
    Ok(())
}

async fn open_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let path: PathBuf = if !arg.is_empty() && Path::new(arg).is_file() {
        PathBuf::from(arg)
    } else if ctx.reply_to_id.is_some() {
        return Err(todo_download("converter.py:open_cmd"));
    } else {
        ctx.reply("Open — Error: Reply to a file or give a local file path")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply("Open — Status: Reading...").await?;
    let data = match tokio::fs::read(&path).await {
        Ok(d) => d,
        Err(e) => {
            ctx.edit(msg, &format!("Open — Error: {e}")).await?;
            return Ok(());
        }
    };
    let text = match String::from_utf8(data) {
        Ok(t) => t,
        Err(_) => {
            ctx.edit(msg, "Open — Error: File is not valid UTF-8 text")
                .await?;
            return Ok(());
        }
    };
    if text.chars().count() <= 4090 {
        ctx.edit(msg, &format!("```{}```", &text[..text.len().min(4090)]))
            .await?;
    } else {
        match spacebin_paste(ctx, &text).await {
            Ok(key) => {
                ctx.edit(
                    msg,
                    &format!(
                        "Open — Status: Message exceeds limits\nPaste: https://spaceb.in/{key}"
                    ),
                )
                .await?;
            }
            Err(_) => {
                ctx.edit(msg, "Open — Error: Too large").await?;
            }
        }
    }
    Ok(())
}

async fn mediainfo(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    // A numeric arg is a saved-message id in Python
    // (`get_messages("me", ids=...)`) — that lookup needs the client.
    // TODO(responder-op): get_messages by id (Python: converter.py:mediainfo).
    if !arg.is_empty() && arg.parse::<i64>().is_ok() {
        return Err(Error::Other(
            "fetching a saved message by id requires a binary Responder op (Python: converter.py:mediainfo)".into(),
        ));
    }
    let path: PathBuf = if !arg.is_empty() && Path::new(arg).is_file() {
        PathBuf::from(arg)
    } else if ctx.reply_to_id.is_some() {
        return Err(todo_download("converter.py:mediainfo"));
    } else {
        ctx.reply("Media Info — Error: Reply to media or provide file ID")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply("Media Info — Status: Fetching info...").await?;
    let meta = tokio::fs::metadata(&path).await?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown".to_string());
    let mime = guess_mime(&path);
    let size = meta.len();
    let (w, h) = if mime.starts_with("image") {
        image::open(&path)
            .ok()
            .map(|im| (im.width(), im.height()))
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let icon = if mime.contains("image") {
        "🖼️"
    } else if mime.contains("video") {
        "🎬"
    } else if mime.contains("audio") {
        "🎵"
    } else {
        "📎"
    };
    let body = format!(
        "Media Info\nName: {icon} `{name}`\nType: `{mime}`\nSize: {}\nDuration: N/A\nWidth: {}\nHeight: {}",
        humanbytes(size),
        if w > 0 { w.to_string() } else { "N/A".into() },
        if h > 0 { h.to_string() } else { "N/A".into() },
    );
    ctx.edit(msg, &body).await?;
    Ok(())
}

async fn thumbset(ctx: &Ctx) -> Result<()> {
    match db_get(ctx, "CUSTOM_THUMBNAIL").await? {
        Some(thumb) if !thumb.is_empty() => {
            ctx.reply(&format!("Thumbset — Status: ✅ Already set\nLink: {thumb}"))
                .await?;
        }
        _ => {
            ctx.reply("Thumbset — Info: No thumbnail set. Use `thumbnail` command first.")
                .await?;
        }
    }
    Ok(())
}
