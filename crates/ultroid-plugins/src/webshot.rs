//! Webshot plugin — port of `plugins/webshot.py` (276 LOC).
//!
//! `.webshot <url> [-f] [-w <width>] [-m]` — capture a webpage screenshot.
//!
//! Python tries Playwright (Chromium) first and falls back to the keyless
//! Microlink screenshot API. This port keeps that order: a system Chromium
//! binary is driven headless when one is installed, otherwise the Microlink
//! API is used via `reqwest` (the Python fallback path, kept verbatim).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;

use crate::util::{def, reg_add};
use regex::Regex;
use tokio::process::Command as AsyncCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available -\n\n• `{i}webshot <url>`\n    Capture and send a screenshot of a webpage.\n\n• `{i}webshot -f <url>`\n    Capture a full-page screenshot.\n\n• `{i}webshot -w <width> <url>`\n    Set the viewport width (e.g. `-w 800`).\n\n• `{i}webshot -m <url>`\n    Use a mobile viewport (375x812).";

const MOBILE_VIEWPORT: (u32, u32) = (375, 812);
const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);
const NAV_TIMEOUT_MS: u64 = 45_000;
const API_TIMEOUT_SECS: u64 = 60;

const PW_INSTALL_HINT: &str = "Install Playwright for reliable, high-quality screenshots:\n\
`pip install playwright && playwright install chromium`";

static SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://").unwrap());
static HOST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://([^/\s?#]+)").unwrap());

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("webshot", "webshot( (.*)|$)", HELP), |ctx| {
        Box::pin(webshot(ctx))
    });
}

/// Split the raw argument string into `(full_page, mobile, width, url)`.
/// Mirrors Python `_parse_opts`; unknown `-` options are an error.
fn parse_opts(text: &str) -> std::result::Result<(bool, bool, Option<String>, String), String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut full_page = false;
    let mut mobile = false;
    let mut width: Option<String> = None;
    let mut url_tokens: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "-f" {
            full_page = true;
        } else if tok == "-m" {
            mobile = true;
        } else if tok == "-w" {
            i += 1;
            if i >= tokens.len() {
                return Err("Option `-w` needs a width value.".to_string());
            }
            width = Some(tokens[i].to_string());
        } else if let Some(rest) = tok.strip_prefix("-w") {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                width = Some(rest.to_string());
            } else {
                return Err(format!(
                    "Unknown option `{tok}`.\nUse `-f` (full page), `-w <width>` or `-m` (mobile)."
                ));
            }
        } else if tok.starts_with('-') && tok.len() > 1 {
            return Err(format!(
                "Unknown option `{tok}`.\nUse `-f` (full page), `-w <width>` or `-m` (mobile)."
            ));
        } else {
            url_tokens.push(tok);
        }
        i += 1;
    }
    Ok((
        full_page,
        mobile,
        width,
        url_tokens.join(" ").trim().to_string(),
    ))
}

/// Return `raw` with a scheme, or `None` when it isn't a URL.
fn normalize_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let raw = if SCHEME_RE.is_match(raw) {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let m = HOST_RE.captures(&raw)?;
    if !m[1].contains('.') {
        return None;
    }
    Some(raw)
}

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

/// Capture with a system headless Chromium (the Python Playwright backend's
/// role). `--virtual-time-budget` gives client-side rendering a moment to
/// settle, mirroring Python's 1.5 s post-navigation wait.
async fn chromium_shot(
    url: &str,
    full_page: bool,
    mobile: bool,
    width: Option<u32>,
    dest: &PathBuf,
) -> Result<()> {
    let chrome = chromium_binary()
        .ok_or_else(|| Error::Other("no headless Chromium installed".to_string()))?;
    let (vw, vh) = if mobile {
        MOBILE_VIEWPORT
    } else if let Some(w) = width {
        (w, 800)
    } else {
        DEFAULT_VIEWPORT
    };
    let mut args: Vec<String> = vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-sandbox".into(),
        "--hide-scrollbars".into(),
        format!("--window-size={vw},{vh}"),
        format!("--virtual-time-budget={}", NAV_TIMEOUT_MS),
        format!("--screenshot={}", dest.display()),
    ];
    if mobile {
        // Best-effort mobile emulation via a phone user-agent.
        args.push(
            "--user-agent=Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
             AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
                .to_string(),
        );
    }
    if full_page {
        // Chromium's --screenshot has no full-page flag; note it and capture
        // the (tall) viewport instead — the Microlink fallback below does
        // support full-page captures.
        tracing::warn!("webshot: --screenshot cannot capture full page; viewport shot taken");
    }
    let status = AsyncCommand::new(chrome)
        .args(&args)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if !status.success()
        || !dest.exists()
        || std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) == 0
    {
        return Err(Error::Other(format!(
            "headless Chromium screenshot failed (exit {status})"
        )));
    }
    Ok(())
}

