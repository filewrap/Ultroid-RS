//! Google Drive — port of `plugins/gdrive.py` (1,139 LOC) + the parts it
//! uses of `pyUltroid/fns/gDrive.py` (`GDriveManager`, 236 LOC).
//!
//! `.gdul` / `.gdown` / `.gdsearch` / `.gdlist` / `.gdinfo` / `.gdmove` /
//! `.gdcp` / `.gdtrash` / `.gddelete` / `.gdshare` / `.gdnewfolder` /
//! `.gdfolder` / `.gdrefresh`.
//!
//! Real Drive **v3** REST calls over `reqwest` (`ctx.http`). Python uses
//! the v2 client library (`googleapiclient.discovery.build("drive", "v2")`
//! with `oauth2client`); v3 is the live equivalent — same operations,
//! `title` → `name`, `files().insert` → `POST /drive/v3/files`.
//!
//! Credentials (exact Python names):
//! * token file `resources/auth/gdrive_creds.json` (oauth2client Storage
//!   format: `access_token` / `refresh_token` / `client_id` /
//!   `client_secret` / `token_expiry`), else the DB key
//!   `GDRIVE_AUTH_TOKEN` (Python stores the token file's contents there);
//! * refresh falls back to DB keys `GDRIVE_CLIENT_ID` /
//!   `GDRIVE_CLIENT_SECRET`;
//! * `GDRIVE_FOLDER_ID` (DB key) is the default upload parent.
//! Missing credentials → `Error::Config` naming the exact key/file.
//! Expired access tokens are refreshed via
//! `https://oauth2.googleapis.com/token` and the token file is updated.
//!
//! Boundary notes (see report): uploading a *replied* Telegram file is
//! downloaded via the Responder (`download_media`) first; the assistant-bot `/gd*`
//! stubs and the inline `gdsearch`/`gdlist` handlers are not ported — the
//! registry already serves both `.` and `/` prefixes, so the asst stubs
//! ("use on user account") would collide with the real commands, and
//! inline delivery has no `Ctx` surface; the pagination keyboard on
//! `.gdlist` (>10 files) is omitted (no inline-keyboard surface) and all
//! rows are rendered in the table instead. The folder-upload branch in
//! Python uploads the directory path itself and passes the returned *link*
//! as `folder_id` (a Drive API error in practice); the port creates a
//! real folder and uploads into it.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ultroid_core::CommandDef;
use ultroid_core::fns::{humanbytes, time_formatter};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, def, reg_add};

const HELP: &str = "✘ Commands Available\n\n\
• `.gdul <reply/file>` — upload file/folder to Google Drive. `.gdul -f <folder_id> <file>` — upload to a specific folder.\n\
• `.gdown <file_id/link>` — download from Google Drive. `.gdown <id> | <filename>` — custom filename.\n\
• `.gdsearch <query>` — search files on Google Drive.\n\
• `.gdlist` — list all Drive files in a rich table. `.gdlist -d` — details. `.gdlist -f <folder_id>` — folder.\n\
• `.gdinfo <file_id>` — detailed file info.\n\
• `.gdmove <file_id> <dest_folder_id>` — move file to folder.\n\
• `.gdcp <file_id> <dest_folder_id>` — copy file to folder.\n\
• `.gdtrash <file_id>` — move file to trash.\n\
• `.gddelete <file_id>` — permanently delete file.\n\
• `.gdshare <file_id> <email>` — share file with email.\n\
• `.gdfolder` — show Drive folder link.\n\
• `.gdnewfolder <name>` — create new folder.\n\
• `.gdrefresh` — refresh file cache and list.";

