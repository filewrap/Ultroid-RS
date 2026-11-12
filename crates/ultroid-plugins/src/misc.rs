//! Misc grab-bag — port of `plugins/misc.py` (834 LOC).
//!
//! * `.randomuser` — randomuser.me + random-data-api credit card, real HTTP.
//! * `.ascii` — text→ASCII art and image→ASCII art. The image engine
//!   uses the `image` crate (grayscale, autocontrast, resize, ramp).
//!   Text is rasterised with an embedded 5x7 bitmap font (the sandbox
//!   has no TTF rasteriser in the dependency set); reply-image mode
//!   needs the replied media downloaded, which is a Responder op.
//! * `.spoil` — spoiler via `<tg-spoiler>` HTML through `reply_html`.
//! * `.react` / `.effects` / `.effect` / `.loclive` / `.stoplive` /
//!   `.qr` / `.qrlist` / `.qrsend` — argument parsing, validation and
//!   state are real; the actual Telegram calls (reactions, effects,
//!   live locations, quick replies) are raw TL requests with no
//!   `Responder` equivalent and return the honest `TODO(responder-op)`
//!   error at their call sites.

use image::{DynamicImage, GrayImage, RgbImage};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub const ASCII_RAMP: &str = " .:-=+*#%@";
pub const FONT_NAMES: [&str; 6] = ["block", "mono", "serif", "slant", "condensed", "thin"];

const ASCII_HELP: &str = "`Usage:`\n`• .ascii <text>` — text art (default font: block)\n`• .ascii -f <font> <text>` — choose a font\n`• .ascii -ls` — list fonts\n`• .ascii <reply image>` — image to ASCII art\n`Flags:` -f -w -i/--invert -c/--chars <ramp> --color --text";

// ── 5x7 bitmap font (glyph = 7 rows of 5 bits, MSB = leftmost pixel) ─────

