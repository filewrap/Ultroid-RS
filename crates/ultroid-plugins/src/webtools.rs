//! Web tools — port of `plugins/webtools.py` (313 LOC).
//!
//! * `.webapp <url> [text]` — send a message with a web_app inline button.
//! * `.simplewebapp <url> [text]` — reply-keyboard simple web_app button.
//! * `.webview <bot_username> <url>` — request a web view from a bot
//!   (gated behind `WEBTOOLS_LIVE=1`, exactly like Python).
//! * `.miniapp [token new]` — Mini App settings-dashboard link (owner-only).
//! * `.webappdata` — show the last web_app data captured (owner-only).
//!
//! Telegram cannot express inline/reply keyboards, web-view requests, or
//! cross-peer sends through the `Ctx`/`Responder` surface, so the button
//! sends are honest `TODO(responder-op)` errors; everything around them
//! (parsing, gating, token minting/rotation, capture formatting) is real.
//! The two update listeners (`MessageActionWebViewDataSent*` and
//! `UpdateWebViewResultSent`) belong to the binary's update loop — see the
//! `record_webapp_data` hook below.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::util::{db_get, db_set, def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "Web tools / web_app demonstration plugin (W3-B).\n\n• `{i}webapp <url> [text]`\n    Send a message with a web_app inline button.\n\n• `{i}simplewebapp <url> [text]`\n    Send a message with a reply-keyboard simple web_app button.\n\n• `{i}webview <bot_username> <url>`\n    Request a web view from a bot and report the returned URL.\n\n• `{i}miniapp [token new]`\n    Open the Mini App settings dashboard, or rotate its access token.\n\n• `{i}webappdata`\n    Show the last web_app data captured by the listeners.";

const MINIAPP_URL_KEY: &str = "MINIAPP_URL";
const MINIAPP_TOKEN_KEY: &str = "MINIAPP_TOKEN";
const MAX_CAPTURE_CHARS: usize = 2048;

/// In-memory store of the last web_app data received by the listeners
/// (transient until restart, exactly like Python's `_last_webapp_data`).
static LAST_WEBAPP_DATA: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("webapp", "webapp( (.*)|$)", HELP), |ctx| {
        Box::pin(webapp_cmd(ctx))
    });
    reg_add(
        registry,
        def("simplewebapp", "simplewebapp( (.*)|$)", HELP),
        |ctx| Box::pin(simplewebapp_cmd(ctx)),
    );
    reg_add(registry, def("webview", "webview( (.*)|$)", HELP), |ctx| {
        Box::pin(webview_cmd(ctx))
    });
    let mut miniapp_def = def("miniapp", r"miniapp(?:\s+(.*)|$)", HELP);
    miniapp_def.owner_only = true;
    reg_add(registry, miniapp_def, |ctx| Box::pin(miniapp_cmd(ctx)));
    let mut data_def = def("webappdata", "webappdata$", HELP);
    data_def.owner_only = true;
    reg_add(registry, data_def, |ctx| Box::pin(webappdata_cmd(ctx)));
}

/// Concise summary for a captured web_app event (Python `_format_data`).
pub fn format_webapp_data(kind: &str, fields: &[(&str, String)]) -> String {
    let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
    let mut summary = format!("`{kind}` {}", parts.join(" "));
    if summary.len() > MAX_CAPTURE_CHARS {
        summary.truncate(MAX_CAPTURE_CHARS - 14);
        summary.push_str("…[truncated]");
    }
    summary
}

/// Binary hook: record a captured web_app event (Python's
/// `on_webapp_data_message` / `on_webview_result_sent` listeners).
/// The binary's update loop calls this when it sees
/// `MessageActionWebViewDataSent` / `MessageActionWebViewDataSentMe` in a
/// service message, or an `UpdateWebViewResultSent`.
pub fn record_webapp_data(kind: &str, payload: String) {
    if let Ok(mut map) = LAST_WEBAPP_DATA.lock() {
        map.insert(kind.to_string(), payload);
    }
}

