//! Queue-aware download/upload — port of `plugins/downloadupload.py`
//! (986 LOC, SarahUB Phase 2.1 rewrite).
//!
//! `.download` / `.dl` / `.ul` / `.fcancel`.
//!
//! What is real here:
//! * argument/flag parsing (`shlex`-compatible tokenizer, value + boolean
//!   flags, unknown-flag rejection) and the t.me link parser;
//! * the queue tracker: batches (GID) + per-file entries (FID) with live
//!   status-table edits, flood-guarded at 1.5 s — a faithful in-module
//!   port of `pyUltroid/_core/queue.py` (`QueueManager`) semantics,
//!   because `ultroid-core::queue::QueueManager` is only a FIFO and has
//!   no GID/status surface;
//! * `.download` direct-URL downloads via `reqwest` (streaming, progress
//!   edits, smart filename from `Content-Disposition` / `Content-Type` —
//!   port of `fns.get_smart_name` / `fns.fast_download`);
//! * `.ul` local file / directory uploads via `Ctx::send_file`, with glob
//!   expansion, the ascending size table, per-file queue tracking and the
//!   `-n` / `-del` / `--stream` / `--no-thumb` flags;
//! * `.fcancel` against the tracker (owner/sudo only).
//!
//! What is gated on the Responder (TODO(responder-op)): fetching Telegram
//! messages (`.dl` reply / t.me link / album resolution); `.download`'s
//! replied `.txt` of URLs is downloaded via the Responder
//! (`download_media`). The `.dl` handlers validate their flags first,
//! then fail honestly at that fetch point.
//! The owner-PM notification (`_pm_owner`) is best-effort in Python and
//! has no equivalent on `Ctx` (the responder only talks to the trigger
//! chat), so it is skipped, not faked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use ultroid_core::fns::{humanbytes, time_formatter};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, def, reg_add};

const HELP: &str = "Queue-aware download/upload plugin.\n\n\
• `.dl` — download Telegram media by reply, or by t.me link (single or `start-end` range). Flags: `-t`, `-ttl <s>`, `-n <name>`, `-d <dir>`, `-del`.\n\
• `.ul` — upload a local file or directory (globs supported). Flags: `-n <name>`, `-del`/`--delete`, `--stream`, `--no-thumb`.\n\
• `.download` — download a direct URL, or reply to a .txt of URLs for a bulk run. Same `-n`, `-t`, `-ttl`, `-d` flags.\n\
• `.fcancel <GID|FID>` — cancel a queued batch or a single file. Owner/sudo only.";

const DOWNLOAD_DIR: &str = "resources/downloads";
const TEMP_TTL_DEFAULT: f64 = 3600.0;
const MAX_BULK: usize = 100;

const DL_VALUE_FLAGS: &[&str] = &["-n", "-ttl", "-d"];
const DL_BOOL_FLAGS: &[&str] = &["-t", "-del"];
const UL_VALUE_FLAGS: &[&str] = &["-n"];
const UL_BOOL_FLAGS: &[&str] = &["--stream", "--no-thumb", "-del", "--delete"];
const DW_VALUE_FLAGS: &[&str] = &["-n", "-ttl", "-d"];
const DW_BOOL_FLAGS: &[&str] = &["-t"];

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("download", "download( (.*)|$)", HELP),
        |ctx| Box::pin(download_cmd(ctx)),
    );
    reg_add(registry, def("dl", "dl( (.*)|$)", HELP), |ctx| {
        Box::pin(dl_cmd(ctx))
    });
    reg_add(registry, def("ul", "ul( (.*)|$)", HELP), |ctx| {
        Box::pin(ul_cmd(ctx))
    });
    reg_add(registry, def("fcancel", "fcancel( (.*)|$)", HELP), |ctx| {
        Box::pin(fcancel_cmd(ctx))
    });
}

// ── shell-like tokeniser (port of `shlex.split`) ──────────────────────────

fn shell_split(text: &str) -> std::result::Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    let mut has_token = false;
    while let Some(c) = chars.next() {
        if escaped {
            cur.push(c);
            escaped = false;
        } else if c == '\\' && !in_single {
            escaped = true;
        } else if c == '\'' && !in_double {
            in_single = !in_single;
            has_token = true;
        } else if c == '"' && !in_single {
            in_double = !in_double;
            has_token = true;
        } else if (c == ' ' || c == '\t' || c == '\n') && !in_single && !in_double {
            if has_token || !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
                has_token = false;
            }
        } else {
            cur.push(c);
            has_token = true;
        }
    }
    if in_single || in_double {
        return Err("Quoting error: no closing quotation".to_string());
    }
    if has_token || !cur.is_empty() {
        tokens.push(cur);
    }
    Ok(tokens)
}

