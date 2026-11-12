//! Zip tools — port of `plugins/ziptools.py` (745 LOC).
//!
//! * `.zip [-p <pw>|-l <0-9>|-m <deflate|bz2|lzma|store>] <text>` — zip text
//!   content or a replied file.
//! * `.unzip [-p|-d|-o <dir>|-t] <file_id>` — extract / preview / details /
//!   test a zip archive.
//! * `.list [-d] <file_id>` — list archive contents as a table.
//! * `.azip` / `.dozip [-p <pw> <level>|-c <level> <method>]` — batch zip.
//!
//! There is no `zip` crate in the workspace, so archives are driven
//! through the system `zip` / `unzip` binaries via `tokio::process`
//! (real tools, honest errors when absent). Methods map to zip's
//! `-0` (store) / default deflate / `-Z bzip2`; **lzma is not supported
//! by the system `zip` CLI** — requesting it returns an honest error
//! naming the missing `zip` crate (see the report). Passwords use
//! ZipCrypto (`-P`), the same legacy scheme Python's `zipfile` writes.
//!
//! The inline mode (`@<assistant> zip …`) and the assistant-bot
//! `/zip` etc. commands are binary-side; the pending-payload store is
//! exposed via [`store_pending`] / [`take_pending`] for the binary's
//! inline + callback handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};

use crate::util::{def, reg_add};
use regex::Regex;
use tokio::process::Command as AsyncCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available\n\n• `{i}zip <text>` — create a zip archive from text content\n  `{i}zip <file_id>` — zip by file ID\n  `{i}zip -p <password> <text>` — password-protected zip\n  `{i}zip -l <level> <text>` — compression level 0-9 (default 6)\n  `{i}zip -m <method> <text>` — method: deflate, bz2, lzma, store (default deflate)\n\n• `{i}unzip <file_id>` — extract zip archive\n  `{i}unzip -p <file_id>` — preview contents without extracting\n  `{i}unzip -d <file_id>` — show details (sizes, ratios, CRC)\n  `{i}unzip -o <dir> <file_id>` — extract to specific directory\n  `{i}unzip -t <file_id>` — test archive integrity\n\n• `{i}list <file_id>` — list archive contents in a table\n  `{i}list -d <file_id>` — list with details\n\n• `{i}azip <reply to file>` — add file to batch for batch zip\n• `{i}dozip <password>` — create batch zip (optional password)\n• `{i}dozip -c <count>` — compress batch with compression level";

const BATCH_DIR: &str = "zipbatch";
const ZIP_CACHE_DIR: &str = "zips";
const PENDING_TTL_SECS: u64 = 300;

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("zip", r"zip(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(zip_cmd(ctx))
    });
    reg_add(registry, def("unzip", r"unzip(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(unzip_cmd(ctx))
    });
    reg_add(registry, def("list", r"list(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(list_cmd(ctx))
    });
    reg_add(registry, def("azip", "azip$", HELP), |ctx| {
        Box::pin(azip_cmd(ctx))
    });
    reg_add(registry, def("dozip", r"dozip(?:\s+(.*))?$", HELP), |ctx| {
        Box::pin(dozip_cmd(ctx))
    });
}

// ── fmtmsg (custom/fmt.py) ────────────────────────────────────────────────

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Python `custom.fmt.fmtmsg(head, {key: value, …})` — `<b>head</b>` plus
/// padded `<code>key</code> : <code>value</code>` rows (values escaped).
fn fmtmsg(head: &str, data: &[(&str, String)]) -> String {
    let padd = data
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("<b>{}</b>", html_escape(head));
    if !data.is_empty() {
        let body: Vec<String> = data
            .iter()
            .map(|(k, v)| {
                let key = format!("{:width$}", html_escape(k), width = padd);
                format!("  <code>{key}</code> : <code>{}</code>", html_escape(v))
            })
            .collect();
        out.push_str("\n\n");
        out.push_str(&body.join("\n"));
    }
    out
}

// ── zip entry info ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub filename: String,
    pub size: u64,
    pub compressed: u64,
    pub ratio: f64,
    pub method: String,
    pub crc: String,
    pub date: String,
}

fn fmt_size(nbytes: u64) -> String {
    let mut v = nbytes as f64;
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if v.abs() < 1024.0 {
            return format!("{v:.1}{unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.1}PB")
}

fn method_name(unzip_method: &str) -> &'static str {
    match unzip_method {
        "Stored" => "store",
        m if m.starts_with("Defl:") => "deflate",
        "BZip2" => "bz2",
        "LZMA" => "lzma",
        _ => "unknown",
    }
}

