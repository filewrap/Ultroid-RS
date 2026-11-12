//! Code beautifiers — port of `plugins/beautify.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python help entry `help_beautify`):
//! - `.carbon` / `.rcarbon` — code screenshot via carbonara
//!   (`.rcarbon` picks a random background colour, `.carbon` uses white).
//! - `.ccarbon <colour>` — code screenshot with a chosen background.
//! - `.rayso [theme] [text]` — ray.so style code screenshot.
//!
//! Code comes from the reply (its text, or a replied file's content —
//! the file case needs a download, see TODO below) or from the command
//! text. Rendering goes through the same HTTP APIs the Python helpers
//! use: carbonara (`https://carbonara.solopov.dev/api/cook`, the
//! backend of `fns.Carbon`) and the ray.so API endpoint
//! (`https://rayso-api-desvhu-33.koyeb.app/generate`, the backend of
//! `fns.Carbon(rayso=True)`). Python's `rayso` handler drives ray.so in
//! a Playwright browser instead; no browser-automation crate is in the
//! workspace deps, so the API backend is used — same rendered output,
//! no browser needed.

use std::path::PathBuf;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "carbon",
            "(rc|c)arbon",
            "Code screenshot: .carbon (reply to code/text).",
        ),
        |ctx| Box::pin(carbon(ctx)),
    );
    reg_add(
        registry,
        def(
            "rcarbon",
            "(rc|c)arbon",
            "Code screenshot with a random background colour: .rcarbon (reply to code/text).",
        ),
        |ctx| Box::pin(carbon(ctx)),
    );
    reg_add(
        registry,
        def(
            "ccarbon",
            "ccarbon( (.*)|$)",
            "Code screenshot with a colour: .ccarbon <colour> (reply to code/text).",
        ),
        |ctx| Box::pin(ccarbon(ctx)),
    );
    reg_add(
        registry,
        def(
            "rayso",
            "rayso",
            "Ray.so code screenshot: .rayso [theme] [text] (reply to text).",
        ),
        |ctx| Box::pin(rayso(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

/// TODO(responder-op): download replied media to a local file.
fn todo_download(python_ref: &str) -> Error {
    Error::Other(format!(
        "download replied media requires a binary Responder op (Python: {python_ref})"
    ))
}

fn tmp_file(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-beauty-{}.{ext}", uuid::Uuid::new_v4()))
}

/// The code to render: replied text wins; else the text after the
/// command; a replied *file* needs a download (TODO site).
fn code_source(ctx: &Ctx, inline_text: &str) -> Result<Option<String>> {
    if let Some(t) = &ctx.reply_to_text {
        if !t.trim().is_empty() {
            return Ok(Some(t.clone()));
        }
    }
    if ctx.reply_to_id.is_some() {
        // Reply exists but carries no text: Python downloads the file and
        // reads it. Only the download is missing here.
        return Err(todo_download("beautify.py:cr_bn/ccarbon"));
    }
    if !inline_text.trim().is_empty() {
        return Ok(Some(inline_text.to_string()));
    }
    Ok(None)
}

/// POST the carbonara API and return the rendered image bytes
/// (Python `fns.Carbon`).
async fn carbon_render(ctx: &Ctx, code: &str, background: &str) -> Result<Vec<u8>> {
    let resp = ctx
        .http
        .post("https://carbonara.solopov.dev/api/cook")
        .json(&serde_json::json!({
            "code": code,
            "backgroundColor": background,
        }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "carbonara API failed: HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        return Err(Error::Other("carbonara API returned no image".into()));
    }
    Ok(bytes.to_vec())
}

fn color_list() -> Vec<String> {
    // Python reads `resources/colorlist.txt` (CSS colour names). The file
    // lives in the source repo, not the Rust workspace; a built-in subset
    // of CSS colours is used instead.
    [
        "AliceBlue",
        "AntiqueWhite",
        "Aqua",
        "Aquamarine",
        "Azure",
        "Beige",
        "Bisque",
        "Black",
        "BlanchedAlmond",
        "Blue",
        "BlueViolet",
        "Brown",
        "BurlyWood",
        "CadetBlue",
        "Chartreuse",
        "Chocolate",
        "Coral",
        "CornflowerBlue",
        "Cornsilk",
        "Crimson",
        "Cyan",
        "DarkBlue",
        "DarkCyan",
        "DarkGoldenRod",
        "DarkGray",
        "DarkGreen",
        "DarkKhaki",
        "DarkMagenta",
        "DarkOliveGreen",
        "DarkOrange",
        "DarkOrchid",
        "DarkRed",
        "DarkSalmon",
        "DarkSeaGreen",
        "DarkSlateBlue",
        "DarkSlateGray",
        "DarkTurquoise",
        "DarkViolet",
        "DeepPink",
        "DeepSkyBlue",
        "DimGray",
        "DodgerBlue",
        "FireBrick",
        "FloralWhite",
        "ForestGreen",
        "Fuchsia",
        "Gainsboro",
        "GhostWhite",
        "Gold",
        "GoldenRod",
        "Gray",
        "Green",
        "GreenYellow",
        "HoneyDew",
        "HotPink",
        "IndianRed",
        "Indigo",
        "Ivory",
        "Khaki",
        "Lavender",
        "LavenderBlush",
        "LawnGreen",
        "LemonChiffon",
        "LightBlue",
        "LightCoral",
        "LightCyan",
        "LightGoldenRodYellow",
        "LightGray",
        "LightGreen",
        "LightPink",
        "LightSalmon",
        "LightSeaGreen",
        "LightSkyBlue",
        "LightSlateGray",
        "LightSteelBlue",
        "LightYellow",
        "Lime",
        "LimeGreen",
        "Linen",
        "Magenta",
        "Maroon",
        "MediumAquaMarine",
        "MediumBlue",
        "MediumOrchid",
        "MediumPurple",
        "MediumSeaGreen",
        "MediumSlateBlue",
        "MediumSpringGreen",
        "MediumTurquoise",
        "MediumVioletRed",
        "MidnightBlue",
        "MintCream",
        "MistyRose",
        "Moccasin",
        "NavajoWhite",
        "Navy",
        "OldLace",
        "Olive",
        "OliveDrab",
        "Orange",
        "OrangeRed",
        "Orchid",
        "PaleGoldenRod",
        "PaleGreen",
        "PaleTurquoise",
        "PaleVioletRed",
        "PapayaWhip",
        "PeachPuff",
        "Peru",
        "Pink",
        "Plum",
        "PowderBlue",
        "Purple",
        "Red",
        "RosyBrown",
        "RoyalBlue",
        "SaddleBrown",
        "Salmon",
        "SandyBrown",
        "SeaGreen",
        "SeaShell",
        "Sienna",
        "Silver",
        "SkyBlue",
        "SlateBlue",
        "SlateGray",
        "Snow",
        "SpringGreen",
        "SteelBlue",
        "Tan",
        "Teal",
        "Thistle",
        "Tomato",
        "Turquoise",
        "Violet",
        "Wheat",
        "White",
        "WhiteSmoke",
        "Yellow",
        "YellowGreen",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ── commands ──────────────────────────────────────────────────────────────

async fn carbon(ctx: &Ctx) -> Result<()> {
    // captures[1] is "rc" or "c" (pattern `(rc|c)arbon`).
    let group = ctx
        .captures
        .get(1)
        .cloned()
        .unwrap_or_else(|| "c".to_string());
    let random_bg = group.starts_with('r');
    let msg = ctx.reply("Processing...").await?;
    let Some(code) = code_source(ctx, &ctx.args)? else {
        ctx.edit(
            msg,
            "Give some code to carbonise (reply or text after the command)",
        )
        .await?;
        return Ok(());
    };
    let background = if random_bg {
        let colors = color_list();
        let idx = rand::random::<usize>() % colors.len();
        colors[idx].clone()
    } else {
        "White".to_string()
    };
    let bytes = carbon_render(ctx, &code, &background).await?;
    let out = tmp_file("jpg");
    tokio::fs::write(&out, &bytes).await?;
    ctx.delete(msg).await?;
    ctx.send_file(&out, "Carbonised").await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

async fn ccarbon(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() && ctx.reply_to_text.is_none() {
        ctx.reply("Give a colour: `.ccarbon <colour>`").await?;
        return Ok(());
    }
    let msg = ctx.reply("Processing...").await?;
    // With a reply, the whole arg is the colour; without one, the first
    // token is the colour and the rest is the code (Python).
    let (colour, inline_code) = if ctx.reply_to_id.is_some() {
        (raw.to_string(), String::new())
    } else {
        let mut it = raw.splitn(2, ' ');
        (
            it.next().unwrap_or("").to_string(),
            it.next().unwrap_or("").to_string(),
        )
    };
    if colour.is_empty() {
        ctx.edit(msg, "Give a colour: `.ccarbon <colour>`").await?;
        return Ok(());
    }
    let Some(code) = code_source(ctx, &inline_code)? else {
        ctx.edit(
            msg,
            "Give some code to carbonise (reply or text after the command)",
        )
        .await?;
        return Ok(());
    };
    let bytes = carbon_render(ctx, &code, &colour).await?;
    let out = tmp_file("jpg");
    tokio::fs::write(&out, &bytes).await?;
    ctx.delete(msg).await?;
    ctx.send_file(&out, "Carbonised").await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

const RAYSO_THEMES: [&str; 14] = [
    "meadow", "breeze", "raindrop", "candy", "crimson", "falcon", "sunset", "noir", "midnight",
    "bitmap", "ice", "sand", "forest", "mono",
];

async fn rayso(ctx: &Ctx) -> Result<()> {
    let msg = ctx.reply("Processing...").await?;
    let tokens: Vec<&str> = ctx.args.split_whitespace().collect();
    let mut theme: Option<String> = None;
    let mut text: Option<String> = None;
    if let Some(first) = tokens.first() {
        if RAYSO_THEMES.contains(first) {
            theme = Some(first.to_string());
            if tokens.len() > 1 {
                text = Some(tokens[1..].join(" "));
            }
        } else {
            text = Some(tokens.join(" "));
        }
    }
    if let Some(reply) = &ctx.reply_to_text {
        if text.is_none() {
            text = Some(reply.clone());
        }
    }
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        ctx.edit(msg, "No text to beautify!").await?;
        return Ok(());
    };
    let theme = theme.unwrap_or_else(|| {
        let idx = rand::random::<usize>() % RAYSO_THEMES.len();
        RAYSO_THEMES[idx].to_string()
    });
    // Same backend as `fns.Carbon(rayso=True)`:
    // POST https://rayso-api-desvhu-33.koyeb.app/generate
    // {text, theme, darkMode, title}.
    let resp = ctx
        .http
        .post("https://rayso-api-desvhu-33.koyeb.app/generate")
        .json(&serde_json::json!({
            "text": text,
            "theme": theme,
            "darkMode": true,
            "title": "Ultroid",
        }))
        .send()
        .await?;
    if !resp.status().is_success() {
        ctx.edit(
            msg,
            &format!("RaySo — Error: API failed (HTTP {})", resp.status()),
        )
        .await?;
        return Err(Error::Other(format!(
            "rayso API failed: HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        ctx.edit(msg, "RaySo — Error: API returned no image")
            .await?;
        return Err(Error::Other("rayso API returned no image".into()));
    }
    let out = tmp_file("png");
    tokio::fs::write(&out, &bytes).await?;
    ctx.delete(msg).await?;
    ctx.send_file(&out, "RaySo").await?;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}