// ── flag parsing (port of `_parse_flags`) ─────────────────────────────────

struct ParsedFlags {
    flags: HashMap<String, String>,
    bools: Vec<String>,
    positionals: Vec<String>,
    unknowns: Vec<String>,
}

fn parse_flags(
    text: &str,
    value_flags: &[&str],
    bool_flags: &[&str],
) -> std::result::Result<ParsedFlags, String> {
    let tokens = shell_split(text)?;
    let mut flags = HashMap::new();
    let mut bools = Vec::new();
    let mut positionals = Vec::new();
    let mut unknowns = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if value_flags.contains(&tok.as_str()) {
            if i + 1 < tokens.len() {
                flags.insert(tok.clone(), tokens[i + 1].clone());
                i += 2;
            } else {
                unknowns.push(format!("{tok} (missing value)"));
                i += 1;
            }
        } else if bool_flags.contains(&tok.as_str()) {
            bools.push(tok.clone());
            i += 1;
        } else if tok.starts_with('-') {
            unknowns.push(tok.clone());
            i += 1;
        } else {
            positionals.push(tok.clone());
            i += 1;
        }
    }
    Ok(ParsedFlags {
        flags,
        bools,
        positionals,
        unknowns,
    })
}

fn flag_error_text(cmd: &str, unknowns: &[String], valid: &[&str]) -> String {
    let mut valid: Vec<&str> = valid.to_vec();
    valid.sort_unstable();
    format!(
        "`Unrecognized argument(s):` {}\n`Valid flags for .{cmd}:` {}\n`Tip:` quote names/paths with spaces, e.g. `.dl -n \"My File\"`",
        unknowns.join(", "),
        valid.join(", ")
    )
}

fn has_bool(p: &ParsedFlags, flag: &str) -> bool {
    p.bools.iter().any(|b| b == flag)
}

// ── names / sizes / temp files ────────────────────────────────────────────

/// Port of `fns.check_filename` (`pyUltroid/fns/tools.py:391`): if the
/// path exists, append `_1`, `_2`, … before the extension.
fn check_filename(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new(""));
    let mut n = 1u32;
    loop {
        let cand = parent.join(format!("{stem}_{n}{ext}"));
        if !cand.exists() {
            return cand;
        }
        n += 1;
    }
}

fn mime_extension(mime: &str) -> Option<&'static str> {
    // Port of `mimetypes.guess_extension` for the types that matter here.
    match mime.split(';').next().unwrap_or("").trim() {
        "audio/mpeg" => Some(".mp3"),
        "audio/ogg" => Some(".ogg"),
        "audio/x-wav" | "audio/wav" => Some(".wav"),
        "audio/mp4" => Some(".m4a"),
        "video/mp4" => Some(".mp4"),
        "video/x-matroska" => Some(".mkv"),
        "video/webm" => Some(".webm"),
        "video/x-msvideo" => Some(".avi"),
        "image/jpeg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "application/pdf" => Some(".pdf"),
        "text/plain" => Some(".txt"),
        "application/zip" => Some(".zip"),
        "application/x-rar-compressed" => Some(".rar"),
        "application/octet-stream" => Some(".bin"),
        _ => None,
    }
}

/// Port of `_resolve_name`: a custom name carrying an extension is used
/// verbatim; a bare name gets the original file's extension (from the
/// original name or the MIME type).
fn resolve_name(
    custom: Option<&str>,
    original: Option<&str>,
    mime: Option<&str>,
) -> Option<String> {
    let custom = custom?;
    let name = custom.trim();
    if name.is_empty() {
        return original.map(str::to_string);
    }
    if Path::new(name).extension().is_some() {
        return Some(name.to_string());
    }
    let ext = original
        .and_then(|o| {
            Path::new(o)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
        })
        .or_else(|| mime.and_then(mime_extension).map(str::to_string))
        .unwrap_or_default();
    Some(format!("{name}{ext}"))
}

async fn temp_ttl(ctx: &Ctx, flags: &ParsedFlags) -> Option<f64> {
    if let Some(raw) = flags.flags.get("-ttl") {
        return raw.parse::<f64>().ok().filter(|v| *v > 0.0);
    }
    match db_get(ctx, "TEMP_TTL").await {
        Ok(Some(v)) => v
            .parse::<f64>()
            .ok()
            .filter(|x| *x > 0.0)
            .or(Some(TEMP_TTL_DEFAULT)),
        _ => Some(TEMP_TTL_DEFAULT),
    }
}

fn mark_temp(path: PathBuf, ttl: f64) {
    if ttl <= 0.0 {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs_f64(ttl)).await;
        let _ = tokio::fs::remove_file(&path).await;
    });
}