static UNZIP_V_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\d+)\s+(\S+)\s+(\d+)\s+(\d+)%\s+(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2})\s+([0-9a-fA-F]{8})\s+(.*)$")
        .unwrap()
});

/// Parse `unzip -v` output into entries (Python `_list_zip` + `_file_info`).
fn parse_unzip_v(stdout: &str) -> Vec<ZipEntry> {
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if let Some(c) = UNZIP_V_LINE.captures(line) {
            let size: u64 = c[1].parse().unwrap_or(0);
            let compressed: u64 = c[3].parse().unwrap_or(0);
            let ratio = if size > 0 {
                (1.0 - compressed as f64 / size as f64) * 100.0
            } else {
                0.0
            };
            entries.push(ZipEntry {
                filename: c[8].to_string(),
                size,
                compressed,
                ratio,
                method: method_name(&c[2]).to_string(),
                crc: c[7].to_lowercase(),
                date: format!("{} {}:00", &c[5], &c[6]),
            });
        }
    }
    entries
}

fn build_table(files: &[ZipEntry], show_details: bool) -> String {
    if files.is_empty() {
        return "<b>No files in archive.</b>".to_string();
    }
    if show_details {
        let mut out = String::from(
            "<table border='1' cellpadding='6' cellspacing='0' \
             style='border-collapse:collapse;width:100%'>\
             <tr style='background:#2ECC71'>\
             <th>File</th><th>Size</th><th>Compressed</th>\
             <th>Ratio</th><th>Method</th><th>CRC32</th><th>Date</th></tr>",
        );
        for f in files {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1}%</td>\
                 <td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                html_escape(&f.filename),
                fmt_size(f.size),
                fmt_size(f.compressed),
                f.ratio,
                f.method,
                f.crc,
                f.date
            ));
        }
        out.push_str("</table>");
        out
    } else {
        let mut out = String::from(
            "<table border='1' cellpadding='6' cellspacing='0' \
             style='border-collapse:collapse;width:100%'>\
             <tr style='background:#2ECC71'><th>#</th><th>File</th><th>Size</th></tr>",
        );
        for (i, f) in files.iter().enumerate() {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                i + 1,
                html_escape(&f.filename),
                fmt_size(f.size)
            ));
        }
        out.push_str("</table>");
        out
    }
}

fn zip_info_summary(entries: &[ZipEntry]) -> (usize, u64, u64, f64, String) {
    let total_size: u64 = entries.iter().map(|e| e.size).sum();
    let total_compressed: u64 = entries.iter().map(|e| e.compressed).sum();
    let ratio = if total_size > 0 {
        (1.0 - total_compressed as f64 / total_size as f64) * 100.0
    } else {
        0.0
    };
    let method = entries
        .first()
        .map(|e| e.method.clone())
        .unwrap_or_else(|| "deflate".into());
    (entries.len(), total_size, total_compressed, ratio, method)
}

// ── zip / unzip primitives (system binaries) ──────────────────────────────

fn check_method(method: &str) -> Result<()> {
    match method {
        "deflate" | "bz2" | "store" => Ok(()),
        "lzma" => Err(Error::Other(
            "lzma compression is not supported by the system `zip` CLI; a `zip` \
             crate would be needed for full Python zipfile parity (Python: \
             ziptools.py:_create_zip, method=lzma)"
                .to_string(),
        )),
        other => Err(Error::Other(format!("unknown compression method: {other}"))),
    }
}

