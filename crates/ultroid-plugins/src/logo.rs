//! Logo maker — port of `plugins/logo.py` (1,225 LOC).
//!
//! * `.logo <text>` — generate a logo (gradient background + text).
//! * `.logograd <text> [preset 0-5]` / `.logogtext` / `.logocurve` /
//!   `.logomulti <l1 | l2 | l3>` — creative tools.
//! * `.logotext` / `.logoresize` / `.logobg` / `.logoborder` /
//!   `.logoround` / `.logoshadow` — text-command equivalents of the
//!   button-menu operations.
//!
//! Text rendering: the workspace has no font-rasterizer crate, so glyphs
//! are rendered with ffmpeg's `drawtext` filter as white-on-black masks
//! (luminance → alpha in the `image` crate). ffmpeg is already a runtime
//! dependency of the project. A mask is rendered twice per text — once
//! with `borderw=0` (body) and once with the stroke width (ring = stroke
//! mask minus body mask) — which reproduces PIL's
//! `draw.text(..., stroke_width, stroke_fill)` structure. All pixel
//! compositing (gradient, round corners, border, shadow, arc layout) is
//! done with the `image` crate plus a small bilinear rotator below.
//!
//! The button menu / callbacks (`lg_*`) and the inline "add text"
//! (`logotext_<sid> …`) live on the assistant bot: the binary wires them
//! to [`logo_main_menu`], [`logo_submenu`], [`logo_apply_op`] and
//! [`logo_add_text`]. Delivery here uses the user client
//! (`ctx.send_file`) plus the text-command hint — the Python bot-first /
//! inline fallbacks need the assistant and are reported as a
//! `TODO(responder-op)` in the parent report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::util::{def, reg_add};
use image::{GenericImageView, Rgb, Rgba, RgbaImage};
use tokio::process::Command as AsyncCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available -\n\n• `{i}logo <text>`\n   Generate a logo of the given Text.\n   Or Reply To image, to write ur text on it.\n   Or Reply To Font File, To write with that font.\n\n• `{i}logograd <text> [preset 0-5]`\n   Gradient-background logo (creative tool).\n\n• `{i}logogtext <text> [preset 0-5]`\n   Logo with gradient-filled text (creative tool).\n\n• `{i}logocurve <text>`\n   Logo with the text laid out along a circular arc (creative tool).\n\n• `{i}logomulti <line1 | line2 | line3>`\n   Multi-layered logo — stacked text layers of decreasing size.\n\n• `{i}logotext <text>`\n   Add text onto a replied logo/image (image-to-logo).\n\n• `{i}logoresize <512|1024|2x|0.5x|WxH>`\n• `{i}logobg <color>`\n• `{i}logoborder [width]`\n• `{i}logoround [radius]`\n• `{i}logoshadow`\n   Text-command equivalents of the button-menu operations.";

const DEFAULT_SIZE: (u32, u32) = (1024, 1024);
const ROUND_RADIUS: u32 = 90;

const BG_COLORS: [(&str, [u8; 3]); 8] = [
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [220, 53, 69]),
    ("blue", [0, 123, 255]),
    ("green", [40, 167, 69]),
    ("purple", [138, 43, 226]),
    ("orange", [255, 165, 0]),
    ("navy", [16, 28, 58]),
];

/// Gradient presets (start, end) — 0 sunset, 1 purple, 2 ocean,
/// 3 neon dark, 4 candy, 5 mint.
const GRADIENTS: [([u8; 3], [u8; 3]); 6] = [
    ([255, 94, 98], [255, 175, 189]),
    ([72, 52, 212], [168, 85, 247]),
    ([0, 198, 255], [0, 114, 255]),
    ([43, 43, 43], [162, 0, 255]),
    ([240, 147, 251], [245, 87, 108]),
    ([0, 255, 136], [0, 196, 180]),
];

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("logo", "logo( (.*)|$)", HELP), |ctx| {
        Box::pin(logo_gen(ctx))
    });
    reg_add(
        registry,
        def("logograd", "logograd( (.*)|$)", HELP),
        |ctx| Box::pin(logo_grad(ctx)),
    );
    reg_add(
        registry,
        def("logogtext", "logogtext( (.*)|$)", HELP),
        |ctx| Box::pin(logo_gtext(ctx)),
    );
    reg_add(
        registry,
        def("logocurve", "logocurve( (.*)|$)", HELP),
        |ctx| Box::pin(logo_curve(ctx)),
    );
    reg_add(
        registry,
        def("logomulti", "logomulti( (.*)|$)", HELP),
        |ctx| Box::pin(logo_multi(ctx)),
    );
    reg_add(
        registry,
        def("logotext", "logotext( (.*)|$)", HELP),
        |ctx| Box::pin(logo_text(ctx)),
    );
    reg_add(
        registry,
        def("logoresize", "logoresize( (.*)|$)", HELP),
        |ctx| Box::pin(logo_resize(ctx)),
    );
    reg_add(registry, def("logobg", "logobg( (.*)|$)", HELP), |ctx| {
        Box::pin(logo_bg(ctx))
    });
    reg_add(
        registry,
        def("logoborder", "logoborder( (.*)|$)", HELP),
        |ctx| Box::pin(logo_border(ctx)),
    );
    reg_add(
        registry,
        def("logoround", "logoround( (.*)|$)", HELP),
        |ctx| Box::pin(logo_round(ctx)),
    );
    reg_add(
        registry,
        def("logoshadow", "logoshadow( (.*)|$)", HELP),
        |ctx| Box::pin(logo_shadow(ctx)),
    );
}

// ── small utilities ───────────────────────────────────────────────────────

fn lerp(c0: [u8; 3], c1: [u8; 3], t: f64) -> [u8; 3] {
    [
        (c0[0] as f64 + (c1[0] as f64 - c0[0] as f64) * t).round() as u8,
        (c0[1] as f64 + (c1[1] as f64 - c0[1] as f64) * t).round() as u8,
        (c0[2] as f64 + (c1[2] as f64 - c0[2] as f64) * t).round() as u8,
    ]
}

fn random_font() -> Option<PathBuf> {
    let dir = Path::new("resources/fonts");
    let mut fonts = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_file() {
                fonts.push(e.path());
            }
        }
    }
    if fonts.is_empty() {
        return None;
    }
    let idx = (rand::random::<u64>() as usize) % fonts.len();
    Some(fonts[idx].clone())
}

fn check_filename(name: &str) -> PathBuf {
    let p = PathBuf::from(name);
    if !p.exists() {
        return p;
    }
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut no = 1u32;
    loop {
        let cand = if ext.is_empty() {
            PathBuf::from(format!("{stem}_{no}"))
        } else {
            PathBuf::from(format!("{stem}_{no}.{ext}"))
        };
        if !cand.exists() {
            return cand;
        }
        no += 1;
    }
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn color_by_name(name: &str, default: [u8; 3]) -> [u8; 3] {
    BG_COLORS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, c)| *c)
        .unwrap_or(default)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(prefix: &str, ext: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{n}.{ext}", std::process::id()))
}