/// Classic 5x7 glyph for one printable ASCII char; lowercase maps to
/// uppercase. Unknown chars render blank.
pub fn glyph(ch: char) -> [u8; 7] {
    let c = ch.to_ascii_uppercase();
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '"' => [0x0A, 0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '*' => [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        ';' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x04, 0x08],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '@' => [0x0E, 0x11, 0x01, 0x0D, 0x15, 0x15, 0x0E],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        '\\' => [0x10, 0x08, 0x08, 0x04, 0x02, 0x02, 0x01],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        '^' => [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        '`' => [0x04, 0x08, 0x10, 0x00, 0x00, 0x00, 0x00],
        '{' => [0x02, 0x04, 0x04, 0x08, 0x04, 0x04, 0x02],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '}' => [0x08, 0x04, 0x04, 0x02, 0x04, 0x04, 0x08],
        '~' => [0x00, 0x00, 0x0E, 0x15, 0x00, 0x00, 0x00],
        _ => [0; 7],
    }
}

// ── ASCII engine ─────────────────────────────────────────────────────────

/// Convert a grayscale image into ASCII lines (Python `_image_to_ascii`).
pub fn image_to_ascii(img: &DynamicImage, width: u32, ramp: &str, invert: bool) -> Vec<String> {
    let ramp: Vec<char> = ramp.chars().collect();
    if ramp.len() < 2 || width == 0 {
        return Vec::new();
    }
    let mut gray = img.to_luma8();
    // Autocontrast: stretch min..max to 0..=255.
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for p in gray.pixels() {
        lo = lo.min(p.0[0]);
        hi = hi.max(p.0[0]);
    }
    if hi > lo {
        for p in gray.pixels_mut() {
            p.0[0] = ((p.0[0] as u32 - lo as u32) * 255 / (hi as u32 - lo as u32)) as u8;
        }
    }
    let (w, h) = gray.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // Characters are roughly twice as tall as they are wide.
    let new_h = ((width as f64 * h as f64 / w as f64) / 2.0).max(1.0) as u32;
    let small = image::imageops::resize(&gray, width, new_h, image::imageops::FilterType::Triangle);
    let ramp: Vec<char> = if invert {
        ramp.into_iter().rev().collect()
    } else {
        ramp
    };
    let n = ramp.len() - 1;
    let mut lines = Vec::with_capacity(new_h as usize);
    for y in 0..new_h {
        let mut row = String::with_capacity(width as usize);
        for x in 0..width {
            let px = small.get_pixel(x, y).0[0] as usize;
            row.push(ramp[n * px / 255]);
        }
        lines.push(row);
    }
    lines
}

/// Render *text* with the embedded bitmap font, then convert to ASCII
/// (Python `_text_to_ascii`, TTF font replaced by the 5x7 font — see the
/// module docs).
pub fn text_to_ascii(text: &str, width: u32, invert: bool) -> Vec<String> {
    let scale: u32 = 8;
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let cell_w = 6 * scale;
    let img_w = chars.len() as u32 * cell_w;
    let img_h = 7 * scale;
    let mut canvas = GrayImage::new(img_w, img_h);
    for (i, ch) in chars.iter().enumerate() {
        let g = glyph(*ch);
        for (ry, row) in g.iter().enumerate() {
            for cx in 0..5 {
                if row & (1 << (4 - cx)) != 0 {
                    let x0 = i as u32 * cell_w + cx as u32 * scale;
                    let y0 = ry as u32 * scale;
                    for dy in 0..scale {
                        for dx in 0..scale {
                            canvas.put_pixel(x0 + dx, y0 + dy, image::Luma([255u8]));
                        }
                    }
                }
            }
        }
    }
    image_to_ascii(&DynamicImage::ImageLuma8(canvas), width, ASCII_RAMP, invert)
}

/// Render ASCII *lines* back into an image, optionally coloured from
/// *color_img* (Python `_ascii_to_image`).
pub fn ascii_to_image(
    lines: &[String],
    color_img: Option<&DynamicImage>,
    ramp: &str,
) -> Option<RgbImage> {
    if lines.is_empty() {
        return None;
    }
    let scale: u32 = 2;
    let char_w = 6 * scale;
    let char_h = 7 * scale + 4;
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u32;
    let rows = lines.len() as u32;
    let mut out = RgbImage::new(cols * char_w, rows * char_h);
    for px in out.pixels_mut() {
        *px = image::Rgb([12, 12, 12]);
    }
    let color_small = color_img.map(|img| {
        let rgb = img.to_rgb8();
        image::imageops::resize(&rgb, cols, rows, image::imageops::FilterType::Triangle)
    });
    let ramp_chars: Vec<char> = ramp.chars().collect();
    let levels = ramp_chars.len().max(2) - 1;
    for (ry, line) in lines.iter().enumerate() {
        for (cx, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let color: [u8; 3] = if let Some(small) = &color_small {
                small.get_pixel(cx as u32, ry as u32).0
            } else {
                let lvl = ramp_chars.iter().position(|c| *c == ch).unwrap_or(levels) as i32;
                let grey = (45 + lvl * 210 / levels as i32) as u8;
                [grey, grey, grey]
            };
            let g = glyph(ch);
            for (gy, row) in g.iter().enumerate() {
                for gx in 0..5 {
                    if row & (1 << (4 - gx)) != 0 {
                        let x0 = cx as u32 * char_w + gx as u32 * scale;
                        let y0 = ry as u32 * char_h + gy as u32 * scale;
                        for dy in 0..scale {
                            for dx in 0..scale {
                                out.put_pixel(x0 + dx, y0 + dy, image::Rgb(color));
                            }
                        }
                    }
                }
            }
        }
    }
    Some(out)
}

fn save_temp_png(img: &RgbImage) -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("ultroid-ascii-{}.png", rand::random::<u32>()));
    img.save(&path)
        .map_err(|e| Error::Other(format!("failed to save ascii image: {e}")))?;
    Ok(path)
}

// ── .ascii argument parsing ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AsciiOpts {
    pub font: String,
    pub width: u32,
    pub invert: bool,
    pub color: bool,
    pub as_text: bool,
    pub ramp: String,
    pub text: String,
    pub list_fonts: bool,
}