/// Create a zip from `sources` (files or directories) at `output`.
/// Staging copies files so archive names are basenames / paths relative
/// to each source dir, exactly like Python's `arcname` handling.
async fn create_zip(
    sources: &[PathBuf],
    output: &Path,
    password: Option<&str>,
    level: u32,
    method: &str,
) -> Result<()> {
    check_method(method)?;
    let level = level.min(9);
    let staging = std::env::temp_dir().join(format!("zip_stage_{}", std::process::id()));
    tokio::fs::create_dir_all(&staging).await?;
    let stage = async {
        for src in sources {
            if src.is_file() {
                let name = src.file_name().unwrap_or_default();
                tokio::fs::copy(src, staging.join(name)).await?;
            } else if src.is_dir() {
                let base = src.file_name().unwrap_or_default();
                let target = staging.join(base);
                copy_dir_recursive(src, &target).await?;
            }
        }
        Ok::<(), Error>(())
    };
    let res = stage.await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e);
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let mut args: Vec<String> = vec!["-q".into(), "-r".into(), format!("-{level}")];
    match method {
        "store" => args.push("-0".into()),
        "bz2" => {
            args.push("-Z".into());
            args.push("bzip2".into());
        }
        _ => {}
    }
    if let Some(pw) = password {
        args.push("-P".into());
        args.push(pw.to_string());
    }
    args.push(output.to_string_lossy().into_owned());
    args.push(".".into());
    let status = AsyncCommand::new("zip")
        .args(&args)
        .current_dir(&staging)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let status = status?;
    if !status.success() {
        return Err(Error::Other(format!("zip failed (exit {status})")));
    }
    Ok(())
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut rd = tokio::fs::read_dir(src).await?;
    while let Some(entry) = rd.next_entry().await? {
        let ty = entry.file_type().await?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &target)).await?;
        } else {
            tokio::fs::copy(entry.path(), target).await?;
        }
    }
    Ok(())
}

/// Version needed to extract, from `zipinfo -v` (Python `ZipFile.version`).
async fn zip_version(zip_path: &Path) -> String {
    let out = AsyncCommand::new("zipinfo")
        .args(["-v", &zip_path.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("version needed to extract:") {
                    return rest.trim().to_string();
                }
            }
            String::new()
        }
        Err(_) => String::new(),
    }
}