/// Fallback capture via the keyless Microlink screenshot API (Python
/// `_microlink_shot`, verbatim parameters).
async fn microlink_shot(
    ctx: &Ctx,
    url: &str,
    full_page: bool,
    mobile: bool,
    width: Option<u32>,
    dest: &PathBuf,
) -> Result<()> {
    let mut params: Vec<(String, String)> = vec![
        ("url".into(), url.into()),
        ("screenshot".into(), "true".into()),
        ("screenshot.type".into(), "png".into()),
    ];
    if full_page {
        params.push(("screenshot.fullPage".into(), "true".into()));
    }
    if mobile {
        params.push(("screenshot.device".into(), "iphone-x".into()));
    } else if let Some(w) = width {
        params.push(("screenshot.width".into(), w.to_string()));
    }
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding(k), urlencoding(v)))
        .collect();
    let api_url = format!("https://api.microlink.io/?{}", query.join("&"));
    let resp = ctx
        .http
        .get(&api_url)
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "screenshot API returned HTTP {}",
            resp.status()
        )));
    }
    let data: serde_json::Value = resp.json().await?;
    if data.get("status").and_then(|s| s.as_str()) != Some("success") {
        return Err(Error::Other(format!(
            "screenshot API failed: {}",
            data.get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
        )));
    }
    let shot_url = data
        .pointer("/data/screenshot/url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| Error::Other("screenshot API returned no image".to_string()))?
        .to_string();
    let img = ctx.http.get(&shot_url).send().await?;
    if !img.status().is_success() {
        return Err(Error::Other(format!(
            "screenshot download failed (HTTP {})",
            img.status()
        )));
    }
    let content = img.bytes().await?;
    if content.is_empty() {
        return Err(Error::Other(
            "screenshot API returned an empty image".to_string(),
        ));
    }
    tokio::fs::write(dest, &content).await?;
    Ok(())
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn webshot(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim().to_string();
    let (full_page, mobile, width_raw, mut url) = match parse_opts(&raw) {
        Ok(v) => v,
        Err(e) => {
            ctx.reply(&e).await?;
            return Ok(());
        }
    };
    let width: Option<u32> = match width_raw {
        Some(w) => match w.parse::<u32>() {
            Ok(n) if (100..=4000).contains(&n) => Some(n),
            _ => {
                ctx.reply("`Width must be a number between 100 and 4000.`")
                    .await?;
                return Ok(());
            }
        },
        None => None,
    };

    // Fall back to the replied message's text when no URL is given inline.
    if url.is_empty() {
        if let Some(rt) = &ctx.reply_to_text {
            url = rt.trim().to_string();
        }
    }
    if url.is_empty() {
        ctx.reply(&format!(
            "{}\n`{}webshot <url> [-f] [-w <width>] [-m]`",
            ctx.get_str("wbs_1"),
            ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }
    let Some(url) = normalize_url(&url) else {
        ctx.reply(&ctx.get_str("wbs_2")).await?;
        return Ok(());
    };

    let msg = ctx.reply("`Capturing screenshot...`").await?;
    let dest = std::env::temp_dir().join(format!("webshot_{}.png", std::process::id()));

    let mut used_fallback = false;
    let outcome: Result<()> = async {
        match chromium_shot(&url, full_page, mobile, width, &dest).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("webshot: Chromium failed ({e}) — trying API fallback.");
                used_fallback = true;
                microlink_shot(ctx, &url, full_page, mobile, width, &dest).await
            }
        }
    }
    .await;

    match outcome {
        Ok(()) => {
            let exists =
                dest.exists() && std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) > 0;
            if !exists {
                ctx.edit(msg, "**Failed to capture screenshot.**\n`the page could not be rendered (empty screenshot)`").await?;
            } else {
                let mut caption = ctx.get_str("wbs_3").replace("{}", &url);
                if used_fallback {
                    caption += &format!("\n\n_Fallback screenshot API used._\n{PW_INSTALL_HINT}");
                }
                ctx.send_file(&dest, &caption).await?;
                let _ = ctx.delete(msg).await;
            }
        }
        Err(e) => {
            let hint = if used_fallback {
                format!("\n\n{PW_INSTALL_HINT}")
            } else {
                String::new()
            };
            ctx.edit(
                msg,
                &format!("**Failed to capture screenshot.**\n`{e}`{hint}"),
            )
            .await?;
        }
    }
    if dest.exists() {
        let _ = tokio::fs::remove_file(&dest).await;
    }
    Ok(())
}