fn ff_escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(':', "\\:")
}

// ── ffmpeg text masks ─────────────────────────────────────────────────────

/// Render `text` with ffmpeg drawtext as a white-on-black RGB mask of
/// `canvas` size, text at the top-left. `stroke` is the drawtext border
/// width (white outline, included in the mask).
async fn render_mask(
    text: &str,
    font: &Path,
    size: u32,
    stroke: u32,
    canvas: (u32, u32),
) -> Result<image::RgbImage> {
    let txt = temp_path("logo_text", "txt");
    // drawtext expands `%{...}`; `%%` is the literal-percent escape.
    tokio::fs::write(&txt, text.replace('%', "%%")).await?;
    let out = temp_path("logo_mask", "png");
    let filter = format!(
        "drawtext=fontfile='{}':textfile='{}':fontsize={}:fontcolor=white:borderw={}:bordercolor=white:x=0:y=0,format=rgb24",
        ff_escape(&font.to_string_lossy()),
        ff_escape(&txt.to_string_lossy()),
        size,
        stroke
    );
    let status = AsyncCommand::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:s={}x{}", canvas.0.max(1), canvas.1.max(1)),
            "-vf",
            &filter,
            "-frames:v",
            "1",
        ])
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await?;
    let _ = tokio::fs::remove_file(&txt).await;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg drawtext failed (exit {status})"
        )));
    }
    let img = image::open(&out)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgb8();
    let _ = tokio::fs::remove_file(&out).await;
    Ok(img)
}