async fn list_zip(zip_path: &Path) -> Result<Vec<ZipEntry>> {
    let out = AsyncCommand::new("unzip")
        .args(["-v", &zip_path.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "unzip -v failed for {}: {}",
            zip_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(parse_unzip_v(&String::from_utf8_lossy(&out.stdout)))
}

/// Test archive integrity — returns `(ok, bad_file)` (Python `_test_zip`).
async fn test_zip(zip_path: &Path) -> Result<(bool, Option<String>)> {
    // No `-q`: the `testing:` lines are what name the bad entry on failure.
    let out = AsyncCommand::new("unzip")
        .args(["-t", &zip_path.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if out.status.success() {
        return Ok((true, None));
    }
    let bad = stdout.lines().find_map(|l| {
        let l = l.trim_start();
        l.strip_prefix("testing:").and_then(|rest| {
            let name = rest.split_whitespace().next()?;
            if l.contains("bad CRC") || l.contains("error") {
                Some(name.to_string())
            } else {
                None
            }
        })
    });
    Ok((false, bad))
}

async fn extract_zip(
    zip_path: &Path,
    dest_dir: &Path,
    password: Option<&str>,
) -> Result<(bool, Option<String>)> {
    let (ok, bad) = test_zip(zip_path).await?;
    if !ok {
        return Ok((false, bad));
    }
    tokio::fs::create_dir_all(dest_dir).await?;
    let mut args: Vec<String> = vec!["-o".into(), "-q".into()];
    if let Some(pw) = password {
        args.push("-P".into());
        args.push(pw.to_string());
    }
    args.push(zip_path.to_string_lossy().into_owned());
    args.push("-d".into());
    args.push(dest_dir.to_string_lossy().into_owned());
    let status = AsyncCommand::new("unzip")
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await?;
    if !status.success() {
        return Err(Error::Other(format!("unzip failed (exit {status})")));
    }
    Ok((true, None))
}

/// Resolve a file id / path (Python `_resolve_file`).
fn resolve_file(file_id_or_path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(file_id_or_path);
    if p.is_file() {
        return Some(p);
    }
    for dir in [ZIP_CACHE_DIR, "unzip"] {
        let cand = Path::new(dir).join(file_id_or_path);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }
    walk(dir, &mut out);
    out.sort();
    out
}

/// Strip MarkdownV3 rich markers the way Python's `parse_rich(text, "mdv3")`
/// cleans text before zipping it.
fn strip_rich(text: &str) -> String {
    text.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "")
        .replace("||", "")
}

fn err_reply(head: &str, msg: &str) -> String {
    fmtmsg(head, &[("Error", msg.to_string())])
}

// ── Inline pending store (Python `_PENDING`) ──────────────────────────────

pub struct PendingPayload {
    pub kind: String,
    pub text: String,
    pub expires_at_unix: u64,
}

static PENDING: LazyLock<Mutex<HashMap<String, PendingPayload>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SEQ: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

/// Binary hook: store an inline zip/unzip payload, returning its key
/// (Python `_store_pending`; TTL 300 s).
pub fn store_pending(kind: &str, text: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut m) = PENDING.lock() {
        m.retain(|_, v| now <= v.expires_at_unix);
    }
    let mut seq = SEQ.lock().unwrap_or_else(|e| e.into_inner());
    *seq += 1;
    let key = seq.to_string();
    if let Ok(mut m) = PENDING.lock() {
        m.insert(
            key.clone(),
            PendingPayload {
                kind: kind.to_string(),
                text: text.to_string(),
                expires_at_unix: now + PENDING_TTL_SECS,
            },
        );
    }
    key
}

/// Binary hook: take (consume) a pending payload by key.
pub fn take_pending(key: &str) -> Option<PendingPayload> {
    PENDING.lock().ok()?.remove(key)
}

// ── Commands ──────────────────────────────────────────────────────────────

async fn zip_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let has_reply = ctx.reply_to_id.is_some();

    // Parse flags (Python order: -p, then -l, then -m).
    let mut password: Option<String> = None;
    let mut level: u32 = 6;
    let mut method = "deflate".to_string();
    let mut text = args.clone();
    if let Some(rest) = args.strip_prefix("-p ") {
        let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
        if parts.len() >= 2 {
            password = Some(parts[0].to_string());
            text = parts[1].to_string();
        } else {
            ctx.reply_html(&err_reply("Zip", "Usage: zip -p <password> <text>"))
                .await?;
            return Ok(());
        }
    } else if let Some(rest) = args.strip_prefix("-l ") {
        let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
        if parts.len() >= 2 {
            match parts[0].parse::<u32>() {
                Ok(n) if n <= 9 => level = n,
                _ => {
                    ctx.reply_html(&err_reply("Zip", "Level must be 0-9"))
                        .await?;
                    return Ok(());
                }
            }
            text = parts[1].to_string();
        } else {
            ctx.reply_html(&err_reply("Zip", "Usage: zip -l <0-9> <text>"))
                .await?;
            return Ok(());
        }
    } else if let Some(rest) = args.strip_prefix("-m ") {
        let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
        if parts.len() >= 2 {
            method = parts[0].to_lowercase();
            if !["deflate", "bz2", "lzma", "store"].contains(&method.as_str()) {
                ctx.reply_html(&err_reply("Zip", "Method: deflate, bz2, lzma, store"))
                    .await?;
                return Ok(());
            }
            text = parts[1].to_string();
        } else {
            ctx.reply_html(&err_reply(
                "Zip",
                "Usage: zip -m <deflate|bz2|lzma|store> <text>",
            ))
            .await?;
            return Ok(());
        }
    }

    if text.is_empty() && !has_reply {
        ctx.reply_html(&fmtmsg(
            "Zip",
            &[(
                "Usage",
                format!(
                    "{}zip -p <pw> <text> | reply to file | use -l/-m flags",
                    ctx.config.hndlr
                ),
            )],
        ))
        .await?;
        return Ok(());
    }

    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let output_path = PathBuf::from(ZIP_CACHE_DIR).join(format!("ultroid_{t}.zip"));
    tokio::fs::create_dir_all(ZIP_CACHE_DIR).await?;

    if !text.is_empty() && !has_reply {
        let tmp_file = PathBuf::from(ZIP_CACHE_DIR).join(format!("tmp_{t}.txt"));
        let clean = if text.contains("{<b>") {
            text.clone()
        } else {
            strip_rich(&text)
        };
        tokio::fs::write(&tmp_file, clean).await?;
        if let Err(e) = create_zip(
            &[tmp_file.clone()],
            &output_path,
            password.as_deref(),
            level,
            &method,
        )
        .await
        {
            let _ = tokio::fs::remove_file(&tmp_file).await;
            let _ = ctx.delete(processing).await;
            return Err(e);
        }
        let _ = tokio::fs::remove_file(&tmp_file).await;
    } else if has_reply {
        // TODO(responder-op): download replied media (Python: ziptools.py:zip_cmd
        // — `fns.downloader` / `event.download_media`)
        let _ = ctx.delete(processing).await;
        return Err(Error::Other(
            "downloading replied media requires a binary Responder op (Python: ziptools.py:zip_cmd)"
                .to_string(),
        ));
    } else {
        let _ = ctx.delete(processing).await;
        ctx.reply_html(&err_reply("Zip", "Reply to a file or provide text content"))
            .await?;
        return Ok(());
    }

    let name = output_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ctx.send_file(&output_path, &format!("`{name}`")).await?;
    let _ = tokio::fs::remove_file(&output_path).await;
    let _ = ctx.delete(processing).await;
    Ok(())
}

async fn unzip_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let has_reply = ctx.reply_to_id.is_some();

    let mut preview = false;
    let mut details = false;
    let mut output_dir = "unzip".to_string();
    let mut test_only = false;
    let file_id: String;

    let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
    if !parts.is_empty() && parts[0].starts_with("-p") {
        preview = true;
        file_id = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    } else if !parts.is_empty() && parts[0].starts_with("-d") {
        details = true;
        file_id = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    } else if !parts.is_empty() && parts[0].starts_with("-o") {
        output_dir = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unzip".into());
        file_id = String::new();
    } else if !parts.is_empty() && parts[0].starts_with("-t") {
        test_only = true;
        file_id = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    } else {
        file_id = args.clone();
    }

    let file_path: PathBuf;
    if file_id.is_empty() && has_reply {
        // TODO(responder-op): download replied zip (Python: ziptools.py:unzip_cmd)
        return Err(Error::Other(
            "downloading replied media requires a binary Responder op (Python: ziptools.py:unzip_cmd)"
                .to_string(),
        ));
    } else if !file_id.is_empty() {
        match resolve_file(&file_id) {
            Some(p) => file_path = p,
            None => {
                ctx.reply_html(&err_reply("Zip", "File not found")).await?;
                return Ok(());
            }
        }
    } else {
        ctx.reply_html(&fmtmsg(
            "Zip",
            &[(
                "Usage",
                format!(
                    "{}unzip <file_id> | reply to zip | -p/-d/-o/-t flags",
                    ctx.config.hndlr
                ),
            )],
        ))
        .await?;
        return Ok(());
    }

    if !file_path.is_file() {
        ctx.reply_html(&err_reply("Zip", "Zip file not found"))
            .await?;
        return Ok(());
    }
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;

    if test_only {
        let (ok, bad) = test_zip(&file_path).await?;
        let _ = ctx.delete(processing).await;
        if ok {
            ctx.reply_html(&fmtmsg(
                "Zip Test",
                &[("Status", "✅ Archive is valid".to_string())],
            ))
            .await?;
        } else {
            ctx.reply_html(&fmtmsg(
                "Zip Test",
                &[
                    ("Status", "❌".to_string()),
                    ("Bad file", bad.unwrap_or_default()),
                ],
            ))
            .await?;
        }
        return Ok(());
    }

    if preview {
        let entries = list_zip(&file_path).await?;
        let count = entries.len();
        let table = build_table(&entries, false);
        let _ = ctx.delete(processing).await;
        ctx.reply_html(&fmtmsg(
            "Zip Preview",
            &[("Files", format!("{count} entries")), ("Table", table)],
        ))
        .await?;
        return Ok(());
    }

    if details {
        let entries = list_zip(&file_path).await?;
        let (count, total_size, total_compressed, ratio, method) = zip_info_summary(&entries);
        let version = zip_version(&file_path).await;
        let table = build_table(&entries, true);
        let _ = ctx.delete(processing).await;
        ctx.reply_html(&fmtmsg(
            "Zip Details",
            &[
                ("Count", count.to_string()),
                ("Size", fmt_size(total_size)),
                ("Compressed", fmt_size(total_compressed)),
                ("Ratio", format!("{ratio:.1}%")),
                ("Method", method),
                ("Version", version),
                ("Table", table),
            ],
        ))
        .await?;
        return Ok(());
    }

    // Regular extract.
    let out_dir = PathBuf::from(&output_dir);
    if out_dir.is_dir() {
        let _ = tokio::fs::remove_dir_all(&out_dir).await;
    }
    let (ok, bad) = extract_zip(&file_path, &out_dir, None).await?;
    if !ok {
        let _ = ctx.delete(processing).await;
        ctx.reply_html(&err_reply(
            "Zip",
            &format!("Corrupted archive: {}", bad.unwrap_or_default()),
        ))
        .await?;
        return Ok(());
    }
    let extracted = all_files(&out_dir);
    let files_info: Vec<ZipEntry> = extracted
        .iter()
        .map(|f| {
            let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            ZipEntry {
                filename: f
                    .strip_prefix(&out_dir)
                    .unwrap_or(f)
                    .to_string_lossy()
                    .into_owned(),
                size,
                compressed: size,
                ratio: 0.0,
                method: "store".into(),
                crc: String::new(),
                date: String::new(),
            }
        })
        .collect();
    let table = build_table(&files_info, false);
    let _ = ctx.delete(processing).await;
    ctx.reply_html(&fmtmsg(
        "Zip Extracted",
        &[
            ("Count", extracted.len().to_string()),
            ("Table", table),
            ("Dir", format!("`{output_dir}/`")),
        ],
    ))
    .await?;
    if file_path
        .parent()
        .map(|p| p.to_string_lossy().contains("unzip"))
        .unwrap_or(false)
    {
        let _ = tokio::fs::remove_file(&file_path).await;
    }
    Ok(())
}