fn post_download(path: PathBuf, flags: &ParsedFlags, ttl: f64) -> PathBuf {
    let mut path = path;
    if let Some(dest) = flags.flags.get("-d") {
        let dir = PathBuf::from(dest);
        if std::fs::create_dir_all(&dir).is_ok() {
            let final_path = dir.join(path.file_name().unwrap_or_default());
            if final_path != path {
                if std::fs::rename(&path, &final_path).is_ok() {
                    path = final_path;
                }
            }
        }
    }
    if has_bool(flags, "-t") || flags.flags.contains_key("-ttl") {
        mark_temp(path.clone(), ttl);
    }
    path
}

// ── t.me link parser (port of `_parse_tg_link`) ───────────────────────────

fn parse_tg_link(link: &str) -> (Option<i64>, Vec<i64>) {
    // `https?://t.me/(c/)?<chat>/(?:pg/)?<start>(?:-<end>)?`
    let re = Regex::new(r"https?://t\.me/(c/)?([^/\s]+)/(?:pg/)?(\d+)(?:-(\d+))?")
        .expect("tg link regex");
    let Some(cap) = re.captures(link) else {
        return (None, vec![]);
    };
    let is_private = cap.get(1).is_some();
    let chat_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
    let chat: Option<i64> = if is_private {
        format!("-100{chat_raw}").parse::<i64>().ok()
    } else {
        None
    };
    // Public usernames cannot be resolved to a chat id from `Ctx`; the
    // caller reports the link as unrecognised only when the id form is
    // needed. A private `c/` link yields a real id.
    let start: i64 = cap
        .get(3)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    let end: Option<i64> = cap.get(4).and_then(|m| m.as_str().parse().ok());
    let ids = match end {
        Some(mut e) => {
            let mut s = start;
            if e < s {
                std::mem::swap(&mut s, &mut e);
            }
            (s..=e).collect()
        }
        None => vec![start],
    };
    (chat, ids)
}

// ── queue tracker (port of `pyUltroid/_core/queue.py` GID/FID semantics) ─

#[derive(Debug, Clone)]
struct FileState {
    fid: String,
    status: String,
    size: u64,
    name: String,
}

#[derive(Debug)]
struct Batch {
    gid: String,
    operation: String,
    files: Vec<FileState>,
    last_refresh: Instant,
}

struct Tracker {
    batches: HashMap<String, Batch>,
}

static TRACKER: OnceLock<Mutex<Tracker>> = OnceLock::new();

fn tracker() -> &'static Mutex<Tracker> {
    TRACKER.get_or_init(|| {
        Mutex::new(Tracker {
            batches: HashMap::new(),
        })
    })
}

fn new_gid() -> String {
    format!("{:08x}", rand::random::<u32>())
}

fn op_words(operation: &str) -> (&'static str, &'static str) {
    match operation {
        "ul" | "upload" | "up" | "u" => ("upload", "batch"),
        _ => ("download", "bulk"),
    }
}

async fn enqueue_batch(operation: &str, files: Vec<(String, u64)>) -> String {
    let gid = new_gid();
    let mut t = tracker().lock().await;
    let batch = Batch {
        gid: gid.clone(),
        operation: operation.to_string(),
        files: files
            .into_iter()
            .enumerate()
            .map(|(i, (name, size))| FileState {
                fid: format!("{gid}-{i}"),
                status: "queued".to_string(),
                size,
                name,
            })
            .collect(),
        last_refresh: Instant::now() - Duration::from_secs(10),
    };
    t.batches.insert(gid.clone(), batch);
    gid
}

async fn file_status(fid: &str) -> Option<String> {
    let (gid, _) = fid.rsplit_once('-')?;
    let t = tracker().lock().await;
    t.batches
        .get(gid)?
        .files
        .iter()
        .find(|f| f.fid == fid)
        .map(|f| f.status.clone())
}

async fn set_file_status(fid: &str, status: &str) {
    let Some((gid, _)) = fid.rsplit_once('-') else {
        return;
    };
    let mut t = tracker().lock().await;
    if let Some(b) = t.batches.get_mut(gid) {
        if let Some(f) = b.files.iter_mut().find(|f| f.fid == fid) {
            f.status = status.to_string();
        }
    }
}

/// Cancel a GID (whole batch) or FID (single file). Returns true if the
/// target was found. Port of `QueueManager.cancel`.
async fn cancel_target(target: &str) -> bool {
    let mut t = tracker().lock().await;
    if target.contains('-') {
        if let Some((gid, _)) = target.rsplit_once('-') {
            if let Some(b) = t.batches.get_mut(gid) {
                if let Some(f) = b.files.iter_mut().find(|f| f.fid == target) {
                    if f.status != "done" {
                        f.status = "cancelled".to_string();
                    }
                    return true;
                }
            }
        }
        return false;
    }
    match t.batches.get_mut(target) {
        Some(b) => {
            for f in b.files.iter_mut() {
                if f.status != "done" {
                    f.status = "cancelled".to_string();
                }
            }
            true
        }
        None => false,
    }
}