const TOKEN_FILE: &str = "resources/auth/gdrive_creds.json";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DRIVE_FOLDER_MIME: &str = "application/vnd.google-apps.folder";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("gdul", r"gdul(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(gdul_cmd(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("gdown", r"gdown(?:\s+(.*))?$", HELP)
        },
        |ctx| Box::pin(gdown_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdsearch", r"gdsearch(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdsearch_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdlist", r"gdlist(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdlist_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdinfo", r"gdinfo(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdinfo_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdmove", r"gdmove(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdmove_cmd(ctx)),
    );
    reg_add(registry, def("gdcp", r"gdcp(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(gdcp_cmd(ctx))
    });
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("gdtrash", r"gdtrash(?:\s+(.*))?$", HELP)
        },
        |ctx| Box::pin(gdtrash_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("gddelete", r"gddelete(?:\s+(.*))?$", HELP)
        },
        |ctx| Box::pin(gddelete_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdshare", r"gdshare(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdshare_cmd(ctx)),
    );
    reg_add(
        registry,
        def("gdnewfolder", r"gdnewfolder(?:\s+(.*))?$", HELP),
        |ctx| Box::pin(gdnewfolder_cmd(ctx)),
    );
    reg_add(registry, def("gdfolder", "gdfolder$", HELP), |ctx| {
        Box::pin(gdfolder_cmd(ctx))
    });
    reg_add(registry, def("gdrefresh", "gdrefresh$", HELP), |ctx| {
        Box::pin(gdrefresh_cmd(ctx))
    });
}

// ── fmtmsg / fmtbyte (port of `custom/fmt.py:fmtmsg`, `fmtbyte`) ──────────

/// `<b>Head</b>` + one `  <code>Key</code> : value` line per row, keys
/// padded to the longest key — exactly `custom/fmt.py:fmtmsg` for a dict.
fn fmtmsg(head: &str, rows: &[(&str, String)]) -> String {
    let padd = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = format!("<b>{head}</b>");
    for (k, v) in rows {
        out.push_str(&format!("\n  <code>{:<padd$}</code> : {v}", k, padd = padd));
    }
    out
}

fn fmtbyte(size: u64) -> String {
    humanbytes(size)
}

// ── credentials ───────────────────────────────────────────────────────────

struct TokenSet {
    access_token: String,
    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    token_file: Option<PathBuf>,
    /// True when the stored `token_expiry` has passed.
    expired: bool,
}

fn token_expiry_valid(v: &Value) -> bool {
    // oauth2client stores `token_expiry` as an RFC3339 string.
    let Some(exp) = v.get("token_expiry").and_then(|e| e.as_str()) else {
        return true; // no expiry recorded — trust the token, like Python does
    };
    let Ok(exp) = chrono::DateTime::parse_from_rfc3339(exp) else {
        return true;
    };
    exp > chrono::Utc::now()
}

async fn load_tokens(ctx: &Ctx) -> Result<TokenSet> {
    // Token file first (Python: `GDriveManager.token_file`), then the DB
    // key `GDRIVE_AUTH_TOKEN`, which Python fills with the file contents.
    let mut raw: Option<(Value, Option<PathBuf>)> = None;
    let candidates = [
        PathBuf::from(&ctx.config.data_dir).join(TOKEN_FILE),
        PathBuf::from(TOKEN_FILE),
    ];
    for cand in candidates {
        if let Ok(text) = tokio::fs::read_to_string(&cand).await {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                raw = Some((v, Some(cand)));
                break;
            }
        }
    }
    if raw.is_none() {
        if let Some(text) = db_get(ctx, "GDRIVE_AUTH_TOKEN").await? {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                raw = Some((v, None));
            }
        }
    }
    let Some((v, file)) = raw else {
        return Err(Error::Config(
            "No GDrive credentials found — set up GDrive auth first (token file resources/auth/gdrive_creds.json or DB key GDRIVE_AUTH_TOKEN)".to_string(),
        ));
    };
    // Client id/secret fall back to the DB keys Python reads
    // (`GDRIVE_CLIENT_ID` / `GDRIVE_CLIENT_SECRET`).
    let client_id = match v.get("client_id").and_then(|c| c.as_str()) {
        Some(c) => Some(c.to_string()),
        None => db_get(ctx, "GDRIVE_CLIENT_ID").await?,
    };
    let client_secret = match v.get("client_secret").and_then(|c| c.as_str()) {
        Some(c) => Some(c.to_string()),
        None => db_get(ctx, "GDRIVE_CLIENT_SECRET").await?,
    };
    let expired = !token_expiry_valid(&v);
    Ok(TokenSet {
        access_token: v
            .get("access_token")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string(),
        refresh_token: v
            .get("refresh_token")
            .and_then(|r| r.as_str())
            .map(str::to_string),
        client_id,
        client_secret,
        token_file: file,
        expired,
    })
}

