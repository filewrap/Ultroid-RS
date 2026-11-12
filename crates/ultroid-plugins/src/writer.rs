//! Writer plugin — port of `plugins/writer.py` (86 LOC).
//!
//! * `.gethtml <url>` — fetch a URL and send it back as `file.html`.
//! * `.image <text|html>` — render text/HTML to an image (headless Chromium).
//! * `.write <text>` — handwrite text onto `resources/extras/template.jpg`
//!   with `resources/fonts/assfont.ttf` (Python: PIL `ImageDraw.text`).
//!
//! Text rendering uses ffmpeg's `drawtext` filter (white-on-black mask,
//! luminance → alpha in the `image` crate): the workspace has no font
//! rasterizer crate, and ffmpeg is already a runtime dependency of the
//! project (voice/media conversion). No headless browser is vendored;
//! `.image` drives a system Chromium when one is installed.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::util::{def, reg_add};
use tokio::process::Command as AsyncCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available -\n\n• `{i}write <text or reply to text>`\n   It will write on a paper.\n\n• `{i}image <text or reply to html or any doc file>`\n   Write a image from html or any text.";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("gethtml", "gethtml( (.*)|$)", HELP), |ctx| {
        Box::pin(gethtml(ctx))
    });
    reg_add(registry, def("image", "image( (.*)|$)", HELP), |ctx| {
        Box::pin(image_cmd(ctx))
    });
    reg_add(registry, def("write", "write( (.*)|$)", HELP), |ctx| {
        Box::pin(write_cmd(ctx))
    });
}

/// Python `fns.text_set`: wrap `text` into lines of at most 55 chars, max 25.
fn text_set(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if text.chars().count() <= 55 {
        lines.push(text.to_string());
    } else {
        for line in text.split('\n') {
            if line.chars().count() <= 55 {
                lines.push(line.to_string());
            } else {
                let chars: Vec<char> = line.chars().collect();
                let k = chars.len() / 55;
                for z in 1..=k + 1 {
                    let start = (z - 1) * 55;
                    let end = (z * 55).min(chars.len());
                    if start < chars.len() {
                        lines.push(chars[start..end].iter().collect());
                    }
                }
            }
        }
    }
    lines.truncate(25);
    lines
}

/// Escape a value for use inside an ffmpeg filter argument.
fn ff_escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('\'', "\\\'")
        .replace(':', "\\:")
}

/// Render `text` with ffmpeg drawtext as a white-on-black mask (RGB PNG).
/// `x`/`y` are the top-left position on a `canvas` of `size`. Returns the
/// saved mask path. `stroke` is the drawtext `borderw` (white outline).
async fn render_mask(
    text: &str,
    font: &Path,
    size: u32,
    canvas: (u32, u32),
    x: i64,
    y: i64,
    stroke: u32,
    out: &Path,
) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("writer_text_{}.txt", std::process::id()));
    // drawtext expands `%{...}`; `%%` is the literal-percent escape.
    let safe = text.replace('%', "%%");
    tokio::fs::write(&tmp, safe).await?;
    let filter = format!(
        "drawtext=fontfile='{}':textfile='{}':fontsize={}:fontcolor=white:borderw={}:bordercolor=white:x={}:y={},format=rgb24",
        ff_escape(&font.to_string_lossy()),
        ff_escape(&tmp.to_string_lossy()),
        size,
        stroke,
        x,
        y
    );
    let status = AsyncCommand::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:s={}x{}", canvas.0, canvas.1),
            "-vf",
            &filter,
            "-frames:v",
            "1",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await?;
    let _ = tokio::fs::remove_file(&tmp).await;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg drawtext failed for text render (exit {status})"
        )));
    }
    Ok(())
}

/// Bounding box of the bright (text) pixels in a white-on-black mask.
fn mask_bbox(path: &Path) -> Result<Option<(u32, u32, u32, u32)>> {
    let img = image::open(path).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let g = img.to_luma8();
    let (w, h) = g.dimensions();
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for (x, y, p) in g.enumerate_pixels() {
        if p[0] > 40 {
            if x < min_x {
                min_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if x > max_x {
                max_x = x;
            }
            if y > max_y {
                max_y = y;
            }
        }
    }
    if max_x < min_x || max_y < min_y {
        return Ok(None);
    }
    Ok(Some((min_x, min_y, max_x, max_y)))
}

/// Composite a white-on-black mask onto `base` with a solid `fill` colour:
/// mask luminance becomes the alpha channel.
fn composite_mask(base: &mut image::RgbaImage, mask_path: &Path, fill: [u8; 3]) -> Result<()> {
    let mask = image::open(mask_path).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let m = mask.to_luma8();
    let (w, h) = base.dimensions();
    for (x, y, mp) in m.enumerate_pixels() {
        if x >= w || y >= h {
            continue;
        }
        let a = mp[0] as f32 / 255.0;
        if a > 0.0 {
            let px = base.get_pixel_mut(x, y);
            for i in 0..3 {
                px[i] = ((fill[i] as f32 * a) + (px[i] as f32 * (1.0 - a))).round() as u8;
            }
            px[3] = 255;
        }
    }
    Ok(())
}

async fn gethtml(ctx: &Ctx) -> Result<()> {
    let txt = ctx.args.trim();
    if txt.is_empty() {
        ctx.reply("`Either reply to any file or give any text`")
            .await?;
        return Ok(());
    }
    let link = ctx
        .text
        .splitn(2, ' ')
        .nth(1)
        .unwrap_or(txt)
        .trim()
        .to_string();
    let resp = ctx.http.get(&link).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "failed to fetch {link}: HTTP {}",
            resp.status()
        )));
    }
    let body = resp.text().await?;
    let path = PathBuf::from("file.html");
    tokio::fs::write(&path, body).await?;
    ctx.send_file(&path, "").await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(())
}

