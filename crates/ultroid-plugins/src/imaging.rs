//! Basic image transforms & effects — port of `plugins/imaging.py`
//! (source HEAD 5d09531) over `pyUltroid/imaging/Imaging`.
//!
//! Commands (from the Python docstring):
//! Basic: `.grey`/`.grayscale`, `.invert`, `.blur [radius]`,
//! `.sharpen [factor]`, `.rotate <degrees>`, `.flip`, `.flop`, `.mirror`.
//! Effects: `.sketch`, `.cartoon`/`.toon`, `.danger`, `.bluec`, `.vintage`,
//! `.glitch [intensity]`, `.emboss`, `.edges`, `.pixelate [size]`,
//! `.vignette`.
//! Adjustments: `.brightness <factor>`, `.contrast <factor>`,
//! `.saturation <factor>`.
//! Composite/text: `.caption <text>`, `.meme <top> | <bottom>`,
//! `.watermark <text>`, `.border [color] [width]`.
//! Generate: `.csample <color>`, `.codeimg`, `.textimg <text>`,
//! `.errcard` (owner-only), `.statcard` (owner-only).
//! Info: `.imginfo`.
//!
//! Every image-taking command works on a local file path given as an
//! argument and on replied-to Telegram media, downloaded via the Responder
//! (`download_media`). Pixel transforms are implemented directly on the `image`
//! crate; text-on-image commands need a font rasterizer (see
//! `TEXT_DEP_NOTE`) and error honestly about it instead of faking text.

use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, Rgba};
use std::path::{Path, PathBuf};
use ultroid_core::imaging as core_img;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

/// Text rendering (caption/meme/watermark/codeimg/textimg) needs the
/// `imageproc` + `ab_glyph` crates, which are not in the workspace deps.
/// See the final report (missing dep).
const TEXT_DEP_NOTE: &str = "text rendering requires the `imageproc` + `ab_glyph` crates (not in the workspace dependencies)";

