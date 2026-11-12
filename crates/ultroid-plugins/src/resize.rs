//! Image size & resize — port of `plugins/resize.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.size <reply to media>` — get the size of it.
//! - `.resize <number> <number>` — resize image on x, y axis.
//!   e.g. `.resize 690 960`
//!
//! Each command works on a local file path given as an argument and on
//! replied-to Telegram media, downloaded via the Responder
//! (`download_media`).

use std::path::PathBuf;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "size",
            "size$",
            "Get the dimensions of replied media: .size.",
        ),
        |ctx| Box::pin(size(ctx)),
    );
    reg_add(
        registry,
        def(
            "resize",
            "resize( (.*)|$)",
            "Resize image: .resize <width> <height> (reply to media).",
        ),
        |ctx| Box::pin(resize(ctx)),
    );
}

/// Resolve the input image: a local path among the args, else replied
/// media (downloaded via the Responder), else usage error.
async fn resolve_input(ctx: &Ctx, _python_ref: &str) -> Result<Option<PathBuf>> {
    if let Some(p) = ctx
        .args
        .split_whitespace()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    {
        return Ok(Some(p));
    }
    if let Some(reply_id) = ctx.reply_to_id {
        let dest =
            std::env::temp_dir().join(format!("ultroid-resize-input-{}", uuid::Uuid::new_v4()));
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        return Ok(Some(path));
    }
    Ok(None)
}

async fn size(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "resize.py:size").await? else {
        ctx.reply("Reply to an image to get its size").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let img = image::open(&input).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let (x, y) = (img.width(), img.height());
    ctx.edit(msg, &format!("Dimension Of This Image Is\n`{x} x {y}`"))
        .await?;
    Ok(())
}

async fn resize(ctx: &Ctx) -> Result<()> {
    let parts: Vec<&str> = ctx.args.split_whitespace().collect();
    let dims: Vec<u32> = parts.iter().filter_map(|t| t.parse::<u32>().ok()).collect();
    if dims.len() != 2 {
        ctx.reply("Give Some Size To Resize, Like `.resize 720 1080`")
            .await?;
        return Ok(());
    }
    let Some(input) = resolve_input(ctx, "resize.py:resize").await? else {
        ctx.reply("Reply to an image to resize it").await?;
        return Ok(());
    };
    let msg = ctx.reply("Processing...").await?;
    let img = image::open(&input).map_err(|e| Error::Other(format!("image open: {e}")))?;
    // Python: PIL `resize((x, y))` — exact dimensions, no aspect keep.
    let resized = img.resize_exact(dims[0], dims[1], image::imageops::FilterType::Lanczos3);
    let out = std::env::temp_dir().join(format!("ultroid-resized-{}.png", uuid::Uuid::new_v4()));
    resized
        .save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    ctx.send_file(&out, &format!("resized to {}x{}", dims[0], dims[1]))
        .await?;
    ctx.delete(msg).await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}