async fn batch_summary(gid: &str) -> Option<(String, usize, usize, usize, usize, usize)> {
    let t = tracker().lock().await;
    let b = t.batches.get(gid)?;
    let total = b.files.len();
    let done = b.files.iter().filter(|f| f.status == "done").count();
    let failed = b.files.iter().filter(|f| f.status == "failed").count();
    let cancelled = b.files.iter().filter(|f| f.status == "cancelled").count();
    let active = total - done - failed - cancelled;
    Some((b.operation.clone(), total, done, failed, cancelled, active))
}

fn format_tracker(b: &Batch) -> String {
    let (verb, mode) = op_words(&b.operation);
    let total = b.files.len();
    let done = b.files.iter().filter(|f| f.status == "done").count();
    let failed = b.files.iter().filter(|f| f.status == "failed").count();
    let cancelled = b.files.iter().filter(|f| f.status == "cancelled").count();
    let active = total - done - failed - cancelled;
    let progress = if total > 0 { done * 100 / total } else { 0 };
    let mut verb_cap = verb.to_string();
    verb_cap[..1].make_ascii_uppercase();
    let mut lines = vec![
        format!("{verb_cap} {mode} — GID: {}", b.gid),
        format!("Progress: {done}/{total} ({progress}%)"),
        format!("Done: {done} | Active: {active} | Failed: {failed} | Cancelled: {cancelled}"),
        String::new(),
        format!("{:<14} {:<12} {:<10} {}", "FID", "Status", "Size", "Name"),
    ];
    for f in &b.files {
        lines.push(format!(
            "{:<14} {:<12} {:<10} {}",
            f.fid,
            f.status,
            humanbytes(f.size),
            f.name
        ));
    }
    format!("```\n{}\n```", lines.join("\n"))
}

/// Flood-guarded tracker edit (port of `_refresh_tracker`, 1.5 s guard).
async fn refresh_tracker(ctx: &Ctx, msg_id: i64, gid: &str, force: bool) -> Result<()> {
    let now = Instant::now();
    let text = {
        let mut t = tracker().lock().await;
        let Some(b) = t.batches.get_mut(gid) else {
            return Ok(());
        };
        if !force && now.duration_since(b.last_refresh) < Duration::from_millis(1500) {
            return Ok(());
        }
        b.last_refresh = now;
        format_tracker(b)
    };
    // Telegram rejects an edit that changes nothing; treat it as a no-op.
    if ctx.edit(msg_id, &text).await.is_err() {
        // Best-effort: the tracker table is informational.
    }
    Ok(())
}

fn bulk_summary(
    operation: &str,
    gid: &str,
    total: usize,
    done: usize,
    failed: usize,
    cancelled: usize,
) -> String {
    let (verb, mode) = op_words(operation);
    let mut v = verb.to_string();
    v[..1].make_ascii_uppercase();
    format!(
        "{v} {mode} finished — GID `{gid}`\nDone: {done}/{total} | Failed: {failed} | Cancelled: {cancelled}"
    )
}

// ── URL download (port of `fns.fast_download` / `fns.get_smart_name`) ─────

