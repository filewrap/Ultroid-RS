//! Web-upload plugin — port of `plugins/webupload.py` (185 LOC) plus the
//! upload backbone it imports, `pyUltroid/_core/webupload.py` (590 LOC).
//!
//! * `.webupload` / `.wul [-s <service>]` — upload a replied file or a path
//!   to catbox, litterbox, file.io, 0x0.st, transfer.sh, uguu, filebin or
//!   siasky.
//!
//! The multipart bodies are built by hand: this workspace's reqwest build
//! has no `multipart` feature (same approach as the assistant crate's
//! `webupload_result`). Files are read fully into memory for the upload —
//! reqwest here has no streaming-body feature, so the Python live-progress
//! callback is replaced by a single "Uploading..." status edit.
//!
//! The service-selection menu is delivered by inline result / inline
//! buttons: the user-client path inline-queries the assistant and clicks
//! the first result, the bot-client path sends the menu via
//! `send_markup_to`; the menu path caches the file and exposes
//! [`service_menu`] / [`webupload_cache_get`] for the binary's inline +
//! callback handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::util::{def, reg_add};
use regex::Regex;
use ultroid_core::fns::humanbytes;
use ultroid_core::tg::InlineButton;
use ultroid_core::{ClientKind, Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available -\n\n• `{i}webupload` | `{i}wul`\n    Upload files on another server.\n\n    Reply to a media message or pass a file path.\n    Use `-s <service>` to upload directly without the menu.\n\n    Services: catbox, litterbox, file.io, 0x0.st, transfer.sh, uguu, filebin, siasky\n    Example: `{i}wul -s catbox` (reply to a file)";

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) Ultroid-WebUpload/1.0";

/// Upload services in registry order (Python `UPLOAD_SERVICES`).
pub const UPLOAD_SERVICES: [&str; 8] = [
    "catbox",
    "litterbox",
    "file.io",
    "0x0.st",
    "transfer",
    "uguu",
    "filebin",
    "siasky",
];

/// Services offered in the selection menu (Python `MENU_SERVICES`).
pub const MENU_SERVICES: [&str; 7] = [
    "catbox",
    "litterbox",
    "file.io",
    "0x0.st",
    "transfer",
    "uguu",
    "filebin",
];

pub fn service_label(key: &str) -> &str {
    match key {
        "transfer" => "transfer.sh",
        other => other,
    }
}

/// Normalise a user-typed service name to a registry key (Python
/// `resolve_service`, aliases included).
pub fn resolve_service(name: &str) -> Option<&'static str> {
    let key = name.trim().to_lowercase();
    match key.as_str() {
        "catbox" => Some("catbox"),
        "litterbox" => Some("litterbox"),
        "file.io" => Some("file.io"),
        "0x0.st" => Some("0x0.st"),
        "transfer" => Some("transfer"),
        "uguu" => Some("uguu"),
        "filebin" => Some("filebin"),
        "siasky" => Some("siasky"),
        "catbox.moe" => Some("catbox"),
        "litterbox.catbox.moe" => Some("litterbox"),
        "fileio" | "file_io" => Some("file.io"),
        "0x0" | "0x0st" | "nullpointer" => Some("0x0.st"),
        "transfer.sh" | "transfersh" => Some("transfer"),
        "uguu.se" => Some("uguu"),
        _ => None,
    }
}

/// Outcome of a successful upload (Python `UploadResult`).
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub url: String,
    pub service: String,
    pub delete_url: Option<String>,
    pub delete_note: Option<String>,
    pub expires: Option<String>,
}

