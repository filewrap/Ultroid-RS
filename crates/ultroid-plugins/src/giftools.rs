//! GIF tools — port of `plugins/giftools.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.invertgif` — make GIF inverted (negative).
//! - `.bwgif` — make GIF black and white.
//! - `.rvgif` — reverse a GIF.
//! - `.vtog` — reply to video, create a GIF.
//! - `.gif <query>` — send video regarding a query (via the `gif` inline bot).
//!
//! Each media command works on a local file path given as an argument and
//! on replied-to Telegram media once it is on disk. Downloading the
//! replied media is the single TODO(responder-op) site per command.

use std::path::{Path, PathBuf};
use tokio::process::Command as ProcCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "bwgif",
            "(bw|invert)gif$",
            "Make GIF black and white (reply to GIF).",
        ),
        |ctx| Box::pin(igif(ctx)),
    );
    reg_add(
        registry,
        def(
            "invertgif",
            "(bw|invert)gif$",
            "Make GIF inverted / negative (reply to GIF).",
        ),
        |ctx| Box::pin(igif(ctx)),
    );
    reg_add(
        registry,
        def("rvgif", "rvgif$", "Reverse a GIF/video (reply to video)."),
        |ctx| Box::pin(reverse_gif(ctx)),
    );
    reg_add(
        registry,
        def(
            "gif",
            "gif( (.*)|$)",
            "Send GIF video regarding a query: .gif <query>.",
        ),
        |ctx| Box::pin(gifs(ctx)),
    );
    reg_add(
        registry,
        def("vtog", "vtog$", "Video to GIF (reply to video)."),
        |ctx| Box::pin(vtogif(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// TODO(responder-op): download replied media to a local file.
fn todo_download(python_ref: &str) -> Error {
    Error::Other(format!(
        "download replied media requires a binary Responder op (Python: {python_ref})"
    ))
}

/// First whitespace token of the args that names an existing local file.
fn local_input(ctx: &Ctx) -> Option<PathBuf> {
    ctx.args
        .split_whitespace()
        .next()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
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

async fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = tokio::fs::remove_file(p).await;
    }
}

// ── commands ──────────────────────────────────────────────────────────────

async fn igif(ctx: &Ctx) -> Result<()> {
    // captures[1] is "bw" or "invert" (pattern `(bw|invert)gif$`).
    let mode = ctx
        .captures
        .get(1)
        .cloned()
        .unwrap_or_else(|| "bw".to_string());
    let Some(input) = resolve_input(ctx, "giftools.py:igif").await? else {
        ctx.reply("`Reply To gif only`").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let out = tmp_file("ult.gif");
    let args: Vec<&str> = if mode == "bw" {
        vec![
            "-i",
            input.to_str().unwrap_or(""),
            "-vf",
            "format=gray",
            out.to_str().unwrap_or(""),
            "-y",
        ]
    } else {
        vec![
            "-i",
            input.to_str().unwrap_or(""),
            "-vf",
            "lutyuv=y=negval:u=negval:v=negval",
            out.to_str().unwrap_or(""),
            "-y",
        ]
    };
    run_ffmpeg(&args).await?;
    ctx.send_file(&out, "gif").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn reverse_gif(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "giftools.py:reverse_gif").await? else {
        ctx.reply("`Reply To Video only`").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let out = tmp_file("reversed.mp4");
    run_ffmpeg(&[
        "-i",
        input.to_str().unwrap_or(""),
        "-vf",
        "reverse",
        "-af",
        "areverse",
        out.to_str().unwrap_or(""),
        "-y",
    ])
    .await?;
    ctx.send_file(&out, "- **Reversed Video/GIF**").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn gifs(ctx: &Ctx) -> Result<()> {
    // Python: `ult.client.inline_query("gif", query)` then clicks a random
    // result (or `n` results when the query ends with `;<n>`). Inline
    // queries/clicks need the Telegram client.
    // TODO(responder-op): inline_query("gif") + click result
    // (Python: giftools.py:gifs).
    let query = ctx.args.trim();
    if query.is_empty() {
        ctx.reply("`.gif <query>`").await?;
        return Ok(());
    }
    Err(Error::Other(
        "inline query to the 'gif' bot requires a binary Responder op (Python: giftools.py:gifs)"
            .into(),
    ))
}

async fn vtogif(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "giftools.py:vtogif").await? else {
        ctx.reply("`Reply To video only`").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let out = tmp_file("ult.gif");
    // Python: short videos (<120s) convert whole; long ones take 100s
    // starting at 3s. Duration is only known from the Telegram attributes
    // in Python, so use ffprobe here.
    let duration = ffprobe_duration(&input).await.unwrap_or(0.0);
    let args: Vec<String> = if duration < 120.0 {
        vec![
            "-i".to_string(),
            input.to_string_lossy().into_owned(),
            "-vf".to_string(),
            "fps=10,scale=320:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse"
                .to_string(),
            "-loop".to_string(),
            "0".to_string(),
            out.to_string_lossy().into_owned(),
            "-y".to_string(),
        ]
    } else {
        vec![
            "-ss".to_string(),
            "3".to_string(),
            "-t".to_string(),
            "100".to_string(),
            "-i".to_string(),
            input.to_string_lossy().into_owned(),
            "-vf".to_string(),
            "fps=10,scale=320:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse"
                .to_string(),
            "-loop".to_string(),
            "0".to_string(),
            out.to_string_lossy().into_owned(),
            "-y".to_string(),
        ]
    };
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_ffmpeg(&arg_refs).await?;
    ctx.send_file(&out, "gif").await?;
    ctx.delete(msg).await?;
    cleanup(&[&out]).await;
    Ok(())
}

async fn ffprobe_duration(path: &Path) -> Result<f64> {
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
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(data
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0))
}
