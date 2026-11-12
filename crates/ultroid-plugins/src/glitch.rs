//! Glitch — port of `plugins/glitch.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.glitch <reply to media>` — gives a glitchy GIF.
//!
//! Python shells out to the `glitch_me` CLI (installed on demand from
//! `github.com/1Danish-00/glitch_me`). The Rust port runs the same CLI
//! (`glitch_me gif --line_count 200 -f 10 -d 50 <in> <out>`) when it is
//! present and errors honestly naming it when it is not — it never
//! pretends to glitch without the tool.
//!
//! The command works on a local file path given as an argument and on
//! replied-to Telegram media, downloaded via the Responder
//! (`download_media`).

use std::path::PathBuf;
use tokio::process::Command as ProcCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "glitch",
            "glitch$",
            "Glitchy GIF: .glitch (reply to media).",
        ),
        |ctx| Box::pin(glitch(ctx)),
    );
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

async fn glitch(ctx: &Ctx) -> Result<()> {
    let input = match ctx
        .args
        .split_whitespace()
        .next()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        Some(p) => p,
        None => {
            if let Some(reply_id) = ctx.reply_to_id {
                let dest = tmp_file("input");
                ctx.responder
                    .download_media(ctx.chat_id, reply_id as i32, &dest)
                    .await?
            } else {
                ctx.reply("Reply to an image/sticker to glitch it").await?;
                return Ok(());
            }
        }
    };
    let msg = ctx.reply("Glitching...").await?;
    let out = tmp_file("ult.gif");
    // Same invocation as Python: glitch_me gif --line_count 200 -f 10 -d 50.
    let run = ProcCommand::new("glitch_me")
        .args([
            "gif",
            "--line_count",
            "200",
            "-f",
            "10",
            "-d",
            "50",
            input.to_str().unwrap_or(""),
            out.to_str().unwrap_or(""),
        ])
        .output()
        .await;
    let output = match run {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ctx.edit(
                msg,
                "Glitch — Error: `glitch_me` CLI is not installed (Python installs it from github.com/1Danish-00/glitch_me).",
            )
            .await?;
            return Err(Error::Other(
                "glitch_me CLI not found (Python: glitch.py:_ installs github.com/1Danish-00/glitch_me)"
                    .into(),
            ));
        }
        Err(e) => return Err(Error::Other(format!("glitch_me failed to run: {e}"))),
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        ctx.edit(
            msg,
            &format!("Glitch — Error: glitch_me failed: {}", err.trim()),
        )
        .await?;
        return Err(Error::Other(format!("glitch_me failed: {}", err.trim())));
    }
    if !out.exists() {
        ctx.edit(msg, "Glitch — Error: glitch_me produced no output")
            .await?;
        return Err(Error::Other("glitch_me produced no output".into()));
    }
    ctx.send_file(&out, "glitched").await?;
    ctx.delete(msg).await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}