/// Resolve a usable access token, refreshing it when expired.
async fn access_token(ctx: &Ctx) -> Result<String> {
    let tokens = load_tokens(ctx).await?;
    if !tokens.access_token.is_empty() && !tokens.expired {
        return Ok(tokens.access_token);
    }
    // Refresh via oauth2.googleapis.com/token.
    let (Some(refresh), Some(cid), Some(secret)) = (
        tokens.refresh_token.clone(),
        tokens.client_id.clone(),
        tokens.client_secret.clone(),
    ) else {
        return Err(Error::Config(
            "GDrive access token expired/missing and no refresh_token + GDRIVE_CLIENT_ID/GDRIVE_CLIENT_SECRET available to refresh it".to_string(),
        ));
    };
    let resp = ctx
        .http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", cid.as_str()),
            ("client_secret", secret.as_str()),
            ("refresh_token", refresh.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "GDrive token refresh failed: HTTP {}",
            resp.status()
        )));
    }
    let v: Value = resp.json().await?;
    let new_token = v
        .get("access_token")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    if new_token.is_empty() {
        return Err(Error::Other(
            "GDrive token refresh returned no access_token".into(),
        ));
    }
    // Persist the refreshed token back into the token file (best effort).
    if let Some(path) = &tokens.token_file {
        if let Ok(text) = tokio::fs::read_to_string(path).await {
            if let Ok(mut j) = serde_json::from_str::<Value>(&text) {
                j["access_token"] = json!(new_token);
                if let Some(expires) = v.get("expires_in").and_then(|e| e.as_i64()) {
                    let exp = chrono::Utc::now() + chrono::Duration::seconds(expires);
                    j["token_expiry"] = json!(exp.to_rfc3339());
                }
                let _ =
                    tokio::fs::write(path, serde_json::to_string_pretty(&j).unwrap_or_default())
                        .await;
            }
        }
    }
    Ok(new_token)
}

async fn folder_id(ctx: &Ctx) -> Result<Option<String>> {
    Ok(db_get(ctx, "GDRIVE_FOLDER_ID")
        .await?
        .filter(|s| !s.is_empty()))
}

// ── Drive v3 client ───────────────────────────────────────────────────────

fn drive_err(status: reqwest::StatusCode, body: &str) -> Error {
    Error::Other(format!("Google Drive API error: HTTP {status}: {body}"))
}

async fn drive_json(
    ctx: &Ctx,
    method: reqwest::Method,
    url: &str,
    body: Option<Value>,
    extra_query: &[(&str, &str)],
) -> Result<Value> {
    let token = access_token(ctx).await?;
    let mut req = ctx
        .http
        .request(method, url)
        .bearer_auth(&token)
        .query(extra_query);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(drive_err(status, &text));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str::<Value>(&text).map_err(Error::from)
}