pub fn register(registry: &mut Registry) {
    let cmds: &[(&str, &str, &str)] = &[
        (
            "grey",
            "(grey|grayscale)$",
            "Convert to grayscale (reply to image).",
        ),
        (
            "grayscale",
            "(grey|grayscale)$",
            "Convert to grayscale (reply to image).",
        ),
        ("invert", "invert$", "Invert colours (reply to image)."),
        (
            "blur",
            r"blur(?: |$)(\d*\.?\d*)",
            "Gaussian blur: .blur [radius] (reply to image).",
        ),
        (
            "sharpen",
            r"sharpen(?: |$)(\d*\.?\d*)",
            "Sharpen: .sharpen [factor] (reply to image).",
        ),
        (
            "rotate",
            r"rotate(?: |$)(-?\d+\.?\d*)",
            "Rotate image: .rotate <degrees> (reply to image).",
        ),
        (
            "flip",
            "(flip|mirror)$",
            "Flip horizontal (reply to image).",
        ),
        (
            "mirror",
            "(flip|mirror)$",
            "Flip horizontal, alias for flip (reply to image).",
        ),
        ("flop", "flop$", "Flip vertical (reply to image)."),
        ("sketch", "sketch$", "Pencil sketch (reply to image)."),
        (
            "cartoon",
            "(cartoon|toon)$",
            "Cartoon effect (reply to image).",
        ),
        (
            "toon",
            "(cartoon|toon)$",
            "Cartoon effect (reply to image).",
        ),
        ("danger", "danger$", "Red channel boost (reply to image)."),
        ("bluec", "bluec$", "Blue channel boost (reply to image)."),
        (
            "vintage",
            "vintage$",
            "Sepia/vintage look (reply to image).",
        ),
        (
            "glitch",
            r"glitch(?: |$)(\d*)",
            "RGB glitch effect: .glitch [intensity] (reply to image).",
        ),
        ("emboss", "emboss$", "Emboss effect (reply to image)."),
        ("edges", "edges$", "Edge detection (reply to image)."),
        (
            "pixelate",
            r"pixelate(?: |$)(\d*)",
            "Pixelation: .pixelate [size] (reply to image).",
        ),
        (
            "vignette",
            "vignette$",
            "Dark vignette frame (reply to image).",
        ),
        (
            "brightness",
            r"brightness(?: |$)(\d*\.?\d*)",
            "Brightness: .brightness <factor> (reply to image).",
        ),
        (
            "contrast",
            r"contrast(?: |$)(\d*\.?\d*)",
            "Contrast: .contrast <factor> (reply to image).",
        ),
        (
            "saturation",
            r"saturation(?: |$)(\d*\.?\d*)",
            "Saturation: .saturation <factor> (reply to image).",
        ),
        (
            "caption",
            r"caption(?: |$)([\s\S]+)",
            "Add caption to image: .caption <text> (reply to image).",
        ),
        (
            "meme",
            r"meme(?: |$)([\s\S]*)",
            "Meme text overlay: .meme <top> | <bottom> (reply to image).",
        ),
        (
            "watermark",
            r"watermark(?: |$)([\s\S]+)",
            "Tiled watermark: .watermark <text> (reply to image).",
        ),
        (
            "border",
            r"border(?: |$)(.*)",
            "Add border: .border [color] [width] (reply to image).",
        ),
        (
            "csample",
            r"csample(?: |$)(.*)",
            "Generate a colour swatch: .csample <color>.",
        ),
        ("codeimg", "codeimg$", "Render replied code block as image."),
        (
            "textimg",
            r"textimg(?: |$)([\s\S]+)",
            "Render text as a styled quote card: .textimg <text>.",
        ),
        (
            "errcard",
            "errcard$",
            "Render last error as error card (owner-only).",
        ),
        (
            "statcard",
            "statcard$",
            "Render bot trace stats as card (owner-only).",
        ),
        (
            "imginfo",
            "imginfo$",
            "Show image info: size, mode, format (reply to image).",
        ),
    ];
    for (name, pattern, help) in cmds {
        let handler: fn(
            &Ctx,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
        > = match *name {
            "grey" | "grayscale" => |ctx| Box::pin(grey_cmd(ctx)),
            "invert" => |ctx| Box::pin(invert_cmd(ctx)),
            "blur" => |ctx| Box::pin(blur_cmd(ctx)),
            "sharpen" => |ctx| Box::pin(sharpen_cmd(ctx)),
            "rotate" => |ctx| Box::pin(rotate_cmd(ctx)),
            "flip" | "mirror" => |ctx| Box::pin(flip_cmd(ctx)),
            "flop" => |ctx| Box::pin(flop_cmd(ctx)),
            "sketch" => |ctx| Box::pin(sketch_cmd(ctx)),
            "cartoon" | "toon" => |ctx| Box::pin(cartoon_cmd(ctx)),
            "danger" => |ctx| Box::pin(danger_cmd(ctx)),
            "bluec" => |ctx| Box::pin(blue_cmd(ctx)),
            "vintage" => |ctx| Box::pin(vintage_cmd(ctx)),
            "glitch" => |ctx| Box::pin(glitch_cmd(ctx)),
            "emboss" => |ctx| Box::pin(emboss_cmd(ctx)),
            "edges" => |ctx| Box::pin(edges_cmd(ctx)),
            "pixelate" => |ctx| Box::pin(pixelate_cmd(ctx)),
            "vignette" => |ctx| Box::pin(vignette_cmd(ctx)),
            "brightness" => |ctx| Box::pin(brightness_cmd(ctx)),
            "contrast" => |ctx| Box::pin(contrast_cmd(ctx)),
            "saturation" => |ctx| Box::pin(saturation_cmd(ctx)),
            "caption" => |ctx| Box::pin(caption_cmd(ctx)),
            "meme" => |ctx| Box::pin(meme_cmd(ctx)),
            "watermark" => |ctx| Box::pin(watermark_cmd(ctx)),
            "border" => |ctx| Box::pin(border_cmd(ctx)),
            "csample" => |ctx| Box::pin(csample_cmd(ctx)),
            "codeimg" => |ctx| Box::pin(codeimg_cmd(ctx)),
            "textimg" => |ctx| Box::pin(textimg_cmd(ctx)),
            "errcard" => |ctx| Box::pin(errcard_cmd(ctx)),
            "statcard" => |ctx| Box::pin(statcard_cmd(ctx)),
            "imginfo" => |ctx| Box::pin(imginfo_cmd(ctx)),
            _ => unreachable!(),
        };
        let mut d = def(name, pattern, help);
        if *name == "errcard" || *name == "statcard" {
            d.owner_only = true;
        }
        reg_add(registry, d, handler);
    }
}

// ── shared helpers ────────────────────────────────────────────────────────

fn tmp_file(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-output-{}.{ext}", uuid::Uuid::new_v4()))
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
        let dest = tmp_file("png");
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        return Ok(Some(path));
    }
    Ok(None)
}