impl Default for AsciiOpts {
    fn default() -> Self {
        AsciiOpts {
            font: "block".to_string(),
            width: 80,
            invert: false,
            color: false,
            as_text: false,
            ramp: ASCII_RAMP.to_string(),
            text: String::new(),
            list_fonts: false,
        }
    }
}

/// Shell-word split (single/double quotes, backslash escapes).
fn shell_split(arg: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut quote: Option<char> = None;
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if q == '"' && c == '\\' {
                if let Some(e) = chars.next() {
                    cur.push(e);
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            ' ' | '\t' | '\n' => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            '\'' | '"' => {
                quote = Some(c);
                has_token = true;
            }
            '\\' => {
                if let Some(e) = chars.next() {
                    cur.push(e);
                    has_token = true;
                }
            }
            _ => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::Other("Bad quotes: unterminated quote".to_string()));
    }
    if has_token {
        out.push(cur);
    }
    Ok(out)
}

/// Port of `_parse_ascii_args`.
pub fn parse_ascii_args(arg: &str) -> Result<AsciiOpts> {
    let tokens = shell_split(arg)?;
    let mut opts = AsciiOpts::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        match tok {
            "-ls" | "--list" | "--fonts" => opts.list_fonts = true,
            "-f" | "--font" if i + 1 < tokens.len() => {
                opts.font = tokens[i + 1].clone();
                i += 1;
            }
            "-w" | "--width" if i + 1 < tokens.len() => {
                opts.width = tokens[i + 1]
                    .parse::<u32>()
                    .map_err(|_| Error::Other("Width must be a number.".to_string()))?;
                i += 1;
            }
            "-c" | "--chars" | "--ramp" if i + 1 < tokens.len() => {
                let ramp = tokens[i + 1].clone();
                if ramp.chars().count() < 2 {
                    return Err(Error::Other(
                        "Char ramp needs at least 2 characters.".to_string(),
                    ));
                }
                opts.ramp = ramp;
                i += 1;
            }
            "-i" | "--invert" => opts.invert = true,
            "--color" => opts.color = true,
            "--text" => opts.as_text = true,
            _ if tok.starts_with('-') && tok.len() > 1 => {
                return Err(Error::Other(format!("Unknown flag: {tok}")));
            }
            _ => positional.push(tok.to_string()),
        }
        i += 1;
    }
    opts.text = positional.join(" ");
    Ok(opts)
}

async fn send_ascii_text(ctx: &Ctx, status_msg_id: i64, art: &str) -> Result<()> {
    if art.len() <= 3800 && art.matches('\n').count() <= 200 {
        if ctx
            .edit(status_msg_id, &format!("```{art}```"))
            .await
            .is_ok()
        {
            return Ok(());
        }
    }
    let out = ascii_to_image(
        &art.lines().map(|l| l.to_string()).collect::<Vec<_>>(),
        None,
        ASCII_RAMP,
    );
    let Some(img) = out else {
        ctx.edit(status_msg_id, "`Nothing to render.`").await?;
        return Ok(());
    };
    let path = save_temp_png(&img)?;
    let _ = ctx.delete(status_msg_id).await;
    let r = ctx
        .send_file(&path, "`🎨 ASCII art (too large for text)`")
        .await;
    let _ = tokio::fs::remove_file(&path).await;
    r?;
    Ok(())
}

// ── Commands ─────────────────────────────────────────────────────────────