async fn list_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let has_reply = ctx.reply_to_id.is_some();
    let mut details_flag = false;
    let mut file_id = args.clone();
    if let Some(rest) = args.strip_prefix("-d ") {
        details_flag = true;
        file_id = rest.trim().to_string();
    }

    let file_path: PathBuf;
    if file_id.is_empty() && has_reply {
        // TODO(responder-op): download replied zip (Python: ziptools.py:list_cmd)
        return Err(Error::Other(
            "downloading replied media requires a binary Responder op (Python: ziptools.py:list_cmd)"
                .to_string(),
        ));
    } else if !file_id.is_empty() {
        match resolve_file(&file_id) {
            Some(p) => file_path = p,
            None => {
                ctx.reply_html(&err_reply("Zip", "File not found")).await?;
                return Ok(());
            }
        }
    } else {
        ctx.reply_html(&fmtmsg(
            "Zip",
            &[(
                "Usage",
                format!(
                    "{}list <file_id> | reply to zip | -d flag",
                    ctx.config.hndlr
                ),
            )],
        ))
        .await?;
        return Ok(());
    }

    let _processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let entries = list_zip(&file_path).await?;
    let table = build_table(&entries, details_flag);
    ctx.reply_html(&fmtmsg(
        "Archive Contents",
        &[
            ("Count", entries.len().to_string()),
            (
                "Details",
                if details_flag { "✅" } else { "❌" }.to_string(),
            ),
            ("Table", table),
        ],
    ))
    .await?;
    if file_path
        .parent()
        .map(|p| p.to_string_lossy().contains("unzip"))
        .unwrap_or(false)
    {
        let _ = tokio::fs::remove_file(&file_path).await;
    }
    Ok(())
}