/// Bounding box of bright pixels in a mask: `(x0, y0, x1, y1)` inclusive.
fn mask_bbox(mask: &image::RgbImage) -> Option<(i64, i64, i64, i64)> {
    let (w, h) = mask.dimensions();
    let mut x0 = w as i64;
    let mut y0 = h as i64;
    let mut x1 = -1i64;
    let mut y1 = -1i64;
    for (x, y, p) in mask.enumerate_pixels() {
        if p[0] > 40 {
            let (x, y) = (x as i64, y as i64);
            if x < x0 {
                x0 = x;
            }
            if y < y0 {
                y0 = y;
            }
            if x > x1 {
                x1 = x;
            }
            if y > y1 {
                y1 = y;
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        None
    } else {
        Some((x0, y0, x1, y1))
    }
}

/// Port of `LogoHelper.find_font_size`: render at size 100, measure the
/// observed width, scale to `target_width_ratio` of the image width.
/// Returns `(font_size, observed_width_at_100)`.
async fn find_font_size(
    text: &str,
    font: &Path,
    image_width: u32,
    target_width_ratio: f64,
) -> Result<(u32, u32)> {
    let mask = render_mask(text, font, 100, 0, (8000, 500)).await?;
    let Some((x0, _, x1, _)) = mask_bbox(&mask) else {
        return Ok((100, 0));
    };
    let observed = (x1 - x0 + 1) as f64;
    if observed <= 0.0 || image_width == 0 {
        return Ok((100, 0));
    }
    let estimated = (100.0 / (observed / image_width as f64) * target_width_ratio).round() as i64;
    Ok((estimated.max(1) as u32, observed as u32))
}

/// Canvas big enough to hold `text` at `size` (from the size-100 measure).
fn text_canvas(size: u32, width_at_100: u32, stroke: u32) -> (u32, u32) {
    let w = if width_at_100 > 0 {
        ((width_at_100 as f64 / 100.0) * size as f64) as u32 + 2 * stroke + 100
    } else {
        4000
    };
    (w.max(100), size * 2 + 200)
}

// ── compositing primitives ────────────────────────────────────────────────

/// Composite a white-on-black mask onto `base` with solid `fill`, at offset
/// `(dx, dy)`; mask luminance becomes alpha. When `subtract` is given, the
/// ring (mask minus subtract) is used instead — the stroke outline.
fn composite_mask(
    base: &mut RgbaImage,
    mask: &image::RgbImage,
    fill: [u8; 3],
    dx: i64,
    dy: i64,
    subtract: Option<&image::RgbImage>,
) {
    let (bw, bh) = base.dimensions();
    for (x, y, p) in mask.enumerate_pixels() {
        let mut a = p[0] as f32 / 255.0;
        if let Some(sub) = subtract {
            let s = sub.get_pixel(x, y)[0] as f32 / 255.0;
            a = (a - s).max(0.0);
        }
        if a <= 0.0 {
            continue;
        }
        let bx = x as i64 + dx;
        let by = y as i64 + dy;
        if bx < 0 || by < 0 || bx >= bw as i64 || by >= bh as i64 {
            continue;
        }
        let px = base.get_pixel_mut(bx as u32, by as u32);
        for i in 0..3 {
            px[i] = ((fill[i] as f32 * a) + (px[i] as f32 * (1.0 - a))).round() as u8;
        }
        px[3] = 255;
    }
}

/// Alpha-"over" composite of an RGBA layer onto `base` at `(dx, dy)`.
fn composite_rgba(base: &mut RgbaImage, layer: &RgbaImage, dx: i64, dy: i64) {
    let (bw, bh) = base.dimensions();
    for (x, y, p) in layer.enumerate_pixels() {
        let sa = p[3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let bx = x as i64 + dx;
        let by = y as i64 + dy;
        if bx < 0 || by < 0 || bx >= bw as i64 || by >= bh as i64 {
            continue;
        }
        let dst = base.get_pixel_mut(bx as u32, by as u32);
        for i in 0..3 {
            dst[i] = ((p[i] as f32 * sa) + (dst[i] as f32 * (1.0 - sa))).round() as u8;
        }
        dst[3] = (255.0 * (sa + dst[3] as f32 / 255.0 * (1.0 - sa))).round() as u8;
    }
}

/// Bilinear rotation of an RGBA image by `angle_deg` (expand canvas).
fn rotate_rgba(img: &RgbaImage, angle_deg: f64) -> RgbaImage {
    let (w, h) = img.dimensions();
    let rad = angle_deg.to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    let nw = ((w as f64 * cos.abs()) + (h as f64 * sin.abs())).ceil() as u32;
    let nh = ((w as f64 * sin.abs()) + (h as f64 * cos.abs())).ceil() as u32;
    let (nw, nh) = (nw.max(1), nh.max(1));
    let mut out = RgbaImage::new(nw, nh);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let (ncx, ncy) = (nw as f64 / 2.0, nh as f64 / 2.0);
    for y in 0..nh {
        for x in 0..nw {
            let dx = x as f64 - ncx;
            let dy = y as f64 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f64 || sy > (h - 1) as f64 {
                continue;
            }
            let x0 = sx.floor() as u32;
            let y0 = sy.floor() as u32;
            let x1 = (x0 + 1).min(w - 1);
            let y1 = (y0 + 1).min(h - 1);
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            let mut px = [0f32; 4];
            for (xx, yy, wt) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                (x1, y0, fx * (1.0 - fy)),
                (x0, y1, (1.0 - fx) * fy),
                (x1, y1, fx * fy),
            ] {
                let s = img.get_pixel(xx, yy);
                for c in 0..4 {
                    px[c] += s[c] as f32 * wt as f32;
                }
            }
            out.put_pixel(
                x,
                y,
                Rgba([px[0] as u8, px[1] as u8, px[2] as u8, px[3] as u8]),
            );
        }
    }
    out
}

// ── backgrounds ───────────────────────────────────────────────────────────

/// Gradient image (Python `_gradient_image`): a 128 px strip resized
/// bilinear, or a 128×128 diagonal field resized bilinear.
fn gradient_image(w: u32, h: u32, colors: ([u8; 3], [u8; 3]), direction: &str) -> image::RgbImage {
    const BASE: u32 = 128;
    let (c0, c1) = colors;
    let small: image::RgbImage = match direction {
        "vertical" => {
            let mut strip = image::RgbImage::new(1, BASE);
            for i in 0..BASE {
                strip.put_pixel(0, i, Rgb(lerp(c0, c1, i as f64 / (BASE - 1) as f64)));
            }
            strip
        }
        "horizontal" => {
            let mut strip = image::RgbImage::new(BASE, 1);
            for i in 0..BASE {
                strip.put_pixel(i, 0, Rgb(lerp(c0, c1, i as f64 / (BASE - 1) as f64)));
            }
            strip
        }
        _ => {
            let mut img = image::RgbImage::new(BASE, BASE);
            for y in 0..BASE {
                for x in 0..BASE {
                    img.put_pixel(
                        x,
                        y,
                        Rgb(lerp(c0, c1, (x + y) as f64 / (2.0 * (BASE - 1) as f64))),
                    );
                }
            }
            img
        }
    };
    image::imageops::resize(
        &small,
        w.max(1),
        h.max(1),
        image::imageops::FilterType::Triangle,
    )
}

fn make_gradient_bg(
    out: &Path,
    size: (u32, u32),
    colors: Option<([u8; 3], [u8; 3])>,
) -> Result<()> {
    let colors =
        colors.unwrap_or_else(|| GRADIENTS[(rand::random::<u64>() as usize) % GRADIENTS.len()]);
    let img = gradient_image(size.0, size.1, colors, "diagonal");
    img.save(out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(())
}

fn make_solid_bg(out: &Path, size: (u32, u32), color: [u8; 3]) -> Result<()> {
    let img = image::RgbImage::from_pixel(size.0, size.1, Rgb(color));
    img.save(out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(())
}

// ── text renderers ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct LogoStyle {
    pub fill: [u8; 3],
    pub stroke_width: u32,
    pub stroke_fill: [u8; 3],
}

impl Default for LogoStyle {
    fn default() -> Self {
        LogoStyle {
            fill: [255, 255, 255],
            stroke_width: 5,
            stroke_fill: [0, 0, 0],
        }
    }
}

/// Center a mask's bbox on the base: returns the (dx, dy) offset to pass
/// to `composite_mask` so the body bbox lands centered.
fn center_offset(base_w: u32, base_h: u32, bbox: (i64, i64, i64, i64)) -> (i64, i64) {
    let bw = bbox.2 - bbox.0 + 1;
    let bh = bbox.3 - bbox.1 + 1;
    (
        (base_w as i64 - bw) / 2 - bbox.0,
        (base_h as i64 - bh) / 2 - bbox.1,
    )
}

/// Port of `LogoHelper.make_logo`: centered text with stroke on `bgpath`.
pub async fn make_logo(
    bgpath: &Path,
    text: &str,
    font: &Path,
    style: LogoStyle,
) -> Result<PathBuf> {
    let mut img = image::open(bgpath)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let (size, w100) = find_font_size(text, font, w, 0.7).await?;
    let canvas = text_canvas(size, w100, style.stroke_width);
    let body = render_mask(text, font, size, 0, canvas).await?;
    let stroke = render_mask(text, font, size, style.stroke_width, canvas).await?;
    let bbox = mask_bbox(&body).ok_or_else(|| Error::Other("text rendered empty".to_string()))?;
    let (dx, dy) = center_offset(w, h, bbox);
    if style.stroke_width > 0 {
        composite_mask(&mut img, &stroke, style.stroke_fill, dx, dy, Some(&body));
    }
    composite_mask(&mut img, &body, style.fill, dx, dy, None);
    let out = check_filename("logo.png");
    img.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

/// Gradient-filled text on a solid background (Python `_gradient_text_logo`).
async fn gradient_text_logo(
    bgpath: &Path,
    text: &str,
    font: &Path,
    grad_colors: ([u8; 3], [u8; 3]),
    stroke_width: u32,
    stroke_fill: [u8; 3],
) -> Result<PathBuf> {
    let mut base = image::open(bgpath)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = base.dimensions();
    let (size, w100) = find_font_size(text, font, w, 0.7).await?;
    let canvas = text_canvas(size, w100, stroke_width);
    let body = render_mask(text, font, size, 0, canvas).await?;
    let stroke = render_mask(text, font, size, stroke_width, canvas).await?;
    let bbox = mask_bbox(&body).ok_or_else(|| Error::Other("text rendered empty".to_string()))?;
    let (dx, dy) = center_offset(w, h, bbox);
    if stroke_width > 0 {
        composite_mask(&mut base, &stroke, stroke_fill, dx, dy, Some(&body));
    }
    // Gradient body through the text-shaped alpha mask.
    let grad = gradient_image(w, h, grad_colors, "diagonal");
    for (x, y, p) in body.enumerate_pixels() {
        let a = p[0] as f32 / 255.0;
        if a <= 0.0 {
            continue;
        }
        let bx = x as i64 + dx;
        let by = y as i64 + dy;
        if bx < 0 || by < 0 || bx >= w as i64 || by >= h as i64 {
            continue;
        }
        let g = grad.get_pixel(bx as u32, by as u32);
        let px = base.get_pixel_mut(bx as u32, by as u32);
        for i in 0..3 {
            px[i] = ((g[i] as f32 * a) + (px[i] as f32 * (1.0 - a))).round() as u8;
        }
        px[3] = 255;
    }
    let out = check_filename("logo.png");
    base.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

/// Text laid out along a circular arc (Python `_curve_text_logo`).
async fn curve_text_logo(
    bgpath: &Path,
    text: &str,
    font: &Path,
    fill: [u8; 3],
    stroke_width: u32,
    stroke_fill: [u8; 3],
) -> Result<PathBuf> {
    let mut base = image::open(bgpath)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = base.dimensions();
    let font_size = (40u32).max((w.min(h) as f64 * 0.12) as u32);
    let chars: Vec<char> = text.chars().collect();
    let chars: Vec<char> = if chars.is_empty() { vec!['?'] } else { chars };
    let spread = (180f64).min((30f64).max(12.0 * chars.len() as f64));
    let radius = (w.min(h) as f64 * 0.32) as i64;
    let (cx, cy) = (w as i64 / 2, (h as f64 * 0.62) as i64);
    let angles: Vec<f64> = if chars.len() == 1 {
        vec![0.0]
    } else {
        let step = spread / (chars.len() - 1) as f64;
        (0..chars.len())
            .map(|i| -spread / 2.0 + step * i as f64)
            .collect()
    };
    for (ch, ang) in chars.iter().zip(angles.iter()) {
        let s: String = [*ch].iter().collect();
        let canvas = text_canvas(font_size, 0, stroke_width);
        let body = render_mask(&s, font, font_size, 0, canvas).await?;
        let stroke = render_mask(&s, font, font_size, stroke_width, canvas).await?;
        let Some(bbox) = mask_bbox(&stroke) else {
            continue;
        };
        // Build the char cell (bbox-sized RGBA): white body, black ring.
        let (cw, chh) = ((bbox.2 - bbox.0 + 1) as u32, (bbox.3 - bbox.1 + 1) as u32);
        let mut cell = RgbaImage::new(cw, chh);
        // Index loop, not enumerate_pixels: put_pixel needs &mut cell.
        for y in 0..chh {
            for x in 0..cw {
                let mx = (x as i64 + bbox.0) as u32;
                let my = (y as i64 + bbox.1) as u32;
                let ba = body.get_pixel(mx, my)[0] as f32 / 255.0;
                let sa = stroke.get_pixel(mx, my)[0] as f32 / 255.0;
                let ring = (sa - ba).max(0.0);
                let (r, g, b, a) = if ba > 0.0 {
                    (fill[0], fill[1], fill[2], ba)
                } else if ring > 0.0 {
                    (stroke_fill[0], stroke_fill[1], stroke_fill[2], ring)
                } else {
                    (0, 0, 0, 0.0)
                };
                cell.put_pixel(x, y, Rgba([r, g, b, (a * 255.0).round() as u8]));
            }
        }
        let cell = rotate_rgba(&cell, *ang);
        let rad = (ang - 90.0).to_radians();
        let px = cx + (radius as f64 * rad.cos()) as i64 - cell.width() as i64 / 2;
        let py = cy + (radius as f64 * rad.sin()) as i64 - cell.height() as i64 / 2;
        composite_rgba(&mut base, &cell, px, py);
    }
    let out = check_filename("logo.png");
    base.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

/// Stacked multi-line logo (Python `_multilayer_logo`).
async fn multilayer_logo(
    bgpath: &Path,
    text: &str,
    font: &Path,
    fill: [u8; 3],
    stroke_fill: [u8; 3],
) -> Result<PathBuf> {
    let mut base = image::open(bgpath)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = base.dimensions();
    let lines: Vec<String> = text
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let lines: Vec<String> = if lines.is_empty() {
        vec![text.to_string()]
    } else {
        lines
    };

    struct Layer {
        body: image::RgbImage,
        stroke: image::RgbImage,
        width: i64,
        height: i64,
    }
    let mut layers: Vec<(Layer, u32)> = Vec::new();
    let mut total_h: i64 = 0;
    for (i, ln) in lines.iter().enumerate() {
        let ratio = (0.25f64).max(0.72 - i as f64 * 0.16);
        let (size, w100) = find_font_size(ln, font, w, ratio).await?;
        let canvas = text_canvas(size, w100, 8);
        let body = render_mask(ln, font, size, 0, canvas).await?;
        let stroke = render_mask(ln, font, size, 8, canvas).await?;
        let bbox = mask_bbox(&body).unwrap_or((0, 0, 0, 0));
        let th = bbox.3 - bbox.1 + 1;
        layers.push((
            Layer {
                body,
                stroke,
                width: bbox.2 - bbox.0 + 1,
                height: th,
            },
            if ln.chars().count() <= 8 { 8 } else { 4 },
        ));
        total_h += th + 12;
    }
    let mut y = (h as i64 - total_h) / 2;
    for (layer, sw) in layers {
        let bbox = mask_bbox(&layer.body).unwrap_or((0, 0, layer.width - 1, layer.height - 1));
        let x = (w as i64 - layer.width) / 2 - bbox.0;
        let dy = y - bbox.1;
        if sw > 0 {
            composite_mask(
                &mut base,
                &layer.stroke,
                stroke_fill,
                x,
                dy,
                Some(&layer.body),
            );
        }
        composite_mask(&mut base, &layer.body, fill, x, dy, None);
        y += layer.height + 12;
    }
    let out = check_filename("logo.png");
    base.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

/// Overlay text on an existing image (Python `_add_text_to_image`).
async fn add_text_to_image(
    path: &Path,
    text: &str,
    font: &Path,
    fill: [u8; 3],
    stroke_width: u32,
    stroke_fill: [u8; 3],
) -> Result<PathBuf> {
    make_logo(
        path,
        text,
        font,
        LogoStyle {
            fill,
            stroke_width,
            stroke_fill,
        },
    )
    .await
}

// ── modification transforms ───────────────────────────────────────────────

fn resize_logo(path: &Path, size: Option<(u32, u32)>, scale: Option<f64>) -> Result<PathBuf> {
    let img = image::open(path).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let (w, h) = img.dimensions();
    let (nw, nh) = match scale {
        Some(s) => (
            ((w as f64 * s) as u32).max(1),
            ((h as f64 * s) as u32).max(1),
        ),
        None => size.unwrap_or((w, h)),
    };
    let out_img = img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3);
    let out = check_filename("logo.png");
    out_img
        .save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

fn round_corners(path: &Path, radius: u32) -> Result<PathBuf> {
    let mut img = image::open(path)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let r = radius.min(w / 2).min(h / 2) as i64;
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (x, y) = (x as i64, y as i64);
        // Inside the rounded rect iff not in a corner circle's outside.
        let (cx, cy) = if x < r && y < r {
            (r, r)
        } else if x >= w as i64 - r && y < r {
            (w as i64 - r - 1, r)
        } else if x < r && y >= h as i64 - r {
            (r, h as i64 - r - 1)
        } else if x >= w as i64 - r && y >= h as i64 - r {
            (w as i64 - r - 1, h as i64 - r - 1)
        } else {
            continue;
        };
        let dx = x - cx;
        let dy = y - cy;
        if dx * dx + dy * dy > r * r {
            p[3] = 0;
        }
    }
    let out = check_filename("logo.png");
    img.save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

fn add_border(path: &Path, width: u32, color: [u8; 3]) -> Result<PathBuf> {
    let img = image::open(path)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let mut out = RgbaImage::from_pixel(
        w + 2 * width,
        h + 2 * width,
        Rgba([color[0], color[1], color[2], 255]),
    );
    // Python: ImageOps.expand on RGB then save — the border is opaque even
    // for RGBA inputs (the expanded canvas is RGB). Keep the PNG save.
    image::imageops::overlay(&mut out, &img, width as i64, width as i64);
    let out_path = check_filename("logo.png");
    out.save(&out_path)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out_path)
}

fn add_shadow(path: &Path, offset: (i64, i64), blur: f32, opacity: u8) -> Result<PathBuf> {
    let img = image::open(path)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let pad = (blur as i64) * 2 + offset.0.abs() + offset.1.abs() + 8;
    let (cw, ch) = (w as i64 + 2 * pad, h as i64 + 2 * pad);
    let mut shadow = RgbaImage::new(cw as u32, ch as u32);
    for (x, y, p) in img.enumerate_pixels() {
        let a = p[3] as f32 / 255.0 * opacity as f32 / 255.0;
        if a > 0.0 {
            shadow.put_pixel(
                (x as i64 + pad) as u32,
                (y as i64 + pad) as u32,
                Rgba([0, 0, 0, (a * 255.0).round() as u8]),
            );
        }
    }
    // blur() already returns an Rgba8 ImageBuffer; no to_rgba8() exists on it.
    let blurred = image::imageops::blur(&image::DynamicImage::ImageRgba8(shadow), blur);
    let mut canvas = RgbaImage::new(cw as u32, ch as u32);
    composite_rgba(&mut canvas, &blurred, offset.0, offset.1);
    composite_rgba(&mut canvas, &img, pad, pad);
    let out = check_filename("logo.png");
    canvas
        .save(&out)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(out)
}

// ── sessions + menus ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LogoSession {
    pub path: PathBuf,
    pub chat: i64,
    pub user: i64,
    pub name: Option<String>,
    pub font: Option<PathBuf>,
    pub style: LogoStyle,
    pub size: (u32, u32),
}

static SESSIONS: LazyLock<Mutex<HashMap<String, LogoSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn new_session(
    path: PathBuf,
    chat: i64,
    user: i64,
    name: Option<String>,
    font: Option<PathBuf>,
    style: LogoStyle,
    size: (u32, u32),
) -> String {
    let sid = uuid::Uuid::new_v4().simple().to_string()[..6].to_string();
    if let Ok(mut sessions) = SESSIONS.lock() {
        // Retire a previous session for this chat/user (Python `_new_session`).
        let old: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.chat == chat && s.user == user)
            .map(|(k, _)| k.clone())
            .collect();
        for k in old {
            if let Some(s) = sessions.remove(&k) {
                if s.path != path {
                    cleanup(&s.path);
                }
            }
        }
        sessions.insert(
            sid.clone(),
            LogoSession {
                path,
                chat,
                user,
                name,
                font,
                style,
                size,
            },
        );
    }
    sid
}

pub fn logo_session(sid: &str) -> Option<LogoSession> {
    SESSIONS.lock().ok()?.get(sid).cloned()
}

fn session_for(chat: i64, user: i64) -> Option<LogoSession> {
    let sessions = SESSIONS.lock().ok()?;
    let mut other = None;
    for s in sessions.values() {
        if s.chat != chat {
            continue;
        }
        if s.user == user {
            return Some(s.clone());
        } else if other.is_none() {
            other = Some(s.clone());
        }
    }
    other
}

pub fn logo_caption(owner_id: i64) -> String {
    format!("Logo by [Owner](tg://user?id={owner_id})")
}

pub fn logo_menu_caption() -> String {
    "_Tap a button to modify your logo._\n\
     • **BG / Gradient** regenerate the base\n\
     • **Resize / Border / Rounded / Shadow / Add Text** apply on top"
        .to_string()
}

/// One menu button (binary maps it to a real Telethon/grammers button).
#[derive(Debug, Clone)]
pub struct MenuButton {
    pub label: String,
    pub action: MenuAction,
}

#[derive(Debug, Clone)]
pub enum MenuAction {
    /// Callback data, e.g. `lg_resize_<sid>`.
    Callback(String),
    /// `switch_inline` button: query sent to the assistant bot.
    SwitchInline(String),
}

fn cb(label: &str, data: String) -> MenuButton {
    MenuButton {
        label: label.to_string(),
        action: MenuAction::Callback(data),
    }
}

/// Binary hook: the main modification menu for a session (Python `_main_menu`).
pub fn logo_main_menu(sid: &str) -> Vec<Vec<MenuButton>> {
    vec![
        vec![
            cb("📐 Resize", format!("lg_resize_{sid}")),
            cb("🎨 BG Color", format!("lg_bg_{sid}")),
        ],
        vec![
            cb("🟦 Border", format!("lg_border_{sid}")),
            cb("⬜ Rounded", format!("lg_round_{sid}")),
        ],
        vec![
            cb("🌒 Shadow", format!("lg_shadow_{sid}")),
            cb("🌈 Gradient", format!("lg_grad_{sid}")),
        ],
        vec![
            MenuButton {
                label: "➕ Add Text".to_string(),
                action: MenuAction::SwitchInline(format!("logotext_{sid} ")),
            },
            cb("✅ Done", format!("lg_done_{sid}")),
        ],
    ]
}

/// Binary hook: a submenu for `op` (`resize`/`bg`/`border`/`grad`), or the
/// main menu otherwise (Python `_submenu`).
pub fn logo_submenu(sid: &str, op: &str, owner_id: i64) -> (String, Vec<Vec<MenuButton>>) {
    let back = vec![cb("‹ Back", format!("lg_menu_{sid}"))];
    match op {
        "resize" => (
            "**Resize** — pick a size:".to_string(),
            vec![
                vec![
                    cb("Half (0.5x)", format!("lg_resize_half_{sid}")),
                    cb("Double (2x)", format!("lg_resize_double_{sid}")),
                ],
                vec![
                    cb("Square 512", format!("lg_resize_512_{sid}")),
                    cb("Square 1024", format!("lg_resize_1024_{sid}")),
                ],
                back,
            ],
        ),
        "bg" => (
            "**Background Color** — regenerates the logo base:".to_string(),
            vec![
                vec![
                    cb("⚫ Black", format!("lg_bg_black_{sid}")),
                    cb("⚪ White", format!("lg_bg_white_{sid}")),
                ],
                vec![
                    cb("🔴 Red", format!("lg_bg_red_{sid}")),
                    cb("🔵 Blue", format!("lg_bg_blue_{sid}")),
                ],
                vec![
                    cb("🟢 Green", format!("lg_bg_green_{sid}")),
                    cb("🟣 Purple", format!("lg_bg_purple_{sid}")),
                ],
                vec![
                    cb("🟠 Orange", format!("lg_bg_orange_{sid}")),
                    cb("🟦 Navy", format!("lg_bg_navy_{sid}")),
                ],
                back,
            ],
        ),
        "border" => (
            "**Border** (white) — pick thickness:".to_string(),
            vec![
                vec![
                    cb("Thin", format!("lg_border_t2_{sid}")),
                    cb("Medium", format!("lg_border_t5_{sid}")),
                ],
                vec![cb("Thick", format!("lg_border_t10_{sid}"))],
                back,
            ],
        ),
        "grad" => (
            "**Gradient Background** — regenerates the logo base:".to_string(),
            vec![
                vec![
                    cb("Sunset", format!("lg_grad_0_{sid}")),
                    cb("Purple", format!("lg_grad_1_{sid}")),
                ],
                vec![
                    cb("Ocean", format!("lg_grad_2_{sid}")),
                    cb("Neon", format!("lg_grad_3_{sid}")),
                ],
                vec![
                    cb("Candy", format!("lg_grad_4_{sid}")),
                    cb("Mint", format!("lg_grad_5_{sid}")),
                ],
                back,
            ],
        ),
        _ => (
            format!("{}\n\n{}", logo_caption(owner_id), logo_menu_caption()),
            logo_main_menu(sid),
        ),
    }
}

/// Binary hook: apply one modification op to a session image (Python
/// `_apply_op`); returns the new path and updates the session.
pub async fn logo_apply_op(sid: &str, op: &str) -> Result<PathBuf> {
    let sess = logo_session(sid)
        .ok_or_else(|| Error::Other("Session expired. Run `.logo <text>` again.".to_string()))?;
    let newpath = apply_op(&sess, op).await?;
    if let Ok(mut sessions) = SESSIONS.lock() {
        if let Some(s) = sessions.get_mut(sid) {
            let old = s.path.clone();
            s.path = newpath.clone();
            if old != newpath {
                cleanup(&old);
            }
        }
    }
    Ok(newpath)
}

async fn apply_op(sess: &LogoSession, op: &str) -> Result<PathBuf> {
    let path = &sess.path;
    match op {
        "round" => round_corners(path, ROUND_RADIUS),
        "shadow" => add_shadow(path, (10, 10), 12.0, 180),
        "resize_half" => resize_logo(path, None, Some(0.5)),
        "resize_double" => resize_logo(path, None, Some(2.0)),
        "resize_512" => resize_logo(path, Some((512, 512)), None),
        "resize_1024" => resize_logo(path, Some((1024, 1024)), None),
        _ if op.starts_with("bg_") => {
            let Some(name) = &sess.name else {
                return Err(Error::Other(
                    "Regenerate needs the original text — use .logograd/.logobg".to_string(),
                ));
            };
            let _ = name;
            regen_bg_async(sess, Some(&op[3..]), None).await
        }
        _ if op.starts_with("grad_") => {
            let Some(name) = &sess.name else {
                return Err(Error::Other(
                    "Regenerate needs the original text — use .logograd/.logobg".to_string(),
                ));
            };
            let _ = name;
            let idx: usize = op[5..]
                .parse()
                .map_err(|_| Error::Other(format!("Unknown op: {op}")))?;
            regen_bg_async(sess, None, Some(idx)).await
        }
        _ if op.starts_with("border_t") => {
            let w: u32 = op[8..]
                .parse()
                .map_err(|_| Error::Other(format!("Unknown op: {op}")))?;
            add_border(path, w, [255, 255, 255])
        }
        _ => Err(Error::Other(format!("Unknown op: {op}"))),
    }
}

/// Regenerate a session logo on a new background (Python `_regen_bg`).
async fn regen_bg_async(
    sess: &LogoSession,
    solid: Option<&str>,
    gradient: Option<usize>,
) -> Result<PathBuf> {
    let Some(name) = &sess.name else {
        return Err(Error::Other(
            "Regenerate needs the original text — use .logograd/.logobg".to_string(),
        ));
    };
    let Some(font) = &sess.font else {
        return Err(Error::Other("No font in session".to_string()));
    };
    let cur = image::open(&sess.path).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let size = cur.dimensions();
    let bg = check_filename("logo_bg.png");
    if let Some(solid) = solid {
        make_solid_bg(&bg, size, color_by_name(solid, [16, 28, 58]))?;
    } else {
        let colors = GRADIENTS[gradient.unwrap_or(0) % GRADIENTS.len()];
        make_gradient_bg(&bg, size, Some(colors))?;
    }
    let out = make_logo(&bg, name, font, sess.style).await?;
    cleanup(&bg);
    Ok(out)
}

/// Binary hook: add text to a session logo via the inline query
/// (`logotext_<sid> <text>`, Python `_inline_add_text`); returns the new
/// path and updates the session.
pub async fn logo_add_text(sid: &str, text: &str) -> Result<PathBuf> {
    let sess = logo_session(sid)
        .ok_or_else(|| Error::Other("Session expired. Run `.logo <text>` again.".to_string()))?;
    let font = sess
        .font
        .clone()
        .ok_or_else(|| Error::Other("No font in session".to_string()))?;
    let newpath = add_text_to_image(
        &sess.path,
        text,
        &font,
        sess.style.fill,
        sess.style.stroke_width,
        sess.style.stroke_fill,
    )
    .await?;
    if let Ok(mut sessions) = SESSIONS.lock() {
        if let Some(s) = sessions.get_mut(sid) {
            let old = s.path.clone();
            s.path = newpath.clone();
            if old != newpath {
                cleanup(&old);
            }
        }
    }
    Ok(newpath)
}

/// Binary hook: parse an inline query of the form `logotext_<sid> <text>`
/// (Python `@in_pattern(r"logotext_(\S+) (.*)")`).
pub fn parse_inline_add_text(query: &str) -> Option<(String, String)> {
    let rest = query.strip_prefix("logotext_")?;
    let (sid, text) = rest.split_once(' ')?;
    if sid.is_empty() || text.trim().is_empty() {
        return None;
    }
    Some((sid.to_string(), text.trim().to_string()))
}

// ── resolve / deliver ─────────────────────────────────────────────────────

/// Return `(path, owned)` — the image to operate on (`None` when there is
/// no session and no reply to resolve).
async fn resolve_target(ctx: &Ctx) -> Result<(Option<PathBuf>, bool)> {
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): download replied media (Python:
        // logo.py:_resolve_target — `rep.download_media()`)
        return Err(Error::Other(
            "downloading replied media requires a binary Responder op (Python: logo.py:_resolve_target)"
                .to_string(),
        ));
    }
    match session_for(ctx.chat_id, ctx.sender_id) {
        Some(s) if s.path.exists() => Ok((Some(s.path), false)),
        _ => Ok((None, false)),
    }
}

fn sess_meta(ctx: &Ctx) -> (Option<String>, Option<PathBuf>, LogoStyle, (u32, u32)) {
    match session_for(ctx.chat_id, ctx.sender_id) {
        Some(s) => (s.name, s.font, s.style, s.size),
        None => (None, random_font(), LogoStyle::default(), DEFAULT_SIZE),
    }
}

/// Deliver a freshly produced logo: user-client send + the text-command
/// hint (Python `_deliver_logo` fallback 3 — the bot-first and inline
/// fallbacks need the assistant; see the module docs).
async fn deliver_logo(
    ctx: &Ctx,
    processing: i64,
    logopath: &Path,
    caption: &str,
    sid: &str,
) -> Result<()> {
    let _ = sid;
    ctx.send_file(logopath, caption).await?;
    let _ = ctx.delete(processing).await;
    ctx.reply(&format!(
        "{caption}\n\n_⚠️ Interactive buttons unavailable here._\n\
         Modify with: `.logoresize`, `.logobg`, `.logoborder`, `.logoround`, \
         `.logoshadow`, `.logograd`, `.logogtext`, `.logocurve`, \
         `.logomulti`, `.logotext`.\n\
         _Or run `.logo` in the assistant bot's DM for buttons._"
    ))
    .await?;
    Ok(())
}

async fn send_result(
    ctx: &Ctx,
    processing: i64,
    outpath: PathBuf,
    name: Option<String>,
    font: Option<PathBuf>,
    style: LogoStyle,
    size: (u32, u32),
) -> Result<()> {
    let sid = new_session(
        outpath.clone(),
        ctx.chat_id,
        ctx.sender_id,
        name,
        font,
        style,
        size,
    );
    let caption = logo_caption(ctx.config.owner_id);
    deliver_logo(ctx, processing, &outpath, &caption, &sid).await
}

// ── commands ──────────────────────────────────────────────────────────────

fn split_text_preset(arg: &str) -> (String, Option<usize>) {
    let parts: Vec<&str> = arg.split_whitespace().collect();
    if parts.len() > 1 {
        if let Ok(n) = parts[parts.len() - 1].parse::<usize>() {
            return (parts[..parts.len() - 1].join(" "), Some(n));
        }
    }
    (arg.to_string(), None)
}

async fn logo_gen(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let name = ctx.args.trim().to_string();
    if name.is_empty() {
        ctx.edit(processing, "`Give a name too!`").await?;
        return Ok(());
    }
    // Reply handling (Python inspects the reply for a font file or a
    // background image): the Ctx surface exposes neither the reply's
    // media nor a download, so a reply is treated the way Python treats
    // a failed reply parse — gradient background + random font.
    let base_size = DEFAULT_SIZE;
    let bg_ = check_filename("logo_bg.png");
    if make_gradient_bg(&bg_, base_size, None).is_err() {
        make_solid_bg(&bg_, base_size, [16, 28, 58])?;
    }
    let Some(font_) = random_font() else {
        cleanup(&bg_);
        ctx.edit(processing, "`No fonts found in resources/fonts/`")
            .await?;
        return Ok(());
    };
    let strke = if name.chars().count() <= 8 {
        10
    } else if name.chars().count() >= 9 {
        5
    } else {
        20
    };
    let style = LogoStyle {
        fill: [255, 255, 255],
        stroke_width: strke,
        stroke_fill: [0, 0, 0],
    };
    let name_path = match make_logo(&bg_, &name, &font_, style).await {
        Ok(p) => p,
        Err(_) => {
            make_solid_bg(&bg_, base_size, [16, 28, 58])?;
            make_logo(&bg_, &name, &font_, style).await?
        }
    };
    let sid = new_session(
        name_path.clone(),
        ctx.chat_id,
        ctx.sender_id,
        Some(name.clone()),
        Some(font_),
        style,
        base_size,
    );
    let caption = logo_caption(ctx.config.owner_id);
    deliver_logo(ctx, processing, &name_path, &caption, &sid).await?;
    if bg_ != name_path && bg_.exists() {
        cleanup(&bg_);
    }
    Ok(())
}

async fn logo_grad(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim().to_string();
    if arg.is_empty() {
        ctx.edit(processing, "`Usage: .logograd <text> [preset 0-5]`")
            .await?;
        return Ok(());
    }
    let (text, preset) = split_text_preset(&arg);
    if text.is_empty() {
        ctx.edit(processing, "`Give some text.`").await?;
        return Ok(());
    }
    let Some(font) = random_font() else {
        ctx.edit(processing, "`No fonts found.`").await?;
        return Ok(());
    };
    let colors = match preset {
        Some(p) => GRADIENTS[p % GRADIENTS.len()],
        None => GRADIENTS[(rand::random::<u64>() as usize) % GRADIENTS.len()],
    };
    let bg = check_filename("logo_bg.png");
    let strke = if text.chars().count() <= 8 { 10 } else { 5 };
    let style = LogoStyle {
        fill: [255, 255, 255],
        stroke_width: strke,
        stroke_fill: [0, 0, 0],
    };
    let out = match (|| async {
        make_gradient_bg(&bg, DEFAULT_SIZE, Some(colors))?;
        make_logo(&bg, &text, &font, style).await
    })()
    .await
    {
        Ok(p) => p,
        Err(e) => {
            cleanup(&bg);
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    cleanup(&bg);
    send_result(
        ctx,
        processing,
        out,
        Some(text),
        Some(font),
        style,
        DEFAULT_SIZE,
    )
    .await
}

async fn logo_gtext(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim().to_string();
    if arg.is_empty() {
        ctx.edit(processing, "`Usage: .logogtext <text> [preset 0-5]`")
            .await?;
        return Ok(());
    }
    let (text, preset) = split_text_preset(&arg);
    if text.is_empty() {
        ctx.edit(processing, "`Give some text.`").await?;
        return Ok(());
    }
    let Some(font) = random_font() else {
        ctx.edit(processing, "`No fonts found.`").await?;
        return Ok(());
    };
    let colors = match preset {
        Some(p) => GRADIENTS[p % GRADIENTS.len()],
        None => GRADIENTS[(rand::random::<u64>() as usize) % GRADIENTS.len()],
    };
    let bg = check_filename("logo_bg.png");
    let strke = if text.chars().count() <= 8 { 8 } else { 4 };
    let style = LogoStyle {
        fill: [255, 255, 255],
        stroke_width: strke,
        stroke_fill: [0, 0, 0],
    };
    let out = match (|| async {
        make_solid_bg(&bg, DEFAULT_SIZE, [0, 0, 0])?;
        gradient_text_logo(&bg, &text, &font, colors, strke, [0, 0, 0]).await
    })()
    .await
    {
        Ok(p) => p,
        Err(e) => {
            cleanup(&bg);
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    cleanup(&bg);
    send_result(
        ctx,
        processing,
        out,
        Some(text),
        Some(font),
        style,
        DEFAULT_SIZE,
    )
    .await
}

async fn logo_curve(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let text = ctx.args.trim().to_string();
    if text.is_empty() {
        ctx.edit(processing, "`Usage: .logocurve <text>`").await?;
        return Ok(());
    }
    let Some(font) = random_font() else {
        ctx.edit(processing, "`No fonts found.`").await?;
        return Ok(());
    };
    let bg = check_filename("logo_bg.png");
    let style = LogoStyle::default();
    let style = LogoStyle {
        stroke_width: 3,
        ..style
    };
    let out = match (|| async {
        make_gradient_bg(&bg, DEFAULT_SIZE, None)?;
        curve_text_logo(&bg, &text, &font, [255, 255, 255], 3, [0, 0, 0]).await
    })()
    .await
    {
        Ok(p) => p,
        Err(e) => {
            cleanup(&bg);
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    cleanup(&bg);
    send_result(
        ctx,
        processing,
        out,
        Some(text),
        Some(font),
        style,
        DEFAULT_SIZE,
    )
    .await
}

async fn logo_multi(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let text = ctx.args.trim().to_string();
    if text.is_empty() {
        ctx.edit(processing, "`Usage: .logomulti line1 | line2 | line3`")
            .await?;
        return Ok(());
    }
    let Some(font) = random_font() else {
        ctx.edit(processing, "`No fonts found.`").await?;
        return Ok(());
    };
    let bg = check_filename("logo_bg.png");
    let style = LogoStyle::default();
    let out = match (|| async {
        make_gradient_bg(&bg, DEFAULT_SIZE, None)?;
        multilayer_logo(&bg, &text, &font, [255, 255, 255], [0, 0, 0]).await
    })()
    .await
    {
        Ok(p) => p,
        Err(e) => {
            cleanup(&bg);
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    cleanup(&bg);
    send_result(
        ctx,
        processing,
        out,
        Some(text),
        Some(font),
        style,
        DEFAULT_SIZE,
    )
    .await
}

async fn logo_text(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let text = ctx.args.trim().to_string();
    if text.is_empty() {
        ctx.edit(
            processing,
            "`Usage: .logotext <text> (reply to a logo/image)`",
        )
        .await?;
        return Ok(());
    }
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(
            processing,
            "`Reply to a logo/image, or generate one with .logo first.`",
        )
        .await?;
        return Ok(());
    };
    let (_, font, _, _) = sess_meta(ctx);
    let Some(font) = font else {
        ctx.edit(processing, "`No fonts found.`").await?;
        return Ok(());
    };
    let style = LogoStyle::default();
    let out = match add_text_to_image(
        &path,
        &text,
        &font,
        style.fill,
        style.stroke_width,
        style.stroke_fill,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    let size = image::open(&out)
        .map(|i| i.dimensions())
        .unwrap_or(DEFAULT_SIZE);
    let (name, _, _, _) = sess_meta(ctx);
    send_result(
        ctx,
        processing,
        out,
        name.or(Some(text)),
        Some(font),
        style,
        size,
    )
    .await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}

async fn logo_resize(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let spec = ctx.args.trim().to_string();
    if spec.is_empty() {
        ctx.edit(processing, "`Usage: .logoresize <512|1024|2x|0.5x|WxH>`")
            .await?;
        return Ok(());
    }
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(
            processing,
            "`Reply to a logo (or generate one with .logo first).`",
        )
        .await?;
        return Ok(());
    };
    let low = spec.to_lowercase();
    let out = (|| -> Result<PathBuf> {
        if low.ends_with('x') {
            let num = &low[..low.len() - 1];
            if !num.is_empty()
                && num.chars().filter(|c| *c == '.').count() <= 1
                && num.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                let scale: f64 = num.parse().map_err(|_| {
                    Error::Other("Invalid size. Use 512, 1024, 2x, 0.5x, or WxH.".to_string())
                })?;
                return resize_logo(&path, None, Some(scale));
            }
        }
        if low.contains('x') {
            let mut it = low.splitn(2, 'x');
            let (a, b) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if !a.is_empty()
                && !b.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
            {
                let w: u32 = a.parse().unwrap();
                let h: u32 = b.parse().unwrap();
                return resize_logo(&path, Some((w, h)), None);
            }
        }
        if spec.chars().all(|c| c.is_ascii_digit()) && !spec.is_empty() {
            let d: u32 = spec.parse().unwrap();
            return resize_logo(&path, Some((d, d)), None);
        }
        Err(Error::Other(
            "Invalid size. Use 512, 1024, 2x, 0.5x, or WxH.".to_string(),
        ))
    })();
    let out = match out {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    let (name, font, style, size) = sess_meta(ctx);
    send_result(ctx, processing, out, name, font, style, size).await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}

async fn logo_bg(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let color = ctx.args.trim().to_string();
    let color = if color.is_empty() {
        "navy".to_string()
    } else {
        color
    };
    if !BG_COLORS
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case(&color))
    {
        let names: Vec<&str> = BG_COLORS.iter().map(|(n, _)| *n).collect();
        ctx.edit(
            processing,
            &format!("`Unknown colour. Pick: {}`", names.join(", ")),
        )
        .await?;
        return Ok(());
    }
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(processing, "`Reply to a logo (or .logo first).`")
            .await?;
        return Ok(());
    };
    let (name, font, style, size) = sess_meta(ctx);
    let Some(name) = name else {
        ctx.edit(processing, "`.logobg` regenerates the base with your text. Generate with `.logo <text>` first, then `.logobg <color>`.").await?;
        return Ok(());
    };
    let Some(font) = font else {
        ctx.edit(processing, "`No font in session.`").await?;
        return Ok(());
    };
    let sess = LogoSession {
        path: path.clone(),
        chat: ctx.chat_id,
        user: ctx.sender_id,
        name: Some(name.clone()),
        font: Some(font.clone()),
        style,
        size,
    };
    let out = match regen_bg_async(&sess, Some(&color), None).await {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    send_result(ctx, processing, out, Some(name), Some(font), style, size).await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}

async fn logo_border(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim().to_string();
    let width: u32 = if arg.chars().all(|c| c.is_ascii_digit()) && !arg.is_empty() {
        arg.parse().unwrap_or(10)
    } else {
        10
    };
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(processing, "`Reply to a logo (or .logo first).`")
            .await?;
        return Ok(());
    };
    let out = match add_border(&path, width, [255, 255, 255]) {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    let (name, font, style, size) = sess_meta(ctx);
    send_result(ctx, processing, out, name, font, style, size).await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}

async fn logo_round(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim().to_string();
    let radius: u32 = if arg.chars().all(|c| c.is_ascii_digit()) && !arg.is_empty() {
        arg.parse().unwrap_or(ROUND_RADIUS)
    } else {
        ROUND_RADIUS
    };
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(processing, "`Reply to a logo (or .logo first).`")
            .await?;
        return Ok(());
    };
    let out = match round_corners(&path, radius) {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    let (name, font, style, size) = sess_meta(ctx);
    send_result(ctx, processing, out, name, font, style, size).await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}

async fn logo_shadow(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let (path, owned) = match resolve_target(ctx).await {
        Ok(v) => v,
        Err(e) => {
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
    };
    let Some(path) = path else {
        ctx.edit(processing, "`Reply to a logo (or .logo first).`")
            .await?;
        return Ok(());
    };
    let out = match add_shadow(&path, (10, 10), 12.0, 180) {
        Ok(p) => p,
        Err(e) => {
            ctx.edit(processing, &format!("`Failed: {e}`")).await?;
            return Ok(());
        }
    };
    let (name, font, style, size) = sess_meta(ctx);
    send_result(ctx, processing, out, name, font, style, size).await?;
    if owned {
        cleanup(&path);
    }
    Ok(())
}