fn load(path: &Path) -> Result<DynamicImage> {
    core_img::load(path)
}

/// Run a transform over the resolved image and send the result back,
/// mirroring Python's `_send_image` caption
/// (`{ops} · {W}×{H} · {ms}ms`).
async fn transform(
    ctx: &Ctx,
    python_ref: &str,
    op: &str,
    f: impl FnOnce(&DynamicImage) -> DynamicImage,
) -> Result<()> {
    let Some(input) = resolve_input(ctx, python_ref).await? else {
        ctx.reply("Reply to a photo, sticker, or image file.")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply(&format!("`🎨 {op}…`")).await?;
    let started = std::time::Instant::now();
    let img = load(&input)?;
    let out_img = f(&img);
    send_image(ctx, msg, &out_img, op, started).await
}

async fn send_image(
    ctx: &Ctx,
    msg: i64,
    img: &DynamicImage,
    op: &str,
    started: std::time::Instant,
) -> Result<()> {
    let out = tmp_file("png");
    core_img::save(img, &out)?;
    let (w, h) = core_img::dimensions(img);
    let ms = started.elapsed().as_millis();
    ctx.delete(msg).await?;
    ctx.send_file(&out, &format!("`{op}` · `{w}×{h}` · `{ms}ms`"))
        .await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

/// First numeric token of the args (or the regex capture group 1).
fn num_arg(ctx: &Ctx, default: f64) -> f64 {
    if let Some(g) = ctx.captures.get(1) {
        if let Ok(v) = g.trim().parse::<f64>() {
            return v;
        }
    }
    ctx.args
        .split_whitespace()
        .find_map(|t| t.parse::<f64>().ok())
        .unwrap_or(default)
}

// ── pixel ops (pure `image`-crate implementations) ────────────────────────

fn per_pixel(img: &DynamicImage, f: impl Fn(u8, u8, u8, u8) -> [u8; 4]) -> DynamicImage {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = [px[0], px[1], px[2], px[3]];
        out.put_pixel(x, y, Rgba(f(r, g, b, a)));
    }
    DynamicImage::ImageRgba8(out)
}

fn clamp255(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

/// Pencil sketch: grayscale → invert → blur → colour-dodge blend.
fn sketch(img: &DynamicImage) -> DynamicImage {
    let grey = img.grayscale();
    let inv = {
        let mut g = grey.clone();
        g.invert();
        g
    };
    let blurred = inv.blur(8.0);
    let g = grey.to_luma8();
    let b = blurred.to_luma8();
    let (w, h) = g.dimensions();
    let mut out: ImageBuffer<image::Luma<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    for (x, y, px) in g.enumerate_pixels() {
        let base = px[0] as u32;
        let blur_px = b.get_pixel(x, y)[0] as u32;
        let v = if blur_px >= 255 {
            255
        } else {
            (base * 255 / (255 - blur_px)).min(255)
        };
        out.put_pixel(x, y, image::Luma([v as u8]));
    }
    DynamicImage::ImageLuma8(out)
}

fn sobel_edges(img: &DynamicImage) -> ImageBuffer<image::Luma<u8>, Vec<u8>> {
    let g = img.grayscale().to_luma8();
    let (w, h) = g.dimensions();
    let mut out: ImageBuffer<image::Luma<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    let gx = [[-1i32, 0, 1], [-2, 0, 2], [-1, 0, 1]];
    let gy = [[-1i32, -2, -1], [0, 0, 0], [1, 2, 1]];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let mut sx = 0i32;
            let mut sy = 0i32;
            for ky in 0..3 {
                for kx in 0..3 {
                    let p = g.get_pixel(x + kx as u32 - 1, y + ky as u32 - 1)[0] as i32;
                    sx += gx[ky][kx] * p;
                    sy += gy[ky][kx] * p;
                }
            }
            let mag = (((sx * sx + sy * sy) as f32).sqrt() as i32).min(255) as u8;
            out.put_pixel(x, y, image::Luma([mag]));
        }
    }
    out
}