async fn cmd_randomuser(ctx: &Ctx) -> Result<()> {
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    // Credit-card data (random-data-api), same fields as Python.
    let card: serde_json::Value = ctx
        .http
        .get("https://random-data-api.com/api/business_credit_card/random_card")
        .send()
        .await
        .map_err(Error::Http)?
        .json()
        .await
        .map_err(Error::Http)?;
    let card_text = format!(
        "**CARD_ID:** {} {}\n**C-ID :** {}",
        card.get("credit_card_number")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        card.get("credit_card_expiry_date")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        card.get("id")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let data: serde_json::Value = ctx
        .http
        .get("https://randomuser.me/api/")
        .send()
        .await
        .map_err(Error::Http)?
        .json()
        .await
        .map_err(Error::Http)?;
    let Some(user) = data
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
    else {
        return Err(Error::Other(
            "randomuser.me returned no results".to_string(),
        ));
    };
    let gender = user.get("gender").and_then(|v| v.as_str()).unwrap_or("");
    let icon = if gender == "male" {
        "🤵🏻‍♂"
    } else {
        "🤵🏻‍♀"
    };
    let name = user.get("name").cloned().unwrap_or_default();
    let loc = user.get("location").cloned().unwrap_or_default();
    let dob = user
        .get("dob")
        .and_then(|v| v.get("date"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let msg = format!(
        "\n{icon} **Name:** {}.{} {}\n**Street:** {} {}\n**City:** {}\n**State:** {}\n**Country:** {}\n**Postal Code:** {}\n**Email:** {}\n**Phone:** {}\n**Card:** {}\n**Birthday:** {}\n",
        name.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        name.get("first").and_then(|v| v.as_str()).unwrap_or(""),
        name.get("last").and_then(|v| v.as_str()).unwrap_or(""),
        loc.get("street")
            .and_then(|v| v.get("number"))
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        loc.get("street")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        loc.get("city").and_then(|v| v.as_str()).unwrap_or(""),
        loc.get("state").and_then(|v| v.as_str()).unwrap_or(""),
        loc.get("country").and_then(|v| v.as_str()).unwrap_or(""),
        loc.get("postcode")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        user.get("email").and_then(|v| v.as_str()).unwrap_or(""),
        user.get("phone").and_then(|v| v.as_str()).unwrap_or(""),
        card_text,
        dob.get(..10).unwrap_or(""),
    );
    let pic_url = user
        .get("picture")
        .and_then(|v| v.get("large"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let _ = ctx.delete(status).await;
    if !pic_url.is_empty() {
        if let Ok(resp) = ctx.http.get(&pic_url).send().await {
            if let Ok(bytes) = resp.bytes().await {
                let path = std::env::temp_dir()
                    .join(format!("ultroid-randomuser-{}.jpg", rand::random::<u32>()));
                if tokio::fs::write(&path, &bytes).await.is_ok() {
                    let r = ctx.send_file(&path, &msg).await;
                    let _ = tokio::fs::remove_file(&path).await;
                    r?;
                    return Ok(());
                }
            }
        }
    }
    ctx.reply(&msg).await?;
    Ok(())
}

async fn cmd_ascii(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let opts = match parse_ascii_args(arg) {
        Ok(o) => o,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    if opts.list_fonts {
        let fonts = FONT_NAMES
            .iter()
            .map(|n| format!("• `{n}`"))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.reply(&format!("**Available ASCII fonts:**\n{fonts}"))
            .await?;
        return Ok(());
    }
    let width = opts.width.clamp(20, 200);
    // Image mode: convert a replied photo/sticker to ASCII.
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): download the replied message's media
        // (Python: plugins/misc.py ascii_cmd via reply.download_media()).
        return Err(Error::Other(
            "converting a replied image to ASCII requires a binary Responder op (Python: plugins/misc.py ascii_cmd via reply.download_media())".to_string(),
        ));
    }
    // Text mode.
    let text = opts.text.clone();
    if text.is_empty() {
        ctx.reply(ASCII_HELP).await?;
        return Ok(());
    }
    if !FONT_NAMES.contains(&opts.font.as_str()) {
        ctx.reply(&format!(
            "`Unknown font '{}'. Use .ascii -ls to list fonts.`",
            opts.font
        ))
        .await?;
        return Ok(());
    }
    let status = ctx.reply(&ctx.get_str("com_1")).await?;
    let lines = text_to_ascii(&text, width, opts.invert);
    let art = lines.join("\n");
    if opts.as_text {
        send_ascii_text(ctx, status, &art).await?;
        return Ok(());
    }
    let img = ascii_to_image(&lines, None, &opts.ramp);
    let Some(img) = img else {
        ctx.edit(status, "`Could not render ASCII art.`").await?;
        return Ok(());
    };
    let path = save_temp_png(&img)?;
    let _ = ctx.delete(status).await;
    let cap = format!(
        "🎨 **ASCII art**{}",
        if opts.color { " (coloured)" } else { "" }
    );
    let r = ctx.send_file(&path, &cap).await;
    let _ = tokio::fs::remove_file(&path).await;
    r?;
    Ok(())
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn cmd_spoil(ctx: &Ctx) -> Result<()> {
    let mut text = ctx.args.trim().to_string();
    if text.is_empty() {
        text = ctx
            .reply_to_text
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() {
            ctx.reply("`Reply to a text message to spoiler it, or give text: .spoil <text>`")
                .await?;
            return Ok(());
        }
    }
    if text.is_empty() {
        ctx.reply("`Usage: .spoil <text>` (or reply to a text message).")
            .await?;
        return Ok(());
    }
    // Python wraps the text in a MessageEntitySpoiler; the Responder
    // surface carries no entities, so the equivalent <tg-spoiler> HTML
    // tag is used instead.
    ctx.reply_html(&format!("<tg-spoiler>{}</tg-spoiler>", escape_html(&text)))
        .await?;
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

async fn cmd_react(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a message to react to it.`").await?;
        return Ok(());
    }
    let arg = ctx.args.trim();
    if arg.is_empty() {
        // TODO(responder-op): read the replied message's reactions
        // (Python: plugins/misc.py react_cmd via reply.reactions.results).
        return Err(Error::Other(
            "listing a message's reactions requires a binary Responder op (Python: plugins/misc.py react_cmd via reply.reactions)".to_string(),
        ));
    }
    // TODO(responder-op): messages.sendReaction
    // (Python: plugins/misc.py react_cmd via client.send_reaction).
    Err(Error::Other(
        "sending a reaction requires a binary Responder op (Python: plugins/misc.py react_cmd via client.send_reaction)".to_string(),
    ))
}

async fn cmd_effects(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): messages.getAvailableEffects
    // (Python: plugins/misc.py effects_cmd via GetAvailableEffectsRequest).
    let _ = ctx;
    Err(Error::Other(
        "listing message effects requires a binary Responder op (Python: plugins/misc.py effects_cmd via messages.GetAvailableEffectsRequest)".to_string(),
    ))
}

async fn cmd_effect(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let mut tokens = arg.splitn(2, char::is_whitespace);
    let Some(id_tok) = tokens.next().filter(|t| !t.is_empty()) else {
        ctx.reply("`Usage: .effect <id> <text>` (see .effects).")
            .await?;
        return Ok(());
    };
    let effect_id: i64 = match id_tok.parse() {
        Ok(n) => n,
        Err(_) => {
            ctx.reply("`Effect id must be a number — see .effects.`")
                .await?;
            return Ok(());
        }
    };
    let mut text = tokens.next().unwrap_or("").trim().to_string();
    if text.is_empty() {
        text = ctx
            .reply_to_text
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    if text.is_empty() {
        ctx.reply("`Usage: .effect <id> <text>` (see .effects).")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): send a message with message_effect_id
    // (Python: plugins/misc.py effect_cmd via send_message(message_effect_id=...)).
    let _ = (effect_id, text);
    Err(Error::Other(
        "sending a message with an effect requires a binary Responder op (Python: plugins/misc.py effect_cmd via send_message(message_effect_id))".to_string(),
    ))
}

/// Port of `_parse_loclive_args`: `(lat, lon, seconds, note)`.
pub fn parse_loclive_args(
    arg: &str,
    reply_geo: Option<(f64, f64)>,
) -> Result<(f64, f64, i64, Option<String>)> {
    let tokens: Vec<&str> = arg.splitn(2, char::is_whitespace).collect();
    let (lat, lon, rest): (f64, f64, String) = match reply_geo {
        Some((lat, lon)) => {
            if tokens.is_empty() {
                return Err(Error::Other(
                    "Give the number of seconds to share for.".to_string(),
                ));
            }
            (lat, lon, tokens.get(1).copied().unwrap_or("").to_string())
        }
        None => {
            let first = tokens.first().copied().unwrap_or("");
            if first.is_empty() || !first.contains(',') {
                return Err(Error::Other(
                    "Give `<lat>,<long> <seconds>`, or reply to a location message.".to_string(),
                ));
            }
            let (lat_s, lon_s) = first.split_once(',').unwrap_or(("", ""));
            let lat: f64 = lat_s
                .parse()
                .map_err(|_| Error::Other("`<lat>,<long>` must both be numbers.".to_string()))?;
            let lon: f64 = lon_s
                .parse()
                .map_err(|_| Error::Other("`<lat>,<long>` must both be numbers.".to_string()))?;
            let rest = tokens.get(1).copied().unwrap_or("");
            (lat, lon, rest.to_string())
        }
    };
    let rest_tokens: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
    if rest_tokens.is_empty() || rest_tokens[0].is_empty() {
        return Err(Error::Other(
            "Give the number of seconds to share for.".to_string(),
        ));
    }
    let seconds: i64 = rest_tokens[0]
        .parse()
        .map_err(|_| Error::Other("Seconds must be a whole number.".to_string()))?;
    let seconds = seconds.clamp(60, 86400);
    let note = rest_tokens
        .get(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok((lat, lon, seconds, note))
}

async fn cmd_loclive(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    // The replied message's coordinates are not in the command context.
    let reply_geo: Option<(f64, f64)> = None;
    if ctx.reply_to_id.is_some()
        && !arg
            .split_whitespace()
            .next()
            .map(|t| t.contains(','))
            .unwrap_or(false)
    {
        // TODO(responder-op): read the replied location message's geo
        // (Python: plugins/misc.py loclive_cmd via reply.geo).
        return Err(Error::Other(
            "re-using a replied location's coordinates requires a binary Responder op (Python: plugins/misc.py loclive_cmd via reply.geo)".to_string(),
        ));
    }
    let parsed = match parse_loclive_args(arg, reply_geo) {
        Ok(p) => p,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    // TODO(responder-op): send InputMediaGeoLive
    // (Python: plugins/misc.py loclive_cmd via send_message(file=InputMediaGeoLive)).
    let _ = parsed;
    Err(Error::Other(
        "starting a live location requires a binary Responder op (Python: plugins/misc.py loclive_cmd via send_message(file=InputMediaGeoLive))".to_string(),
    ))
}

async fn cmd_stoplive(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to your live-location message with .stoplive to stop it.`")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): edit the replied live-location message with
    // InputMediaGeoLive(stopped=True)
    // (Python: plugins/misc.py stoplive_cmd via EditMessageRequest).
    Err(Error::Other(
        "stopping a live location requires a binary Responder op (Python: plugins/misc.py stoplive_cmd via messages.EditMessageRequest with InputMediaGeoLive(stopped=True))".to_string(),
    ))
}

async fn cmd_qr(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    let mut tokens = arg.splitn(2, char::is_whitespace);
    let Some(shortcut) = tokens.next().filter(|t| !t.is_empty()) else {
        ctx.reply("`Usage: .qr <shortcut> <text>` (or reply to save its text).")
            .await?;
        return Ok(());
    };
    let mut text = tokens.next().unwrap_or("").trim().to_string();
    if text.is_empty() {
        text = ctx
            .reply_to_text
            .clone()
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    if text.is_empty() {
        ctx.reply("`Usage: .qr <shortcut> <text>` (or reply to save its text).")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): send to Saved Messages with
    // quick_reply_shortcut=InputQuickReplyShortcut(shortcut=...)
    // (Python: plugins/misc.py qr_cmd via send_message("me", ...)).
    let _ = (shortcut, text);
    Err(Error::Other(
        "saving a quick reply requires a binary Responder op (Python: plugins/misc.py qr_cmd via send_message(\"me\", quick_reply_shortcut=...))".to_string(),
    ))
}

async fn cmd_qrlist(ctx: &Ctx) -> Result<()> {
    // TODO(responder-op): messages.getQuickReplies
    // (Python: plugins/misc.py qrlist_cmd via GetQuickRepliesRequest).
    let _ = ctx;
    Err(Error::Other(
        "listing quick replies requires a binary Responder op (Python: plugins/misc.py qrlist_cmd via messages.GetQuickRepliesRequest)".to_string(),
    ))
}

async fn cmd_qrsend(ctx: &Ctx) -> Result<()> {
    let shortcut = ctx.args.trim();
    if shortcut.is_empty() {
        ctx.reply("`Usage: .qrsend <shortcut>`").await?;
        return Ok(());
    }
    // TODO(responder-op): messages.getQuickReplyMessages +
    // messages.sendQuickReplyMessages
    // (Python: plugins/misc.py qrsend_cmd via GetQuickReplyMessagesRequest /
    // SendQuickReplyMessagesRequest).
    Err(Error::Other(
        "sending a quick reply requires a binary Responder op (Python: plugins/misc.py qrsend_cmd via messages.SendQuickReplyMessagesRequest)".to_string(),
    ))
}

pub fn register(registry: &mut Registry) {
    let help = "Commands Available -\n• `.randomuser` — Generate details about a random user.\n• `.ascii <text>` — Convert text into big ASCII art using built-in fonts/styles.\n• `.ascii -f <font> <text>` — Render text art with a specific font. Fonts: block, mono, serif, slant, condensed, thin\n• `.ascii -ls` — List all available ASCII fonts.\n• `.ascii <reply image>` — Convert a replied image into ASCII art (rendered as a photo).\n• `.spoil <text>` — Send text hidden behind a spoiler (tap-to-reveal).\n• `.react <emoji...>` (reply) — React to the replied message.\n• `.effects` — List available message effects (id + emoticon).\n• `.effect <id> <text>` — Send `<text>` with the message effect `<id>`.\n• `.loclive <lat>,<long> <seconds> [note]` — Start sharing a live location.\n• `.stoplive` (reply) — Stop an active live location.\n• `.qr <shortcut> <text>` — Create/append to a quick-reply shortcut.\n• `.qrlist` — List saved quick-reply shortcuts.\n• `.qrsend <shortcut>` — Send a saved quick-reply shortcut's messages.";
    reg_add(registry, def("randomuser", "randomuser", help), |ctx| {
        Box::pin(cmd_randomuser(ctx))
    });
    reg_add(registry, def("ascii", "ascii( (.*)|$)", help), |ctx| {
        Box::pin(cmd_ascii(ctx))
    });
    reg_add(registry, def("spoil", "spoil( (.*)|$)", help), |ctx| {
        Box::pin(cmd_spoil(ctx))
    });
    reg_add(registry, def("react", "react( (.*)|$)", help), |ctx| {
        Box::pin(cmd_react(ctx))
    });
    reg_add(registry, def("effects", "effects$", help), |ctx| {
        Box::pin(cmd_effects(ctx))
    });
    reg_add(registry, def("effect", "effect( (.*)|$)", help), |ctx| {
        Box::pin(cmd_effect(ctx))
    });
    reg_add(registry, def("loclive", "loclive( (.*)|$)", help), |ctx| {
        Box::pin(cmd_loclive(ctx))
    });
    reg_add(registry, def("stoplive", "stoplive$", help), |ctx| {
        Box::pin(cmd_stoplive(ctx))
    });
    reg_add(registry, def("qr", "qr( (.*)|$)", help), |ctx| {
        Box::pin(cmd_qr(ctx))
    });
    reg_add(registry, def("qrlist", "qrlist$", help), |ctx| {
        Box::pin(cmd_qrlist(ctx))
    });
    reg_add(registry, def("qrsend", "qrsend( (.*)|$)", help), |ctx| {
        Box::pin(cmd_qrsend(ctx))
    });
}