/// Port of `fns.get_smart_name` (`pyUltroid/fns/helper.py:104`): HEAD the
/// URL; prefer `Content-Disposition: filename=`, else rebuild the basename
/// with the extension guessed from `Content-Type`.
async fn smart_name(ctx: &Ctx, url: &str) -> String {
    let fallback = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file")
        .split('?')
        .next()
        .unwrap_or("file")
        .to_string();
    let Ok(resp) = ctx
        .http
        .head(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    else {
        return fallback;
    };
    if let Some(cd) = resp.headers().get("content-disposition") {
        if let Ok(cd) = cd.to_str() {
            if let Some(name) = cd.split("filename=").nth(1) {
                let name = name
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'' || c == ';' || c == ' ');
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    if let Some(ct) = resp.headers().get("content-type") {
        if let Ok(ct) = ct.to_str() {
            if let Some(ext) = mime_extension(ct) {
                let stem = fallback
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(&fallback);
                return format!("{stem}{ext}");
            }
        }
    }
    fallback
}

/// Streaming GET to `dest`, editing `msg_id` with progress (port of
/// `fns.fast_download` + the `fns.progress` callback). Returns
/// `(dest, elapsed_ms)`.
async fn fast_download(ctx: &Ctx, url: &str, dest: &Path, msg_id: i64) -> Result<(PathBuf, u128)> {
    let start = Instant::now();
    let mut resp = ctx.http.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "download failed: HTTP {}",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest).await?;
    let mut done: u64 = 0;
    let mut last_edit = Instant::now() - Duration::from_secs(10);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if last_edit.elapsed() >= Duration::from_secs(3) {
            last_edit = Instant::now();
            let pct = if total > 0 { done * 100 / total } else { 0 };
            let _ = ctx
                .edit(
                    msg_id,
                    &format!(
                        "Downloading: `{name}`\n{} / {} — {pct}%",
                        humanbytes(done),
                        humanbytes(total)
                    ),
                )
                .await;
        }
    }
    file.flush().await?;
    Ok((dest.to_path_buf(), start.elapsed().as_millis()))
}

// ── .download ─────────────────────────────────────────────────────────────

async fn download_one_url(
    ctx: &Ctx,
    msg_id: i64,
    url: &str,
    flags: &ParsedFlags,
    custom_name: Option<&str>,
) -> Result<()> {
    let smart = smart_name(ctx, url).await;
    let filename = resolve_name(custom_name, Some(&smart), None)
        .or_else(|| Some(smart.clone()))
        .unwrap_or_else(|| "file".to_string());
    let dir = PathBuf::from(&ctx.config.data_dir).join(DOWNLOAD_DIR);
    tokio::fs::create_dir_all(&dir).await?;
    let dest = check_filename(&dir.join(&filename));
    let ttl = temp_ttl(ctx, flags).await.unwrap_or(TEMP_TTL_DEFAULT);
    match fast_download(ctx, url, &dest, msg_id).await {
        Ok((saved, elapsed)) => {
            let saved = post_download(saved, flags, ttl);
            ctx.edit(
                msg_id,
                &format!(
                    "`{}` downloaded in {}.",
                    saved.display(),
                    time_formatter(elapsed as u64)
                ),
            )
            .await?;
            Ok(())
        }
        Err(e) => {
            ctx.edit(msg_id, &format!("`Error: {e}`")).await?;
            Ok(())
        }
    }
}

async fn bulk_url_download(
    ctx: &Ctx,
    msg_id: i64,
    urls: &[String],
    flags: &ParsedFlags,
    custom_name: Option<&str>,
) -> Result<()> {
    let operation = "dl";
    let gid = enqueue_batch(operation, urls.iter().map(|u| (u.clone(), 0)).collect()).await;
    refresh_tracker(ctx, msg_id, &gid, true).await?;
    let ttl = temp_ttl(ctx, flags).await.unwrap_or(TEMP_TTL_DEFAULT);
    let dir = PathBuf::from(&ctx.config.data_dir).join(DOWNLOAD_DIR);
    tokio::fs::create_dir_all(&dir).await?;
    for (index, url) in urls.iter().enumerate() {
        let fid = format!("{gid}-{index}");
        if file_status(&fid).await.as_deref() == Some("cancelled") {
            refresh_tracker(ctx, msg_id, &gid, false).await?;
            continue;
        }
        set_file_status(&fid, "downloading").await;
        refresh_tracker(ctx, msg_id, &gid, false).await?;
        let smart = smart_name(ctx, url).await;
        let name = resolve_name(custom_name, Some(&smart), None).unwrap_or(smart);
        let dest = check_filename(&dir.join(&name));
        match fast_download(ctx, url, &dest, msg_id).await {
            Ok((saved, _)) => {
                post_download(saved, flags, ttl);
                set_file_status(&fid, "done").await;
            }
            Err(_) => set_file_status(&fid, "failed").await,
        }
        refresh_tracker(ctx, msg_id, &gid, false).await?;
    }
    refresh_tracker(ctx, msg_id, &gid, true).await?;
    if let Some((op, total, done, failed, cancelled, _)) = batch_summary(&gid).await {
        ctx.edit(
            msg_id,
            &bulk_summary(&op, &gid, total, done, failed, cancelled),
        )
        .await?;
    }
    Ok(())
}

async fn download_cmd(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let parsed = match parse_flags(raw, DW_VALUE_FLAGS, DW_BOOL_FLAGS) {
        Ok(p) => p,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    if !parsed.unknowns.is_empty() {
        let mut valid: Vec<&str> = DW_VALUE_FLAGS
            .iter()
            .chain(DW_BOOL_FLAGS.iter())
            .copied()
            .collect();
        valid.sort_unstable();
        ctx.reply(&flag_error_text("download", &parsed.unknowns, &valid))
            .await?;
        return Ok(());
    }
    if parsed.flags.contains_key("-ttl") && temp_ttl(ctx, &parsed).await.is_none() {
        ctx.reply("`Invalid -ttl value, expected a number of seconds.`")
            .await?;
        return Ok(());
    }
    let custom_name = parsed.flags.get("-n").map(|s| s.as_str());
    let mut urls: Vec<String> = parsed
        .positionals
        .iter()
        .filter(|p| p.starts_with("http://") || p.starts_with("https://"))
        .cloned()
        .collect();

    // Fall back to a replied .txt file containing one URL per line.
    if urls.is_empty() && ctx.reply_to_id.is_some() {
        // Download the replied `.txt` file, read one URL per line (skip
        // blanks and `#` comments), then run the bulk download below
        // (Python: downloadupload.py:down).
        let reply_id = ctx.reply_to_id.unwrap_or(0);
        let dir = PathBuf::from(&ctx.config.data_dir).join(DOWNLOAD_DIR);
        tokio::fs::create_dir_all(&dir).await?;
        let dest = dir.join(format!("urls-{}.txt", uuid::Uuid::new_v4()));
        let path = ctx
            .responder
            .download_media(ctx.chat_id, reply_id as i32, &dest)
            .await?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| Error::Other(format!("replied .txt file is not readable text: {e}")))?;
        let _ = tokio::fs::remove_file(&path).await;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("http://") || line.starts_with("https://") {
                urls.push(line.to_string());
            }
        }
    }

    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    if urls.is_empty() {
        ctx.edit(
            msg_id,
            "`Give a direct link to download, or reply to a .txt file with URLs.`",
        )
        .await?;
        return Ok(());
    }
    urls.truncate(MAX_BULK);
    if urls.len() == 1 {
        download_one_url(ctx, msg_id, &urls[0], &parsed, custom_name).await
    } else {
        bulk_url_download(ctx, msg_id, &urls, &parsed, custom_name).await
    }
}