async fn azip_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply_html(&fmtmsg("Zip", &[("Error", ctx.get_str("zip_1"))]))
            .await?;
        return Ok(());
    }
    // TODO(responder-op): download replied media into `zipbatch/`
    // (Python: ziptools.py:azipp — `fns.downloader` / `event.download_media`)
    Err(Error::Other(
        "downloading replied media requires a binary Responder op (Python: ziptools.py:azipp)"
            .to_string(),
    ))
}

async fn dozip_cmd(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let mut password: Option<String> = None;
    let mut level: u32 = 6;
    let mut method = "deflate".to_string();

    if let Some(rest) = args.strip_prefix("-p ") {
        let parts: Vec<&str> = rest.splitn(3, char::is_whitespace).collect();
        if parts.len() >= 2 {
            password = Some(parts[0].to_string());
            level = parts[1].parse().unwrap_or(6);
        }
    } else if let Some(rest) = args.strip_prefix("-c ") {
        let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
        if !parts.is_empty() {
            level = parts[0].parse().unwrap_or(6);
        }
        if parts.len() >= 2 {
            method = parts[1].to_lowercase();
        }
    }

    let batch = PathBuf::from(BATCH_DIR);
    if !batch.is_dir() {
        ctx.reply_html(&fmtmsg(
            "Zip",
            &[(
                "Error",
                ctx.get_str("zip_2").replace("{}", &ctx.config.hndlr),
            )],
        ))
        .await?;
        return Ok(());
    }
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let files = all_files(&batch);
    if files.is_empty() {
        let _ = ctx.delete(processing).await;
        ctx.reply_html(&err_reply("Zip", "No files in batch"))
            .await?;
        return Ok(());
    }
    tokio::fs::create_dir_all(ZIP_CACHE_DIR).await?;
    let output_path = PathBuf::from(ZIP_CACHE_DIR).join("ultroid.zip");
    if let Err(e) = create_zip(&files, &output_path, password.as_deref(), level, &method).await {
        let _ = ctx.delete(processing).await;
        return Err(e);
    }
    ctx.send_file(&output_path, "").await?;
    let _ = tokio::fs::remove_dir_all(&batch).await;
    let _ = tokio::fs::remove_file(&output_path).await;
    let _ = ctx.delete(processing).await;
    Ok(())
}