/// Binary hook: snapshot of the captured web_app data, in insertion order
/// (best-effort — `HashMap` order is not insertion order; callers that need
/// order should keep their own log; Python used a dict, which is ordered).
pub fn last_webapp_data() -> Vec<(String, String)> {
    LAST_WEBAPP_DATA
        .lock()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// Binary hook: the metadata line the listener forwards to `LOG_CHANNEL`
/// (Python `_forward_capture_summary`); the actual send needs a
/// `send_message` Responder op to an arbitrary peer.
pub fn log_channel_summary(kind: &str, payload_len: usize) -> String {
    format!("**webtools** `{kind}` captured; payload withheld ({payload_len} chars).")
}

/// Mint a fresh Mini App bearer token (Python `MiniAppStore` /
/// `AUTHORITY.mint` — a random token persisted under `MINIAPP_TOKEN`).
fn mint_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| format!("{:x}", rng.r#gen::<u8>() % 16))
        .collect()
}

async fn webapp_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim();
    if args.is_empty() {
        ctx.reply(&format!(
            "**Usage:** `{}webapp <url> [text]`",
            ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }
    let mut parts = args.splitn(2, char::is_whitespace);
    let url = parts.next().unwrap_or("").trim();
    let text = parts.next().unwrap_or("").trim();
    let text = if text.is_empty() {
        "Open Web App"
    } else {
        text
    };
    let _ = (url, text);
    // TODO(responder-op): send message with a web_app inline button
    // (Python: webtools.py:webapp_cmd — `event.eor(text, buttons=ikm(...))`)
    Err(Error::Other(
        "sending a message with a web_app inline button requires a binary \
         Responder op (Python: webtools.py:webapp_cmd)"
            .to_string(),
    ))
}

async fn simplewebapp_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim();
    if args.is_empty() {
        ctx.reply(&format!(
            "**Usage:** `{}simplewebapp <url> [text]`",
            ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }
    let mut parts = args.splitn(2, char::is_whitespace);
    let url = parts.next().unwrap_or("").trim();
    let text = parts.next().unwrap_or("").trim();
    let text = if text.is_empty() {
        "Open Web App"
    } else {
        text
    };
    let _ = (url, text);
    // TODO(responder-op): send message with a ReplyKeyboardMarkup holding a
    // KeyboardButtonSimpleWebView (Python: webtools.py:simplewebapp_cmd)
    Err(Error::Other(
        "sending a reply-keyboard simple-web-app button requires a binary \
         Responder op (Python: webtools.py:simplewebapp_cmd)"
            .to_string(),
    ))
}

async fn webview_cmd(ctx: &Ctx) -> Result<()> {
    // Gated exactly like Python: `_LIVE_GATE = bool(os.environ.get("WEBTOOLS_LIVE"))`.
    if std::env::var("WEBTOOLS_LIVE")
        .unwrap_or_default()
        .is_empty()
    {
        ctx.reply(
            "**Live web-view round-trip is gated.**\n\
             Set environment variable `WEBTOOLS_LIVE=1` and retry.\n\
             This requires a bot peer that supports web apps.",
        )
        .await?;
        return Ok(());
    }
    let args = ctx.args.trim();
    let mut parts = args.splitn(2, char::is_whitespace);
    let bot_handle = parts.next().unwrap_or("");
    let url = parts.next().unwrap_or("").trim();
    if bot_handle.is_empty() || url.is_empty() {
        ctx.reply(&format!(
            "**Usage:** `{}webview <bot_username> <url>`",
            ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }
    let bot_handle = if bot_handle.starts_with('@') {
        bot_handle.to_string()
    } else {
        format!("@{bot_handle}")
    };
    let _ = (bot_handle, url);
    // TODO(responder-op): messages.RequestWebViewRequest round-trip
    // (Python: webtools.py:webview_cmd — resolve bot entity, request the
    // web view, report WebViewResultUrl + query_id/fullsize/fullscreen)
    Err(Error::Other(
        "messages.RequestWebViewRequest requires a binary Responder op \
         (Python: webtools.py:webview_cmd)"
            .to_string(),
    ))
}

async fn miniapp_cmd(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_lowercase();
    if arg == "token new" {
        let token = mint_token();
        db_set(ctx, MINIAPP_TOKEN_KEY, &token).await?;
        ctx.reply(&format!(
            "**Mini App token rotated.** Old dashboard links stop working — \
             send `{}miniapp` for a fresh one.",
            ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }

    let base_url = db_get(ctx, MINIAPP_URL_KEY).await?.unwrap_or_default();
    let base_url = base_url.trim().to_string();
    if base_url.is_empty() {
        ctx.reply(&format!(
            "**Mini App settings dashboard isn't configured.**\n\n\
             1. Set a public HTTPS base URL that reaches this bot:\n\
             `{}setdb {MINIAPP_URL_KEY} https://your-public-host`\n\
             2. Point it (reverse proxy/tunnel) at this bot's Mini App \
             server — `MINIAPP_BIND`/`MINIAPP_PORT` env vars, default \
             `0.0.0.0:7861`.\n\
             3. Restart the bot, then run `{}miniapp` again.",
            ctx.config.hndlr, ctx.config.hndlr
        ))
        .await?;
        return Ok(());
    }

    let token = match db_get(ctx, MINIAPP_TOKEN_KEY).await? {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            let t = mint_token();
            db_set(ctx, MINIAPP_TOKEN_KEY, &t).await?;
            t
        }
    };
    let settings_url = format!(
        "{}/settings?token={}",
        base_url.trim_end_matches('/'),
        token
    );
    let _ = settings_url;
    // TODO(responder-op): send the settings-dashboard message with a web_app
    // inline button (private chat), or deliver it to Saved Messages when
    // invoked in a group/channel so the bearer token is never posted
    // publicly (Python: webtools.py:miniapp_cmd)
    Err(Error::Other(
        "sending the Mini App dashboard web_app button requires a binary \
         Responder op (Python: webtools.py:miniapp_cmd)"
            .to_string(),
    ))
}

async fn webappdata_cmd(ctx: &Ctx) -> Result<()> {
    let data = last_webapp_data();
    if data.is_empty() {
        ctx.reply("**No web_app data received yet.**").await?;
        return Ok(());
    }
    let mut lines = vec!["**Last web_app events:**".to_string()];
    for (kind, payload) in data {
        lines.push(format!("• `{kind}`: `{payload}`"));
    }
    ctx.reply(&lines.join("\n")).await?;
    Ok(())
}