// ── .dl (Telegram media) ──────────────────────────────────────────────────

async fn dl_cmd(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let parsed = match parse_flags(raw, DL_VALUE_FLAGS, DL_BOOL_FLAGS) {
        Ok(p) => p,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    if !parsed.unknowns.is_empty() {
        let mut valid: Vec<&str> = DL_VALUE_FLAGS
            .iter()
            .chain(DL_BOOL_FLAGS.iter())
            .copied()
            .collect();
        valid.sort_unstable();
        ctx.reply(&flag_error_text("dl", &parsed.unknowns, &valid))
            .await?;
        return Ok(());
    }
    if parsed.flags.contains_key("-ttl") && temp_ttl(ctx, &parsed).await.is_none() {
        ctx.reply("`Invalid -ttl value, expected a number of seconds.`")
            .await?;
        return Ok(());
    }
    let _custom_name = parsed.flags.get("-n");
    let _delete_originals = has_bool(&parsed, "-del");
    let link = parsed
        .positionals
        .iter()
        .find(|p| p.contains("t.me/"))
        .cloned();

    // Python sends the status message before either branch.
    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;

    // --- t.me link path ---
    if let Some(link) = link {
        let (chat, ids) = parse_tg_link(&link);
        if ids.is_empty() {
            ctx.edit(msg_id, &ctx.get_str("gms_1")).await?;
            return Ok(());
        }
        if ids.len() > MAX_BULK {
            ctx.edit(
                msg_id,
                &format!(
                    "`Range too large: {} messages (max {MAX_BULK}).`",
                    ids.len()
                ),
            )
            .await?;
            return Ok(());
        }
        // TODO(responder-op): resolve the chat (a public username needs a
        // client-side resolve; a private `c/` link already yields the
        // `-100…` id) and fetch the messages
        // (`event.client.get_messages(chat, ids=ids)`), filter to those
        // with media, then download each via the queue-tracked bulk path
        // (`_bulk_tg_download`, parallel) or the single-file path
        // (Python: downloadupload.py:download).
        let _ = chat;
        return Err(Error::Other(
            "fetch Telegram messages by t.me link requires a binary Responder op (Python: downloadupload.py:download)"
                .to_string(),
        ));
    }

    // --- reply path (single media or full album) ---
    if ctx.reply_to_id.is_none() {
        ctx.edit(msg_id, &ctx.get_str("cvt_3")).await?;
        return Ok(());
    }
    // TODO(responder-op): fetch the replied message (`get_reply_message`),
    // expand grouped-media albums (`_resolve_album`), and download via
    // `_download_tg_media` (Python: downloadupload.py:download).
    Err(Error::Other(
        "fetch replied Telegram message + download its media requires a binary Responder op (Python: downloadupload.py:download)"
            .to_string(),
    ))
}

// ── .ul (local file / directory upload) ───────────────────────────────────

/// Minimal glob: `*` / `?` in the final path segment (port of the
/// `glob.glob(path)` use in `.ul`; Python also matches `*` in any segment,
/// this covers the documented `path/` + `*` and single-segment cases).
fn glob_expand(pattern: &str) -> Vec<PathBuf> {
    if !pattern.contains('*') && !pattern.contains('?') {
        let p = PathBuf::from(pattern);
        return if p.exists() { vec![p] } else { vec![] };
    }
    fn matches(pat: &str, text: &str) -> bool {
        let p: Vec<char> = pat.chars().collect();
        let t: Vec<char> = text.chars().collect();
        fn rec(p: &[char], t: &[char]) -> bool {
            if p.is_empty() {
                return t.is_empty();
            }
            match p[0] {
                '*' => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
                '?' => !t.is_empty() && rec(&p[1..], &t[1..]),
                c => !t.is_empty() && t[0] == c && rec(&p[1..], &t[1..]),
            }
        }
        rec(&p, &t)
    }
    let path = Path::new(pattern);
    let dir = path.parent().unwrap_or(Path::new("."));
    let file_pat = path.file_name().map(|f| f.to_string_lossy().into_owned());
    let Some(file_pat) = file_pat else {
        return vec![];
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| matches(&file_pat, &n.to_string_lossy()))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// Port of `list_directory_files` (`pyUltroid/_core/queue.py:466`): regular
/// files directly under `path`, ascending by size, with a final `Total` row.
fn list_directory_files(path: &Path) -> Vec<(String, u64)> {
    let Ok(rd) = std::fs::read_dir(path) else {
        return vec![];
    };
    let mut rows: Vec<(String, u64)> = Vec::new();
    let mut total = 0u64;
    for e in rd.flatten() {
        if let Ok(md) = e.metadata() {
            if md.is_file() {
                total += md.len();
                rows.push((e.file_name().to_string_lossy().into_owned(), md.len()));
            }
        }
    }
    rows.sort_by_key(|(_, s)| *s);
    rows.push(("Total".to_string(), total));
    rows
}

/// Port of `fns.get_all_files`: every file under `directory`, recursive.
fn get_all_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn format_dir_table(rows: &[(String, u64)]) -> String {
    let mut lines = vec![
        "```".to_string(),
        format!("{:<28} {:>10}", "File", "Size"),
        "-".repeat(40),
    ];
    for (name, size) in rows {
        let shown: String = name.chars().take(28).collect();
        lines.push(format!("{shown:<28} {:>10}", humanbytes(*size)));
    }
    lines.push("```".to_string());
    lines.join("\n")
}

fn upload_name(custom: Option<&str>, path: &Path, index: Option<usize>) -> String {
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(custom) = custom else {
        return base;
    };
    let custom = custom.trim();
    let ext = Path::new(&base)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    if let Some(i) = index {
        return format!("[{i}]{custom}{ext}");
    }
    if Path::new(custom).extension().is_some() {
        custom.to_string()
    } else {
        format!("{custom}{ext}")
    }
}

async fn upload_single_file(
    ctx: &Ctx,
    path: &Path,
    custom_name: Option<&str>,
    delete: bool,
    index: Option<usize>,
) -> Result<()> {
    let start = Instant::now();
    let filename = upload_name(custom_name, path, index);
    ctx.send_file(
        path,
        &format!(
            "`Uploaded` `{filename}` `in {}`",
            time_formatter(start.elapsed().as_millis() as u64)
        ),
    )
    .await?;
    if delete {
        let _ = tokio::fs::remove_file(path).await;
    }
    Ok(())
}

async fn upload_directory(
    ctx: &Ctx,
    msg_id: i64,
    directory: &Path,
    _flags: &ParsedFlags,
    custom_name: Option<&str>,
    delete: bool,
) -> Result<()> {
    let rows = list_directory_files(directory);
    if rows.is_empty() {
        ctx.edit(
            msg_id,
            &format!("`No files found in` `{}`", directory.display()),
        )
        .await?;
        return Ok(());
    }
    ctx.edit(
        msg_id,
        &format!(
            "`Uploading directory:` `{}`\n{}",
            directory.display(),
            format_dir_table(&rows)
        ),
    )
    .await?;
    let files = get_all_files(directory);
    if files.is_empty() {
        return Ok(());
    }
    let operation = "ul";
    let gid = enqueue_batch(
        operation,
        files
            .iter()
            .map(|f| {
                (
                    f.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    f.metadata().map(|m| m.len()).unwrap_or(0),
                )
            })
            .collect(),
    )
    .await;
    refresh_tracker(ctx, msg_id, &gid, true).await?;
    for (index, fpath) in files.iter().enumerate() {
        let fid = format!("{gid}-{index}");
        if file_status(&fid).await.as_deref() == Some("cancelled") {
            refresh_tracker(ctx, msg_id, &gid, false).await?;
            continue;
        }
        set_file_status(&fid, "uploading").await;
        refresh_tracker(ctx, msg_id, &gid, false).await?;
        match upload_single_file(ctx, fpath, custom_name, delete, Some(index)).await {
            Ok(()) => set_file_status(&fid, "done").await,
            Err(_) => set_file_status(&fid, "failed").await,
        }
        refresh_tracker(ctx, msg_id, &gid, false).await?;
    }
    refresh_tracker(ctx, msg_id, &gid, true).await?;
    if let Some((op, total, done, failed, cancelled, _)) = batch_summary(&gid).await {
        ctx.edit(
            msg_id,
            &bulk_summary(&op, &gid, total, done, failed, cancelled),
        )
        .await?;
    }
    Ok(())
}

async fn ul_cmd(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let parsed = match parse_flags(raw, UL_VALUE_FLAGS, UL_BOOL_FLAGS) {
        Ok(p) => p,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    if !parsed.unknowns.is_empty() {
        let mut valid: Vec<&str> = UL_VALUE_FLAGS
            .iter()
            .chain(UL_BOOL_FLAGS.iter())
            .copied()
            .collect();
        valid.sort_unstable();
        ctx.reply(&flag_error_text("ul", &parsed.unknowns, &valid))
            .await?;
        return Ok(());
    }
    let custom_name = parsed.flags.get("-n").map(|s| s.as_str());
    let delete = has_bool(&parsed, "-del") || has_bool(&parsed, "--delete");
    // `--stream` / `--no-thumb` have no effect on `Ctx::send_file`; they are
    // accepted (Python: `supports_streaming` / `thumb`) and recorded here.
    let _stream = has_bool(&parsed, "--stream");
    let _no_thumb = has_bool(&parsed, "--no-thumb");
    let path = parsed.positionals.join(" ").trim().to_string();

    let msg_id = ctx.reply(&ctx.get_str("com_1")).await?;
    if path.is_empty() {
        ctx.edit(msg_id, "`Give a file/directory path to upload.`")
            .await?;
        return Ok(());
    }
    // Python: `if not event.out and path == ".env"` — refuse a `.env`
    // upload unless the command came from the owner account itself.
    if path == ".env" && !ctx.is_owner {
        ctx.reply("`You can't do this...`").await?;
        return Ok(());
    }
    let mut pattern = path.clone();
    if pattern.ends_with('/') {
        pattern.push('*');
    }
    let mut results = glob_expand(&pattern);
    if results.is_empty() && Path::new(&path).exists() {
        results = vec![PathBuf::from(&path)];
    }
    if results.is_empty() {
        ctx.edit(msg_id, &ctx.get_str("ls1")).await?;
        return Ok(());
    }
    for result in results {
        if result.is_dir() {
            upload_directory(ctx, msg_id, &result, &parsed, custom_name, delete).await?;
            continue;
        }
        if let Err(e) = upload_single_file(ctx, &result, custom_name, delete, None).await {
            // Python: `ValueError`/`IsADirectoryError` are skipped
            // silently; anything else is logged and the loop continues.
            let _ = e;
            continue;
        }
    }
    ctx.delete(msg_id).await?;
    Ok(())
}

// ── .fcancel ──────────────────────────────────────────────────────────────

async fn fcancel_cmd(ctx: &Ctx) -> Result<()> {
    let target = ctx.args.trim().split_whitespace().next().unwrap_or("");
    if target.is_empty() {
        ctx.reply("`Usage: .fcancel <GID>  or  .fcancel <FID>`")
            .await?;
        return Ok(());
    }
    if !(ctx.is_owner || ctx.is_sudo) {
        ctx.reply("`Only the owner/sudo can cancel queued tasks.`")
            .await?;
        return Ok(());
    }
    let ok = cancel_target(target).await;
    let info = batch_summary(target).await;
    // FID-level status: look the single file up directly.
    let fid_status = if target.contains('-') {
        file_status(target).await
    } else {
        None
    };
    if !ok && info.is_none() && fid_status.is_none() {
        ctx.reply(&format!("`No active task found for` `{target}`"))
            .await?;
        return Ok(());
    }
    if target.contains('-') {
        ctx.reply(&format!(
            "`Cancelled file` `{target}` `— status:` `{}`",
            fid_status.unwrap_or_else(|| "cancelled".to_string())
        ))
        .await?;
        return Ok(());
    }
    if let Some((_, _, done, failed, cancelled, active)) = info {
        ctx.reply(&format!(
            "`Cancelled batch` `{target}`\nDone: {done} | Cancelled: {cancelled} | Failed: {failed} | Active: {active}"
        ))
        .await?;
        return Ok(());
    }
    ctx.reply(&format!("`Cancelled` `{target}`")).await?;
    Ok(())
}