fn cartoon(img: &DynamicImage) -> DynamicImage {
    // Cheap cartoon: strong blur + colour posterize, dark Sobel edges on top.
    let blurred = img.blur(2.0);
    let levels = 6.0f32;
    let poster = per_pixel(&blurred, |r, g, b, a| {
        let q = |v: u8| ((v as f32 / 255.0 * levels).round() / levels * 255.0) as u8;
        [q(r), q(g), q(b), a]
    });
    let edges = sobel_edges(img);
    let mut rgba = poster.to_rgba8();
    let (w, h) = rgba.dimensions();
    for (x, y, px) in rgba.enumerate_pixels_mut() {
        if edges.get_pixel(x, y)[0] > 90 {
            *px = Rgba([0, 0, 0, px[3]]);
        }
    }
    let _ = (w, h);
    DynamicImage::ImageRgba8(rgba)
}

fn convolve3(img: &DynamicImage, kernel: [[f32; 3]; 3], bias: f32) -> DynamicImage {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut out: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 3];
            for ky in 0..3 {
                for kx in 0..3 {
                    let sx = x as i32 + kx as i32 - 1;
                    let sy = y as i32 + ky as i32 - 1;
                    if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                        continue;
                    }
                    let p = rgb.get_pixel(sx as u32, sy as u32);
                    let k = kernel[ky][kx];
                    for c in 0..3 {
                        acc[c] += p[c] as f32 * k;
                    }
                }
            }
            out.put_pixel(
                x,
                y,
                Rgb([
                    clamp255(acc[0] + bias),
                    clamp255(acc[1] + bias),
                    clamp255(acc[2] + bias),
                ]),
            );
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Nearest-neighbour rotation by arbitrary degrees (the `image` crate
/// only rotates by 90° steps; Python's `Imaging.rotate` is arbitrary).
fn rotate_any(img: &DynamicImage, degrees: f64) -> DynamicImage {
    let (w, h) = img.dimensions();
    let rad = degrees.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    let nw = ((w as f64 * cos.abs() + h as f64 * sin.abs()).ceil() as u32).max(1);
    let nh = ((w as f64 * sin.abs() + h as f64 * cos.abs()).ceil() as u32).max(1);
    let src = img.to_rgba8();
    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(nw, nh);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let (ncx, ncy) = (nw as f64 / 2.0, nh as f64 / 2.0);
    for y in 0..nh {
        for x in 0..nw {
            let dx = x as f64 - ncx;
            let dy = y as f64 - ncy;
            let sx = (cos * dx + sin * dy + cx).round() as i64;
            let sy = (-sin * dx + cos * dy + cy).round() as i64;
            if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                out.put_pixel(x, y, *src.get_pixel(sx as u32, sy as u32));
            }
        }
    }
    DynamicImage::ImageRgba8(out)
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().to_lowercase();
    if let Some(hex) = s.strip_prefix('#').or_else(|| s.strip_prefix("0x")) {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some([r, g, b]);
        }
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            return Some([r, g, b]);
        }
        return None;
    }
    Some(match s.as_str() {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        "red" => [255, 0, 0],
        "green" => [0, 128, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "orange" => [255, 165, 0],
        "purple" => [128, 0, 128],
        "pink" => [255, 192, 203],
        "cyan" => [0, 255, 255],
        "magenta" => [255, 0, 255],
        "grey" | "gray" => [128, 128, 128],
        "brown" => [165, 42, 42],
        "lime" => [0, 255, 0],
        "navy" => [0, 0, 128],
        "teal" => [0, 128, 128],
        _ => return None,
    })
}

// ── basic transforms ──────────────────────────────────────────────────────

async fn grey_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:grey_cmd", "grayscale", |img| {
        core_img::grey(img)
    })
    .await
}

async fn invert_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:invert_cmd", "invert", |img| {
        core_img::invert(img)
    })
    .await
}

async fn blur_cmd(ctx: &Ctx) -> Result<()> {
    let radius = num_arg(ctx, 5.0) as f32;
    transform(
        ctx,
        "imaging.py:blur_cmd",
        &format!("blur r={radius}"),
        move |img| core_img::blur(img, radius),
    )
    .await
}