fn mime_for(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("mp4") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("zip") => "application/zip",
        Some("json") => "application/json",
        Some("html") | Some("htm") => "text/html",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Multipart upload (port of `GDriveManager._upload_file`).
/// The multipart body is built by hand: a JSON metadata part + the file
/// bytes, `multipart/related`. Returns the view/download link.
async fn upload_file(ctx: &Ctx, path: &Path, parent: Option<&str>) -> Result<String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mime = mime_for(path);
    let mut meta = json!({
        "name": name,
        "mimeType": mime,
        "description": "Uploaded using Ultroid Userbot",
    });
    let parent = match parent {
        Some(p) => Some(p.to_string()),
        None => folder_id(ctx).await?,
    };
    if let Some(p) = &parent {
        meta["parents"] = json!([p]);
    }
    let bytes = tokio::fs::read(path).await?;
    let boundary = format!("ultroid-{}", rand::random::<u64>());
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(serde_json::to_string(&meta).unwrap_or_default().as_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let token = access_token(ctx).await?;
    let resp = ctx
        .http
        .post(UPLOAD_API)
        .query(&[
            ("uploadType", "multipart"),
            ("fields", "id,name,webContentLink,webViewLink"),
        ])
        .bearer_auth(&token)
        .header(
            "Content-Type",
            format!("multipart/related; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(drive_err(status, &text));
    }
    let v: Value = serde_json::from_str(&text)?;
    let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
    // Python `_set_permissions` makes the upload anyone-with-link readable
    // (best effort, failures swallowed).
    if !id.is_empty() {
        let _ = drive_json(
            ctx,
            reqwest::Method::POST,
            &format!("{DRIVE_API}/files/{id}/permissions"),
            Some(
                json!({"role": "reader", "type": "anyone", "value": Value::Null, "withLink": true}),
            ),
            &[],
        )
        .await;
    }
    Ok(v.get("webContentLink")
        .or_else(|| v.get("webViewLink"))
        .and_then(|l| l.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://drive.google.com/file/d/{id}/view")))
}

/// Download a file (port of `GDriveManager._download_file`).
/// `file_id` may be an id or a Drive link (Python's `=download` / `/view`
/// parsing). Returns the saved path.
async fn download_file(ctx: &Ctx, file_id: &str, filename: Option<&str>) -> Result<PathBuf> {
    let id = parse_drive_id(file_id);
    let name = match filename {
        Some(f) => f.to_string(),
        None => {
            let meta = drive_json(
                ctx,
                reqwest::Method::GET,
                &format!("{DRIVE_API}/files/{id}"),
                None,
                &[("fields", "name")],
            )
            .await?;
            meta.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("file")
                .to_string()
        }
    };
    let token = access_token(ctx).await?;
    let resp = ctx
        .http
        .get(format!("{DRIVE_API}/files/{id}"))
        .query(&[("alt", "media")])
        .bearer_auth(&token)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(drive_err(status, &text));
    }
    let bytes = resp.bytes().await?;
    let dir = PathBuf::from(&ctx.config.data_dir).join("resources/downloads");
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(&name);
    tokio::fs::write(&dest, &bytes).await?;
    Ok(dest)
}

/// Port of the id extraction in `GDriveManager._download_file`.
fn parse_drive_id(file_id: &str) -> String {
    if file_id.starts_with("http") {
        if file_id.contains("=download") {
            // `https://drive.google.com/uc?id=<ID>&export=download`
            if let Some(after_eq) = file_id.split('=').nth(1) {
                let trimmed = after_eq.strip_suffix("&export").unwrap_or(after_eq);
                return trimmed.to_string();
            }
        } else if file_id.contains("/view") {
            // `https://drive.google.com/file/d/<ID>/view`
            let parts: Vec<&str> = file_id.split('/').collect();
            if parts.len() >= 2 {
                return parts[parts.len() - 2].to_string();
            }
        }
    }
    file_id.to_string()
}

/// List files (port of `GDriveManager._list_files` / `.search`).
/// Returns `(id, name, mime, size, modified)` rows.
async fn list_files(
    ctx: &Ctx,
    folder: Option<&str>,
    query: Option<&str>,
) -> Result<Vec<(String, String, String, u64, String)>> {
    let mut q_parts: Vec<String> = Vec::new();
    if let Some(f) = folder {
        q_parts.push(format!("'{f}' in parents"));
    }
    if let Some(t) = query {
        let safe = t.replace('\'', "");
        q_parts.push(format!("name contains '{safe}'"));
    }
    q_parts.push("trashed = false".to_string());
    let q = q_parts.join(" and ");
    let v = drive_json(
        ctx,
        reqwest::Method::GET,
        &format!("{DRIVE_API}/files"),
        None,
        &[
            ("q", q.as_str()),
            ("fields", "files(id,name,mimeType,size,modifiedTime)"),
            ("pageSize", "100"),
        ],
    )
    .await?;
    let mut out = Vec::new();
    if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
        for f in files {
            out.push((
                f.get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string(),
                f.get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("Unknown")
                    .to_string(),
                f.get("mimeType")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                f.get("size")
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0),
                f.get("modifiedTime")
                    .and_then(|m| m.as_str())
                    .unwrap_or("N/A")
                    .to_string(),
            ));
        }
    }
    Ok(out)
}

fn mime_icon(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "📄",
        "text/plain" => "📃",
        "image/jpeg" | "image/png" => "🖼️",
        "image/gif" => "🎞️",
        "video/mp4" | "video/avi" => "🎬",
        "audio/mp3" | "audio/mpeg" => "🎵",
        "application/vnd.google-apps.folder" => "📁",
        "application/zip" | "application/x-rar-compressed" => "📦",
        _ => "📎",
    }
}