/// Find a headless-Chromium binary, if one is installed.
fn chromium_binary() -> Option<&'static str> {
    for cand in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if std::process::Command::new("which")
            .arg(cand)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(cand);
        }
    }
    None
}

async fn image_cmd(ctx: &Ctx) -> Result<()> {
    let txt = ctx.args.trim().to_string();
    let mut html: Option<String> = None;
    if !txt.is_empty() {
        html = Some(ctx.text.splitn(2, ' ').nth(1).unwrap_or(&txt).to_string());
    } else if ctx.reply_to_id.is_some() {
        if let Some(rt) = &ctx.reply_to_text {
            if !rt.is_empty() {
                html = Some(rt.clone());
            }
        }
        if html.is_none() {
            // TODO(responder-op): download replied-to media file (Python:
            // writer.py:f2i — `e.client.download_media(r.media)`)
            return Err(Error::Other(
                "download replied media requires a binary Responder op (Python: writer.py:f2i)"
                    .to_string(),
            ));
        }
    }
    let Some(html) = html else {
        ctx.reply("`Either reply to any file or give any text`")
            .await?;
        return Ok(());
    };
    let html = html.replace('\n', "<br>");
    let chrome = chromium_binary().ok_or_else(|| {
        Error::Other(
            "no headless Chromium installed: rendering HTML to an image needs a \
             Chromium binary (chromium / google-chrome) on PATH (Python: writer.py:f2i \
             via htmlwebshot/WebShot)"
                .to_string(),
        )
    })?;
    // Python injects this CSS into the WebShot render.
    let css = "body {background: white;} p {color: red;}";
    let page = format!("<html><head><style>{css}</style></head><body>{html}</body></html>");
    let dir = std::env::temp_dir();
    let src = dir.join(format!("writer_page_{}.html", std::process::id()));
    let out = dir.join(format!("writer_shot_{}.png", std::process::id()));
    tokio::fs::write(&src, page).await?;
    let status = AsyncCommand::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--hide-scrollbars",
            "--window-size=1280,800",
            &format!("--screenshot={}", out.display()),
            &format!("file://{}", src.display()),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    let _ = tokio::fs::remove_file(&src).await;
    if !status.success() || !out.exists() {
        return Err(Error::Other(format!(
            "headless Chromium screenshot failed (exit {status})"
        )));
    }
    ctx.send_file(&out, "").await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

async fn write_cmd(ctx: &Ctx) -> Result<()> {
    let text = if let Some(rt) = &ctx.reply_to_text {
        if !rt.is_empty() {
            rt.clone()
        } else {
            ctx.args.trim().to_string()
        }
    } else {
        ctx.args.trim().to_string()
    };
    if text.is_empty() {
        ctx.reply(&ctx.get_str("writer_1")).await?;
        return Ok(());
    }
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;

    let template = PathBuf::from("resources/extras/template.jpg");
    let font = PathBuf::from("resources/fonts/assfont.ttf");
    if !template.exists() {
        return Err(Error::Other(
            "resources/extras/template.jpg not found (Python: writer.py:writer)".to_string(),
        ));
    }
    if !font.exists() {
        return Err(Error::Other(
            "resources/fonts/assfont.ttf not found (Python: writer.py:writer)".to_string(),
        ));
    }
    let mut img = image::open(&template)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();

    // Line height: PIL `font.getbbox("hg")` height at size 30.
    let dir = std::env::temp_dir();
    let probe = dir.join(format!("writer_hg_{}.png", std::process::id()));
    render_mask("hg", &font, 30, (w, h), 0, 0, 0, &probe).await?;
    let line_height = match mask_bbox(&probe)? {
        Some((_, y0, _, y1)) => (y1 - y0 + 1) as i64,
        None => 30,
    };
    let _ = tokio::fs::remove_file(&probe).await;

    let mut y: i64 = 140;
    for line in text_set(&text) {
        if line.is_empty() {
            y += line_height - 5;
            continue;
        }
        let mask = dir.join(format!("writer_line_{}.png", std::process::id()));
        render_mask(&line, &font, 30, (w, h), 150, y, 0, &mask).await?;
        composite_mask(&mut img, &mask, [1, 22, 55])?;
        let _ = tokio::fs::remove_file(&mask).await;
        y += line_height - 5;
    }
    let file = PathBuf::from("ult.jpg");
    img.save(&file)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    ctx.send_file(&file, "").await?;
    let _ = tokio::fs::remove_file(&file).await;
    ctx.delete(processing).await?;
    Ok(())
}