fn guess_ctype(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn multipart_body(
    boundary: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    filename: &str,
    ctype: &str,
    file_bytes: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; \
             filename=\"{filename}\"\r\nContent-Type: {ctype}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn first_url_line(text: &str) -> String {
    text.trim().lines().next().unwrap_or("").trim().to_string()
}

/// Upload `file_path` to service `key` (Python `upload_to`). Raises
/// `Error::Other` naming the service on any failure.
pub async fn upload_to(
    http: &reqwest::Client,
    key: &str,
    file_path: &Path,
) -> Result<UploadResult> {
    if !file_path.is_file() {
        return Err(Error::Other(format!(
            "{key}: file not found: {}",
            file_path.display()
        )));
    }
    let filename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let ctype = guess_ctype(file_path);
    let bytes = tokio::fs::read(file_path).await?;
    let boundary = format!("----ultroid{}", uuid::Uuid::new_v4().simple());
    let ct = format!("multipart/form-data; boundary={boundary}");

    let fail = |key: &str, status: reqwest::StatusCode, text: &str| {
        Error::Other(format!(
            "{key} returned HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        ))
    };

    match key {
        "catbox" | "litterbox" => {
            let endpoint = if key == "catbox" {
                "https://catbox.moe/user/api.php"
            } else {
                "https://litterbox.catbox.moe/resources/internals/api.php"
            };
            let mut fields: Vec<(&str, &str)> = vec![("reqtype", "fileupload")];
            if key == "litterbox" {
                fields.push(("time", "72h"));
            }
            let body = multipart_body(&boundary, &fields, "fileToUpload", &filename, ctype, &bytes);
            let resp = http
                .post(endpoint)
                .header("Content-Type", ct)
                .header("User-Agent", UA)
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let url = first_url_line(&text);
            if !url.starts_with("http") {
                return Err(Error::Other(format!(
                    "{key} did not return a URL: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            Ok(UploadResult {
                url,
                service: key.to_string(),
                delete_url: None,
                delete_note: None,
                expires: if key == "litterbox" {
                    Some("72h".into())
                } else {
                    None
                },
            })
        }
        "file.io" => {
            let body = multipart_body(&boundary, &[], "file", &filename, ctype, &bytes);
            let resp = http
                .post("https://file.io")
                .header("Content-Type", ct)
                .header("User-Agent", UA)
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let mut link = data
                .get("link")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let key_val = data.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if link.is_empty() && !key_val.is_empty() {
                link = format!("https://file.io/{key_val}");
            }
            if !link.starts_with("http") {
                return Err(Error::Other(format!(
                    "file.io did not return a link: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            let delete_url = if key_val.is_empty() {
                None
            } else {
                Some(format!("https://file.io/{key_val}"))
            };
            Ok(UploadResult {
                url: link,
                service: key.to_string(),
                delete_url: delete_url.clone(),
                delete_note: delete_url.map(|_| "DELETE request".to_string()),
                expires: data
                    .get("expiry")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        }
        "0x0.st" => {
            let body = multipart_body(&boundary, &[], "file", &filename, ctype, &bytes);
            let resp = http
                .post("https://0x0.st")
                .header("Content-Type", ct)
                .header("User-Agent", UA)
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let url = first_url_line(&text);
            if !url.starts_with("http") {
                return Err(Error::Other(format!(
                    "0x0.st did not return a URL: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            Ok(UploadResult {
                url,
                service: key.to_string(),
                delete_url: None,
                delete_note: None,
                expires: None,
            })
        }
        "transfer" => {
            // PUT with the filename in the path, raw body (Python `TransferShUploader`).
            let encoded: String = filename
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect();
            let resp = http
                .put(format!("https://transfer.sh/{encoded}"))
                .header("User-Agent", UA)
                .header("Max-Days", "14")
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let url = first_url_line(&text);
            if !url.starts_with("http") {
                return Err(Error::Other(format!(
                    "transfer.sh did not return a URL: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            Ok(UploadResult {
                url,
                service: key.to_string(),
                delete_url: None,
                delete_note: None,
                expires: Some("14 days".into()),
            })
        }
        "uguu" => {
            let body = multipart_body(&boundary, &[], "files[]", &filename, ctype, &bytes);
            let resp = http
                .post("https://uguu.se/upload")
                .header("Content-Type", ct)
                .header("User-Agent", UA)
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let mut url = data
                .pointer("/files/0/url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if url.is_empty() {
                url = data
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            if !url.starts_with("http") {
                return Err(Error::Other(format!(
                    "uguu did not return a URL: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            Ok(UploadResult {
                url,
                service: key.to_string(),
                delete_url: None,
                delete_note: None,
                expires: Some("3 hours".into()),
            })
        }
        "filebin" => {
            // POST /<bin>/<filename>, raw body; bin is random per upload.
            let bin = format!(
                "ultroid-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..10]
            );
            let encoded: String = filename
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect();
            let url = format!("https://filebin.net/{bin}/{encoded}");
            let resp = http
                .post(&url)
                .header("User-Agent", UA)
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let bin_field = data.get("bin");
            let bin_id = match bin_field {
                Some(serde_json::Value::Object(o)) => o
                    .get("id")
                    .or_else(|| o.get("bin"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            let delete_url = bin_id.map(|id| format!("https://filebin.net/{id}"));
            Ok(UploadResult {
                url,
                service: key.to_string(),
                delete_url: delete_url.clone(),
                delete_note: delete_url.map(|_| "DELETE request".to_string()),
                expires: None,
            })
        }
        "siasky" => {
            let body = multipart_body(&boundary, &[], "file", &filename, ctype, &bytes);
            let resp = http
                .post("https://siasky.net/skynet/skyfile")
                .header("Content-Type", ct)
                .header("User-Agent", UA)
                .body(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if status.as_u16() >= 400 {
                return Err(fail(key, status, &text));
            }
            let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let skylink = data.get("skylink").and_then(|v| v.as_str()).unwrap_or("");
            if skylink.is_empty() {
                return Err(Error::Other(format!(
                    "siasky did not return a skylink: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            Ok(UploadResult {
                url: format!("https://siasky.net/{skylink}"),
                service: key.to_string(),
                delete_url: None,
                delete_note: None,
                expires: None,
            })
        }
        other => Err(Error::Other(format!("Unknown upload service: {other}"))),
    }
}

/// Render an `UploadResult` as display text (Python `_format_result`;
/// the "View" URL button is binary-side — see `format_result_url`).
pub fn format_result(result: &UploadResult, filename: &str) -> String {
    let mut text = format!("**Uploaded** `{filename}` **on** `{}`.", result.service);
    if let Some(delete_url) = &result.delete_url {
        text += &format!("\n🗑 Delete: `{delete_url}`");
        if let Some(note) = &result.delete_note {
            text += &format!(" ({note})");
        }
    }
    if let Some(expires) = &result.expires {
        text += &format!("\n⏳ Expires: {expires}");
    }
    text
}

/// The "View" URL button target for a result (binary builds the button).
pub fn format_result_url(result: &UploadResult) -> &str {
    &result.url
}

// ── Menu-path cache (Python `pyUltroid.fns.tools._webupload_cache`) ────────

/// `(chat_id, message_id) -> file path`, for the menu path.
static WEBUPLOAD_CACHE: LazyLock<Mutex<HashMap<(i64, i64), PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Binary hook: the 2-column service-selection menu as `(label,
/// callback_data)` pairs. Callback data format is Python's `fl<key>//<cache_key>`.
pub fn service_menu(cache_key: &str) -> Vec<(String, String)> {
    MENU_SERVICES
        .iter()
        .map(|k| (service_label(k).to_string(), format!("fl{k}//{cache_key}")))
        .collect()
}

/// Binary hook: look up a cached upload file by the `fl…//<chat>:<msg>`
/// cache key the menu callback carries.
pub fn webupload_cache_get(cache_key: &str) -> Option<PathBuf> {
    let (chat, msg) = cache_key.split_once(':')?;
    let chat: i64 = chat.parse().ok()?;
    let msg: i64 = msg.parse().ok()?;
    WEBUPLOAD_CACHE.lock().ok()?.get(&(chat, msg)).cloned()
}

/// Binary hook: upload a menu-cached file to a service and render the
/// result text (used by the `fl<key>//<cache_key>` callback handler).
pub async fn upload_cached(
    http: &reqwest::Client,
    key: &str,
    cache_key: &str,
) -> Result<(UploadResult, String)> {
    let Some(path) = webupload_cache_get(cache_key) else {
        return Err(Error::Other(format!("upload cache miss for {cache_key}")));
    };
    let result = upload_to(http, key, &path).await?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = format_result(&result, &filename);
    Ok((result, text))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("webupload", "webupload( (.*)|$)", HELP),
        |ctx| Box::pin(webupload_cmd(ctx)),
    );
    reg_add(registry, def("wul", "wul( (.*)|$)", HELP), |ctx| {
        Box::pin(webupload_cmd(ctx))
    });
}

static SERVICE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)-s\s+(\S+)").unwrap());

fn progress_bar(pct: u64) -> String {
    let filled = (pct / 10).min(10);
    format!(
        "[{}{}]",
        "■".repeat(filled as usize),
        "□".repeat((10 - filled) as usize)
    )
}

/// Resolve the file to upload from a path arg or a replied media message.
/// Returns `(path, is_temp)`; `is_temp` marks files we may clean up after.
async fn obtain_file(ctx: &Ctx, path: &str) -> Result<Option<(PathBuf, bool)>> {
    if !path.is_empty() {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(Some((p, false)));
        }
    }
    if ctx.reply_to_id.is_none() {
        return Ok(None);
    }
    // Download the replied media via the Responder (Python:
    // webupload.py:_obtain_file — `reply.download_media` / fast_downloader).
    let reply_id = ctx.reply_to_id.unwrap_or(0);
    let dest = std::env::temp_dir().join(format!("ultroid-webupload-{}", uuid::Uuid::new_v4()));
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    Ok(Some((path, true)))
}

async fn do_direct_upload(ctx: &Ctx, key: &str, file: &Path, is_temp: bool) -> Result<()> {
    let label = service_label(key);
    let disp = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let total = tokio::fs::metadata(file)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let msg = ctx
        .reply(&format!("📤 Uploading `{disp}` on {label}..."))
        .await?;
    let bar = progress_bar(0);
    let _ = ctx
        .edit(
            msg,
            &format!(
                "📤 Uploading `{disp}` on {label}...\n{bar} 0%  (0 B/{})",
                humanbytes(total)
            ),
        )
        .await;
    match upload_to(&ctx.http, key, file).await {
        Ok(result) => {
            let text = format_result(&result, &disp);
            let _ = ctx.edit(msg, &format!("{text}\n🔗 {}", result.url)).await;
        }
        Err(e) => {
            let _ = ctx
                .edit(
                    msg,
                    &format!("❌ Failed to upload `{disp}` on {label}: `{e}`"),
                )
                .await;
        }
    }
    if is_temp {
        let _ = tokio::fs::remove_file(file).await;
    }
    Ok(())
}

async fn webupload_cmd(ctx: &Ctx) -> Result<()> {
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim().to_string();

    let (raw_service, path) = match SERVICE_RE.captures(&arg) {
        Some(m) => {
            let raw = m.get(1).map(|g| g.as_str().to_string());
            let start = m.get(0).unwrap().start();
            let end = m.get(0).unwrap().end();
            let path = format!("{}{}", &arg[..start], &arg[end..])
                .trim()
                .to_string();
            (raw, path)
        }
        None => (None, arg.clone()),
    };

    let file = obtain_file(ctx, &path).await?;
    let Some((file, is_temp)) = file else {
        if raw_service.is_some() {
            ctx.edit(processing, "Reply to a file or give a file path to upload.")
                .await?;
        } else {
            ctx.edit(processing, "Reply to file or give file path...")
                .await?;
        }
        return Ok(());
    };

    // Direct upload via the `-s` flag.
    if let Some(raw) = raw_service {
        let Some(key) = resolve_service(&raw) else {
            let avail: Vec<&str> = UPLOAD_SERVICES.iter().map(|k| service_label(k)).collect();
            ctx.edit(
                processing,
                &format!("Unknown service `{raw}`.\nAvailable: {}", avail.join(", ")),
            )
            .await?;
            return Ok(());
        };
        let _ = ctx.delete(processing).await;
        return do_direct_upload(ctx, key, &file, is_temp).await;
    }

    // Menu path: cache the file, then offer the menu.
    let cache_key = format!("{}:{}", ctx.chat_id, ctx.message_id);
    if let Ok(mut cache) = WEBUPLOAD_CACHE.lock() {
        cache.insert((ctx.chat_id, ctx.message_id), file.clone());
    }
    let _ = ctx.delete(processing).await;
    if ctx.client_kind != ClientKind::AsstBot {
        // User client: inline-query `fl2lnk <chat>:<msg>` on the assistant
        // and click the first result into this chat (Python:
        // webupload.py:webupload_cmd).
        let Some(asst) = &ctx.asst else {
            return Err(Error::Other(
                "the upload service menu needs the assistant bot, which is not connected (Python: webupload.py:webupload_cmd)".into(),
            ));
        };
        let Some(bot_dialog_id) = ctx.asst_dialog_id else {
            return Err(Error::Other(
                "the upload service menu needs the assistant bot, which is not connected (Python: webupload.py:webupload_cmd)".into(),
            ));
        };
        asst.inline_query_click(
            bot_dialog_id,
            ctx.asst_username.as_deref(),
            &format!("fl2lnk {cache_key}"),
            ctx.chat_id,
            ctx.reply_to_id.map(|id| id as i32),
        )
        .await?;
    } else {
        // Bot client: send the `fl<key>//<cache_key>` menu from
        // `service_menu()` in 2-column rows (Python edits the status
        // message; there is no edit-with-markup op, so this sends a fresh
        // menu message instead).
        let rows: Vec<Vec<InlineButton>> = service_menu(&cache_key)
            .chunks(2)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|(label, data)| InlineButton::data(label, data.as_bytes().to_vec()))
                    .collect()
            })
            .collect();
        ctx.responder
            .send_markup_to(ctx.chat_id, "Choose Server to Upload File...", rows)
            .await?;
    }
    Ok(())
}