fn build_file_table(files: &[(String, String, String, u64, String)], details: bool) -> String {
    if files.is_empty() {
        return "<b>📭 No files found.</b>".to_string();
    }
    let mut rows = String::new();
    for (idx, (id, name, mime, size, modified)) in files.iter().enumerate() {
        let icon = mime_icon(mime);
        if details {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{icon} <code>{name}</code></td><td>{}</td><td><code>{mime}</code></td><td>{modified}</td></tr>",
                idx + 1,
                fmtbyte(*size)
            ));
        } else {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{icon} <code>{name}</code></td><td>{}</td><td>{id}</td></tr>",
                idx + 1,
                fmtbyte(*size)
            ));
        }
    }
    let header = if details {
        "<table border='1' cellpadding='5' cellspacing='0' style='border-collapse:collapse;width:100%'><tr style='background:#2ECC71'><th>#</th><th>File</th><th>Size</th><th>Type</th><th>Modified</th></tr>"
    } else {
        "<table border='1' cellpadding='5' cellspacing='0' style='border-collapse:collapse;width:100%'><tr style='background:#2ECC71'><th>#</th><th>File</th><th>Size</th><th>ID</th></tr>"
    };
    format!("{header}{rows}</table>")
}

/// Reply helper: Python's `eor` on an HTML `fmtmsg` — a status message
/// that is then edited. Here: send the HTML, return the message id.
async fn status_msg(ctx: &Ctx, title: &str, rows: &[(&str, String)]) -> Result<i64> {
    ctx.reply_html(&fmtmsg(title, rows)).await
}

async fn require_creds(ctx: &Ctx) -> Result<()> {
    // Touch the token so a missing setup fails early with `Error::Config`.
    access_token(ctx).await?;
    Ok(())
}

/// Extension guessed from a file's magic bytes (`None` when unrecognised).
fn sniff_extension(path: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut buf = [0u8; 12];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    let b = &buf[..n];
    if b.starts_with(b"%PDF") {
        Some("pdf")
    } else if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else if b.starts_with(b"RIFF") && b.len() >= 12 && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"RIFF") {
        Some("wav")
    } else if b.len() >= 8 && &b[4..8] == b"ftyp" {
        Some("mp4")
    } else if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("mkv")
    } else if b.starts_with(b"OggS") {
        Some("ogg")
    } else if b.starts_with(b"ID3") || b.starts_with(&[0xFF, 0xFB]) {
        Some("mp3")
    } else if b.starts_with(b"PK\x03\x04") {
        Some("zip")
    } else {
        None
    }
}

/// Rename `path` in place to carry the extension sniffed from its magic
/// bytes, when recognised — the Drive upload names the file after it.
fn with_sniffed_ext(path: &Path) -> PathBuf {
    match sniff_extension(path) {
        Some(ext) => {
            let renamed = path.with_extension(ext);
            if std::fs::rename(path, &renamed).is_ok() {
                renamed
            } else {
                path.to_path_buf()
            }
        }
        None => path.to_path_buf(),
    }
}

// ── commands ──────────────────────────────────────────────────────────────