async fn sharpen_cmd(ctx: &Ctx) -> Result<()> {
    let factor = num_arg(ctx, 2.0);
    transform(
        ctx,
        "imaging.py:sharpen_cmd",
        &format!("sharpen f={factor}"),
        move |img| {
            // `image` unsharpen: sigma ~ factor, threshold 0.
            image::DynamicImage::ImageRgba8(image::imageops::unsharpen(img, factor as f32, 0))
        },
    )
    .await
}

async fn rotate_cmd(ctx: &Ctx) -> Result<()> {
    let degrees = num_arg(ctx, 90.0);
    let d = degrees;
    transform(
        ctx,
        "imaging.py:rotate_cmd",
        &format!("rotate {d}°"),
        move |img| {
            let norm = d.rem_euclid(360.0);
            if (norm - 90.0).abs() < f64::EPSILON
                || (norm - 180.0).abs() < f64::EPSILON
                || (norm - 270.0).abs() < f64::EPSILON
                || norm < f64::EPSILON
            {
                core_img::rotate(img, norm as i32)
            } else {
                rotate_any(img, d)
            }
        },
    )
    .await
}

async fn flip_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:flip_cmd", "flip", |img| {
        core_img::flip_horizontal(img)
    })
    .await
}

async fn flop_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:flop_cmd", "flop", |img| {
        core_img::flip_vertical(img)
    })
    .await
}

// ── effects ───────────────────────────────────────────────────────────────

async fn sketch_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:sketch_cmd", "sketch", sketch).await
}

async fn cartoon_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:cartoon_cmd", "cartoon", cartoon).await
}

async fn danger_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:danger_cmd", "danger", |img| {
        per_pixel(img, |r, g, b, a| {
            [
                clamp255(r as f32 * 1.5),
                clamp255(g as f32 * 0.8),
                clamp255(b as f32 * 0.8),
                a,
            ]
        })
    })
    .await
}

async fn blue_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:blue_cmd", "blue channel", |img| {
        per_pixel(img, |r, g, b, a| {
            [
                clamp255(r as f32 * 0.8),
                clamp255(g as f32 * 0.8),
                clamp255(b as f32 * 1.5),
                a,
            ]
        })
    })
    .await
}

async fn vintage_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:vintage_cmd", "vintage", |img| {
        per_pixel(img, |r, g, b, a| {
            let (r, g, b) = (r as f32, g as f32, b as f32);
            [
                clamp255(0.393 * r + 0.769 * g + 0.189 * b),
                clamp255(0.349 * r + 0.686 * g + 0.168 * b),
                clamp255(0.272 * r + 0.534 * g + 0.131 * b),
                a,
            ]
        })
    })
    .await
}

async fn glitch_cmd(ctx: &Ctx) -> Result<()> {
    let intensity = num_arg(ctx, 10.0).clamp(1.0, 50.0) as i32;
    transform(
        ctx,
        "imaging.py:glitch_cmd",
        &format!("glitch i={intensity}"),
        move |img| core_img::glitch(img, intensity),
    )
    .await
}

async fn emboss_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:emboss_cmd", "emboss", |img| {
        convolve3(
            img,
            [[-2.0, -1.0, 0.0], [-1.0, 1.0, 1.0], [0.0, 1.0, 2.0]],
            128.0,
        )
    })
    .await
}

async fn edges_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:edges_cmd", "edges", |img| {
        DynamicImage::ImageLuma8(sobel_edges(img))
    })
    .await
}

async fn pixelate_cmd(ctx: &Ctx) -> Result<()> {
    let size = num_arg(ctx, 20.0).clamp(2.0, 100.0) as u32;
    transform(
        ctx,
        "imaging.py:pixelate_cmd",
        &format!("pixelate s={size}"),
        move |img| {
            let (w, h) = img.dimensions();
            let sw = (w / size).max(1);
            let sh = (h / size).max(1);
            let small = img.resize_exact(sw, sh, image::imageops::FilterType::Nearest);
            small.resize_exact(w, h, image::imageops::FilterType::Nearest)
        },
    )
    .await
}

