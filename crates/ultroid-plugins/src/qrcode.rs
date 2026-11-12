//! QR codes — port of `plugins/qrcode.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.qrcode <text/reply to text>` — makes a QR code of the text.
//! - `.addqr <reply image> <text>` — makes a QR of the text and adds it
//!   to the image.
//! - `.qrdecode <reply to qrcode>` — decodes the QR code.
//!
//! `qrcode`/`addqr` are fully implemented with the `qrcode` crate.
//! Python pastes a 60×60 centre logo (replied sticker/photo, else the
//! account's own profile photo, else the default thumb) into the QR;
//! fetching a profile photo is a Telegram op, so the Rust port renders a
//! plain QR and notes the difference (see report).
//!
//! `qrdecode` needs a QR decoder: Python uses `cv2.QRCodeDetector` and
//! there is no decoder in the workspace deps (see report, missing dep
//! `rqrr`). The command resolves its input file honestly and then errors
//! naming the missing crate instead of faking a decode.

use image::{ImageBuffer, Luma};
use qrcode::{EcLevel, QrCode};
use std::path::{Path, PathBuf};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "qrcode",
            "qrcode( (.*)|$)",
            "Make a QR code of text: .qrcode <text/reply to text>.",
        ),
        |ctx| Box::pin(qrcode_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "addqr",
            "addqr( (.*)|$)",
            "Make a QR of text and add it to an image: .addqr <reply image> <text>.",
        ),
        |ctx| Box::pin(addqr(ctx)),
    );
    reg_add(
        registry,
        def(
            "qrdecode",
            "qrdecode$",
            "Decode a QR code: .qrdecode (reply to QR code).",
        ),
        |ctx| Box::pin(qrdecode(ctx)),
    );
}

fn tmp_file(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-qr-{}.{ext}", uuid::Uuid::new_v4()))
}

/// Resolve the input file: a local path among the args, else replied
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
        let dest = tmp_file("png");
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        return Ok(Some(path));
    }
    Ok(None)
}

async fn qrcode_cmd(ctx: &Ctx) -> Result<()> {
    // Reply text wins over the argument, exactly like Python.
    let msg_text = match ctx.reply_to_text.clone().filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => {
            let a = ctx.args.trim();
            if a.is_empty() {
                ctx.reply("`Give Some Text or Reply").await?;
                return Ok(());
            }
            a.to_string()
        }
    };
    let status = ctx.reply("Processing...").await?;
    let code = QrCode::with_error_correction_level(msg_text.as_bytes(), EcLevel::H)
        .map_err(|e| Error::Other(format!("QR encode failed: {e}")))?;
    let img: ImageBuffer<Luma<u8>, Vec<u8>> = code.render::<Luma<u8>>().build();
    let out = tmp_file("png");
    img.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    ctx.send_file(&out, "QR code").await?;
    ctx.delete(status).await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

async fn addqr(ctx: &Ctx) -> Result<()> {
    let text = ctx.args.trim();
    let Some(input) = resolve_input(ctx, "qrcode.py:qrwater").await? else {
        ctx.reply("`Reply Any Media and Give Text`").await?;
        return Ok(());
    };
    if text.is_empty() {
        ctx.reply("`Reply Any Media and Give Text`").await?;
        return Ok(());
    }
    // The text is the first non-path token; with a local path as the first
    // token the text is the remainder.
    let text = if Path::new(text.split_whitespace().next().unwrap_or("")).is_file() {
        text.splitn(2, ' ').nth(1).unwrap_or("").trim()
    } else {
        text
    };
    if text.is_empty() {
        ctx.reply("`Reply Any Media and Give Text`").await?;
        return Ok(());
    }
    let msg = ctx.reply("Processing...").await?;
    let code =
        QrCode::new(text.as_bytes()).map_err(|e| Error::Other(format!("QR encode failed: {e}")))?;
    // Python: QRCode(box_size=5).
    let qr: ImageBuffer<Luma<u8>, Vec<u8>> =
        code.render::<Luma<u8>>().module_dimensions(5, 5).build();
    let mut bg = image::open(&input)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (bw, bh) = bg.dimensions();
    let (qw, qh) = qr.dimensions();
    let (px, py) = (bw.saturating_sub(qw), bh.saturating_sub(qh));
    for y in 0..qh {
        for x in 0..qw {
            if qr.get_pixel(x, y)[0] < 128 {
                bg.put_pixel(px + x, py + y, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    let out = tmp_file("png");
    bg.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    ctx.send_file(&out, "QR added").await?;
    ctx.delete(msg).await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

async fn qrdecode(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "qrcode.py:decod").await? else {
        ctx.reply("`Reply to Qrcode Media`").await?;
        return Ok(());
    };
    let _ = input;
    // Python decodes with cv2.QRCodeDetector. No QR decoder crate is in
    // the workspace dependencies, so a decode cannot be performed here.
    Err(Error::Other(
        "QR decoding requires the `rqrr` crate (Python: qrcode.py:decod uses cv2.QRCodeDetector); it is not in the workspace dependencies".into(),
    ))
}