async fn gdul_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let mut folder_id_arg: Option<String> = None;
    let mut file_path: Option<String> = None;
    if let Some(rest) = args.strip_prefix("-f ") {
        let mut parts = rest.splitn(2, char::is_whitespace);
        folder_id_arg = parts.next().map(str::to_string);
        file_path = parts.next().map(|s| s.trim().to_string());
        if folder_id_arg.is_none() || file_path.as_deref().map(str::is_empty).unwrap_or(true) {
            ctx.reply_html(&fmtmsg(
                "GDrive",
                &[("Usage", "gdul -f <folder_id> <file>".to_string())],
            ))
            .await?;
            return Ok(());
        }
    } else if !args.is_empty() {
        file_path = Some(args);
    }

    let mut downloaded: Option<PathBuf> = None;
    if file_path.is_none() && ctx.reply_to_id.is_some() {
        // Download the replied Telegram file via the Responder and upload
        // it (Python: gdrive.py:gdul).
        let reply_id = ctx.reply_to_id.unwrap_or(0);
        let dest = std::env::temp_dir().join(format!("ultroid-gdrive-{}", uuid::Uuid::new_v4()));
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        let path = with_sniffed_ext(&path);
        file_path = Some(path.to_string_lossy().into_owned());
        downloaded = Some(path);
    }
    let Some(file_path) = file_path else {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[(
                "Usage",
                ".gdul [-f <folder_id>] <file> | reply to file".to_string(),
            )],
        ))
        .await?;
        return Ok(());
    };

    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Uploading...".to_string())]).await?;
    let start = std::time::Instant::now();
    let path = PathBuf::from(&file_path);

    if path.is_dir() {
        let mut files: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&path) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    files.push(p);
                }
            }
        }
        files.sort();
        if files.is_empty() {
            ctx.edit(
                status_id,
                &fmtmsg("GDrive", &[("Error", "Empty directory".to_string())]),
            )
            .await?;
            return Ok(());
        }
        // Create a real Drive folder for the directory (see module docs:
        // Python passes the upload *link* as `folder_id` here).
        let dir_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "folder".to_string());
        let folder = drive_json(
            ctx,
            reqwest::Method::POST,
            &format!("{DRIVE_API}/files"),
            Some(json!({"name": dir_name, "mimeType": DRIVE_FOLDER_MIME})),
            &[("fields", "id")],
        )
        .await?;
        let new_folder_id = folder
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        let mut count = 0usize;
        for f in &files {
            if upload_file(ctx, f, Some(&new_folder_id)).await.is_ok() {
                count += 1;
            }
        }
        ctx.edit(
            status_id,
            &fmtmsg(
                "GDrive",
                &[
                    ("Status", "✅ Folder uploaded".to_string()),
                    ("Files", count.to_string()),
                    (
                        "Link",
                        format!("<a href='https://drive.google.com/folderview?id={new_folder_id}'>View Folder</a>"),
                    ),
                ],
            ),
        )
        .await?;
        return Ok(());
    }

    match upload_file(ctx, &path, folder_id_arg.as_deref()).await {
        Ok(link) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Uploaded".to_string()),
                        ("Link", format!("<a href='{link}'>View on Drive</a>")),
                        ("Time", time_formatter(start.elapsed().as_millis() as u64)),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    if let Some(p) = downloaded {
        let _ = tokio::fs::remove_file(&p).await;
    }
    Ok(())
}