async fn vignette_cmd(ctx: &Ctx) -> Result<()> {
    transform(ctx, "imaging.py:vignette_cmd", "vignette", |img| {
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let max_d = ((cx * cx + cy * cy) as f32).sqrt().max(1.0);
        let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(w, h);
        for (x, y, px) in rgba.enumerate_pixels() {
            let d = (((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) as f32).sqrt();
            let f = 1.0 - 0.65 * (d / max_d).powi(2);
            out.put_pixel(
                x,
                y,
                Rgba([
                    clamp255(px[0] as f32 * f),
                    clamp255(px[1] as f32 * f),
                    clamp255(px[2] as f32 * f),
                    px[3],
                ]),
            );
        }
        DynamicImage::ImageRgba8(out)
    })
    .await
}

// ── adjustments ───────────────────────────────────────────────────────────

async fn brightness_cmd(ctx: &Ctx) -> Result<()> {
    let factor = num_arg(ctx, 1.5).clamp(0.1, 5.0);
    transform(
        ctx,
        "imaging.py:brightness_cmd",
        &format!("brightness {factor}"),
        move |img| {
            per_pixel(img, |r, g, b, a| {
                [
                    clamp255(r as f32 * factor as f32),
                    clamp255(g as f32 * factor as f32),
                    clamp255(b as f32 * factor as f32),
                    a,
                ]
            })
        },
    )
    .await
}

async fn contrast_cmd(ctx: &Ctx) -> Result<()> {
    let factor = num_arg(ctx, 1.5).clamp(0.1, 5.0);
    transform(
        ctx,
        "imaging.py:contrast_cmd",
        &format!("contrast {factor}"),
        move |img| {
            let mut out = img.clone();
            out = out.adjust_contrast(factor as f32);
            out
        },
    )
    .await
}

async fn saturation_cmd(ctx: &Ctx) -> Result<()> {
    let factor = num_arg(ctx, 1.5).clamp(0.0, 5.0);
    transform(
        ctx,
        "imaging.py:saturation_cmd",
        &format!("saturation {factor}"),
        move |img| {
            per_pixel(img, |r, g, b, a| {
                let grey = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as f32;
                let f = factor as f32;
                [
                    clamp255(grey + (r as f32 - grey) * f),
                    clamp255(grey + (g as f32 - grey) * f),
                    clamp255(grey + (b as f32 - grey) * f),
                    a,
                ]
            })
        },
    )
    .await
}

// ── composite / text ──────────────────────────────────────────────────────

/// Glyph drawing is the blocked step; the image is still loaded first so
/// a missing/invalid input fails the same way Python's loader would.
async fn text_blocked(ctx: &Ctx, python_ref: &str, usage: &str) -> Result<()> {
    let Some(input) = resolve_input(ctx, python_ref).await? else {
        ctx.reply(usage).await?;
        return Ok(());
    };
    let _img = load(&input)?;
    Err(Error::Other(format!(
        "{TEXT_DEP_NOTE} (Python: {python_ref})"
    )))
}

async fn caption_cmd(ctx: &Ctx) -> Result<()> {
    // `top:` / `bottom:` / `center:` position prefix, like Python.
    let text = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if text.is_empty() {
        ctx.reply("`Usage: .caption <text>`").await?;
        return Ok(());
    }
    let _position = ["top", "bottom", "center"]
        .iter()
        .find(|p| text.to_lowercase().starts_with(&format!("{p}:")))
        .copied()
        .unwrap_or("bottom");
    text_blocked(ctx, "imaging.py:caption_cmd", "`Usage: .caption <text>`").await
}

async fn meme_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let parts: Vec<&str> = arg.splitn(2, '|').collect();
    let (top, bot) = (
        parts.first().map(|s| s.trim()).unwrap_or(""),
        parts.get(1).map(|s| s.trim()).unwrap_or(""),
    );
    if top.is_empty() && bot.is_empty() {
        ctx.reply("`Usage: .meme top text | bottom text`").await?;
        return Ok(());
    }
    text_blocked(
        ctx,
        "imaging.py:meme_cmd",
        "`Usage: .meme top text | bottom text`",
    )
    .await
}

async fn watermark_cmd(ctx: &Ctx) -> Result<()> {
    let text = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if text.is_empty() {
        ctx.reply("`Usage: .watermark <text>`").await?;
        return Ok(());
    }
    text_blocked(
        ctx,
        "imaging.py:watermark_cmd",
        "`Usage: .watermark <text>`",
    )
    .await
}

async fn border_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let parts: Vec<&str> = arg.split_whitespace().collect();
    let color = parts.first().copied().unwrap_or("white");
    let width: u32 = parts
        .get(1)
        .and_then(|w| w.parse::<u32>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let Some(rgb) = parse_color(color) else {
        ctx.reply(&format!("Unknown colour: `{color}`")).await?;
        return Ok(());
    };
    let Some(input) = resolve_input(ctx, "imaging.py:border_cmd").await? else {
        ctx.reply("Reply to a photo, sticker, or image file.")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply("`🖼 adding border…`").await?;
    let started = std::time::Instant::now();
    let img = load(&input)?;
    let (w, h) = img.dimensions();
    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(w + 2 * width, h + 2 * width, |_, _| {
            Rgba([rgb[0], rgb[1], rgb[2], 255])
        });
    image::imageops::overlay(&mut out, &img.to_rgba8(), width as i64, width as i64);
    send_image(ctx, msg, &DynamicImage::ImageRgba8(out), "border", started).await
}

// ── generate ──────────────────────────────────────────────────────────────

async fn csample_cmd(ctx: &Ctx) -> Result<()> {
    let color = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if color.is_empty() {
        ctx.reply("`Usage: .csample red` or `.csample #ff6600`")
            .await?;
        return Ok(());
    }
    let Some(rgb) = parse_color(&color) else {
        ctx.reply(&format!("❌ Unknown colour: `{color}`")).await?;
        return Ok(());
    };
    let msg = ctx.reply(&format!("`🎨 sampling {color}…`")).await?;
    let started = std::time::Instant::now();
    let card = core_img::error_card(512, 512, rgb);
    send_image(ctx, msg, &card, &format!("colour sample {color}"), started).await
}

async fn codeimg_cmd(ctx: &Ctx) -> Result<()> {
    let Some(code) = ctx.reply_to_text.clone().filter(|t| !t.trim().is_empty()) else {
        ctx.reply("`Reply to a message containing code.`").await?;
        return Ok(());
    };
    let _code = code.trim().trim_matches('`');
    // Language detection mirrors Python (python hint only).
    let _lang =
        if _code.starts_with("python") || _code.contains("def ") || _code.contains("import ") {
            "python"
        } else {
            ""
        };
    Err(Error::Other(format!(
        "{TEXT_DEP_NOTE} (Python: imaging.py:codeimg_cmd)"
    )))
}

async fn textimg_cmd(ctx: &Ctx) -> Result<()> {
    let text = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if text.is_empty() {
        ctx.reply("`Usage: .textimg <your text>`").await?;
        return Ok(());
    }
    Err(Error::Other(format!(
        "{TEXT_DEP_NOTE} (Python: imaging.py:textimg_cmd)"
    )))
}

async fn errcard_cmd(ctx: &Ctx) -> Result<()> {
    // Python reads `pyUltroid.tracer.tr.live.failures(n=1)`. The Rust
    // core tracer (`ultroid_core::tracer`) is a source-code searcher, not
    // a live failure store, and no live trace store is exposed through Ctx.
    let _ = ctx;
    Err(Error::Other(
        "live trace failure store is not exposed through Ctx (Python: imaging.py:errcard_cmd via pyUltroid.tracer.tr.live.failures)".into(),
    ))
}

async fn statcard_cmd(ctx: &Ctx) -> Result<()> {
    let _ = ctx;
    Err(Error::Other(
        "live trace stats store is not exposed through Ctx (Python: imaging.py:statcard_cmd via pyUltroid.tracer.tr.live.stats)".into(),
    ))
}

// ── info ──────────────────────────────────────────────────────────────────

async fn imginfo_cmd(ctx: &Ctx) -> Result<()> {
    let Some(input) = resolve_input(ctx, "imaging.py:imginfo_cmd").await? else {
        ctx.reply("Reply to a photo, sticker, or image file.")
            .await?;
        return Ok(());
    };
    let msg = ctx.reply("`🔍 reading image…`").await?;
    let started = std::time::Instant::now();
    let img = load(&input)?;
    let (w, h) = img.dimensions();
    let format = image::ImageReader::open(&input)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.format())
        .map(|f| format!("{f:?}"))
        .unwrap_or_else(|| "unknown".to_string());
    let mode = format!("{:?}", img.color());
    ctx.edit(
        msg,
        &format!(
            "**🖼 Image Info**\n\nSize:    `{w} × {h}`\nMode:    `{mode}`\nFormat:  `{format}`\nLoaded in: `{:.0}ms`",
            started.elapsed().as_millis()
        ),
    )
    .await?;
    Ok(())
}