async fn gdown_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    if args.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdown <file_id/link>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    let (id, filename) = match args.split_once(" | ") {
        Some((a, b)) => (a.trim().to_string(), Some(b.trim().to_string())),
        None => (args, None),
    };
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Downloading...".to_string())]).await?;
    let start = std::time::Instant::now();
    match download_file(ctx, &id, filename.as_deref()).await {
        Ok(path) => {
            let elapsed = time_formatter(start.elapsed().as_millis() as u64);
            // Python stops at the status message; the file stays on disk
            // (it does not send it back to the chat).
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Downloaded".to_string()),
                        ("File", format!("`{}`", path.display())),
                        ("Time", elapsed),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdsearch_cmd(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim().to_string();
    if query.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdsearch <query>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    require_creds(ctx).await?;
    let status_id = status_msg(
        ctx,
        "GDrive",
        &[("Status", format!("Searching: <code>{query}</code>..."))],
    )
    .await?;
    match list_files(ctx, None, Some(&query)).await {
        Ok(files) => {
            if files.is_empty() {
                ctx.edit(
                    status_id,
                    &fmtmsg("GDrive", &[("Status", "📭 No results".to_string())]),
                )
                .await?;
                return Ok(());
            }
            let table = build_file_table(&files, false);
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive Search",
                    &[
                        ("Query", format!("`{query}`")),
                        ("Results", files.len().to_string()),
                        ("Table", table),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdlist_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let details = args.starts_with("-d ");
    let folder = args.split_once("-f ").map(|(_, f)| f.trim().to_string());
    require_creds(ctx).await?;
    let status_id = status_msg(
        ctx,
        "GDrive",
        &[("Status", "Fetching files...".to_string())],
    )
    .await?;
    match list_files(ctx, folder.as_deref(), None).await {
        Ok(files) => {
            let table = build_file_table(&files, details);
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive Files",
                    &[
                        ("Count", files.len().to_string()),
                        ("Details", if details { "✅" } else { "❌" }.to_string()),
                        ("Table", table),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdinfo_cmd(ctx: &Ctx) -> Result<()> {
    let file_id = ctx.args.trim().to_string();
    if file_id.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdinfo <file_id>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    require_creds(ctx).await?;
    let status_id = status_msg(
        ctx,
        "GDrive",
        &[("Status", "Fetching details...".to_string())],
    )
    .await?;
    let details = drive_json(
        ctx,
        reqwest::Method::GET,
        &format!("{DRIVE_API}/files/{file_id}"),
        None,
        &[("fields", "*")],
    )
    .await;
    match details {
        Ok(d) => {
            let name = d.get("name").and_then(|n| n.as_str()).unwrap_or("Unknown");
            let mime = d
                .get("mimeType")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            let size = d
                .get("size")
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let modified = d
                .get("modifiedTime")
                .and_then(|m| m.as_str())
                .unwrap_or("N/A");
            let created = d
                .get("createdTime")
                .and_then(|c| c.as_str())
                .unwrap_or("N/A");
            let owned = d.get("ownedByMe").and_then(|o| o.as_bool()).unwrap_or(true);
            let trashed = d.get("trashed").and_then(|t| t.as_bool()).unwrap_or(false);
            let copyable = d
                .get("viewersCanCopyContent")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let icon = mime_icon(mime);
            let rows = [
                ("Name", format!("{icon} <b>{name}</b>")),
                ("Type", format!("<code>{mime}</code>")),
                ("Size", fmtbyte(size)),
                ("Modified", modified.to_string()),
                ("Created", created.to_string()),
                (
                    "Owner",
                    if owned { "✅ You" } else { "❌ Shared" }.to_string(),
                ),
                (
                    "Trashed",
                    if trashed { "✅ Yes" } else { "❌ No" }.to_string(),
                ),
                ("Copyable", if copyable { "✅" } else { "❌" }.to_string()),
            ];
            let body = fmtmsg("GDrive File", &rows);
            ctx.edit(status_id, &body).await?;
        }
        Err(_) => {
            ctx.edit(
                status_id,
                &fmtmsg("GDrive", &[("Error", "File not found".to_string())]),
            )
            .await?;
        }
    }
    Ok(())
}

async fn gdmove_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let Some((file_id, dest)) = args.split_once(char::is_whitespace) else {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdmove <file_id> <dest_folder_id>".to_string())],
        ))
        .await?;
        return Ok(());
    };
    let dest = dest.trim();
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Moving...".to_string())]).await?;
    let current = drive_json(
        ctx,
        reqwest::Method::GET,
        &format!("{DRIVE_API}/files/{file_id}"),
        None,
        &[("fields", "parents")],
    )
    .await;
    let result = match current {
        Ok(c) => {
            let old_parents = c
                .get("parents")
                .and_then(|p| p.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            drive_json(
                ctx,
                reqwest::Method::PATCH,
                &format!("{DRIVE_API}/files/{file_id}"),
                None,
                &[
                    ("addParents", dest),
                    ("removeParents", old_parents.as_str()),
                    ("fields", "id,parents"),
                ],
            )
            .await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(_) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Moved".to_string()),
                        ("File", format!("`{file_id}`")),
                        ("To", format!("`{dest}`")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdcp_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let Some((file_id, dest)) = args.split_once(char::is_whitespace) else {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdcp <file_id> <dest_folder_id>".to_string())],
        ))
        .await?;
        return Ok(());
    };
    let dest = dest.trim();
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Copying...".to_string())]).await?;
    // Python's body is literally `{"name": file_id, "parents": [dest]}` —
    // the copy is named after the source id. Kept faithful, flagged in report.
    let result = drive_json(
        ctx,
        reqwest::Method::POST,
        &format!("{DRIVE_API}/files/{file_id}/copy"),
        Some(json!({"name": file_id, "parents": [dest]})),
        &[],
    )
    .await;
    match result {
        Ok(_) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Copied".to_string()),
                        ("File", format!("`{file_id}`")),
                        ("To", format!("`{dest}`")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdtrash_cmd(ctx: &Ctx) -> Result<()> {
    let file_id = ctx.args.trim().to_string();
    if file_id.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdtrash <file_id>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Trashing...".to_string())]).await?;
    let result = drive_json(
        ctx,
        reqwest::Method::PATCH,
        &format!("{DRIVE_API}/files/{file_id}"),
        Some(json!({"trashed": true})),
        &[],
    )
    .await;
    match result {
        Ok(_) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "🗑️ Trashed".to_string()),
                        ("File", format!("`{file_id}`")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gddelete_cmd(ctx: &Ctx) -> Result<()> {
    let file_id = ctx.args.trim().to_string();
    if file_id.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gddelete <file_id>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Deleting...".to_string())]).await?;
    let result = drive_json(
        ctx,
        reqwest::Method::DELETE,
        &format!("{DRIVE_API}/files/{file_id}"),
        None,
        &[],
    )
    .await;
    match result {
        Ok(_) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "❌ Deleted permanently".to_string()),
                        ("File", format!("`{file_id}`")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdshare_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let Some((file_id, email)) = args.split_once(char::is_whitespace) else {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdshare <file_id> <email>".to_string())],
        ))
        .await?;
        return Ok(());
    };
    let email = email.trim();
    require_creds(ctx).await?;
    let status_id = status_msg(
        ctx,
        "GDrive",
        &[("Status", format!("Sharing with {email}..."))],
    )
    .await?;
    let result = drive_json(
        ctx,
        reqwest::Method::POST,
        &format!("{DRIVE_API}/files/{file_id}/permissions"),
        Some(json!({"role": "reader", "type": "user", "value": email})),
        &[],
    )
    .await;
    match result {
        Ok(_) => {
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Shared".to_string()),
                        ("File", format!("`{file_id}`")),
                        ("With", format!("`{email}`")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdnewfolder_cmd(ctx: &Ctx) -> Result<()> {
    let name = ctx.args.trim().to_string();
    if name.is_empty() {
        ctx.reply_html(&fmtmsg(
            "GDrive",
            &[("Usage", ".gdnewfolder <name>".to_string())],
        ))
        .await?;
        return Ok(());
    }
    require_creds(ctx).await?;
    let status_id = status_msg(
        ctx,
        "GDrive",
        &[("Status", format!("Creating folder: <code>{name}</code>..."))],
    )
    .await?;
    let result = drive_json(
        ctx,
        reqwest::Method::POST,
        &format!("{DRIVE_API}/files"),
        Some(json!({"name": name, "mimeType": DRIVE_FOLDER_MIME})),
        &[("fields", "id")],
    )
    .await;
    match result {
        Ok(folder) => {
            let folder_id = folder
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown");
            let link = format!("https://drive.google.com/folderview?id={folder_id}");
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive",
                    &[
                        ("Status", "✅ Folder created".to_string()),
                        ("Name", format!("`{name}`")),
                        ("ID", format!("`{folder_id}`")),
                        ("Link", format!("<a href='{link}'>Open Folder</a>")),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}

async fn gdfolder_cmd(ctx: &Ctx) -> Result<()> {
    match folder_id(ctx).await? {
        Some(fid) => {
            let link = format!("https://drive.google.com/folderview?id={fid}");
            ctx.reply_html(&fmtmsg(
                "GDrive",
                &[
                    ("Status", "📁 Folder link".to_string()),
                    ("Link", format!("<a href='{link}'>{link}</a>")),
                    ("ID", format!("`{fid}`")),
                ],
            ))
            .await?;
        }
        None => {
            ctx.reply_html(&fmtmsg(
                "GDrive",
                &[("Info", "No folder ID set. Set GDRIVE_FOLDER_ID".to_string())],
            ))
            .await?;
        }
    }
    Ok(())
}

async fn gdrefresh_cmd(ctx: &Ctx) -> Result<()> {
    require_creds(ctx).await?;
    let status_id = status_msg(ctx, "GDrive", &[("Status", "Refreshing...".to_string())]).await?;
    match list_files(ctx, None, None).await {
        Ok(files) => {
            let table = build_file_table(&files, false);
            ctx.edit(
                status_id,
                &fmtmsg(
                    "GDrive Files",
                    &[
                        ("Status", "✅ Refreshed".to_string()),
                        ("Count", files.len().to_string()),
                        ("Table", table),
                    ],
                ),
            )
            .await?;
        }
        Err(e) => {
            ctx.edit(status_id, &fmtmsg("GDrive", &[("Error", e.to_string())]))
                .await?;
        }
    }
    Ok(())
}
