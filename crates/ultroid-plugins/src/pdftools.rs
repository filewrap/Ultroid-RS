//! PDF tools — port of `plugins/pdftools.py` (1,113 LOC).
//!
//! `.pdf` / `.pdtext` / `.pdocr` / `.pdsplit` / `.pdmerge` / `.pdscan` /
//! `.pdsave` / `.pdsend`.
//!
//! What is real here, and what is not — stated exactly, because this
//! plugin is where the dependency boundary bites:
//!
//! * **Real, std-only**: argument/flag/range parsing (`-o` / `-r`,
//!   `1-5,7,10-12` ranges, split targets), the `pdf/` working dir, output
//!   naming (`check_filename`, `_sanitize_name`), throttled progress
//!   edits, and the minimal PDF writer at the bottom of this file: a
//!   standards-conformant PDF 1.4 writer that embeds a JPEG as a
//!   `DCTDecode` image XObject (no compression library needed — the JPEG
//!   bytes are stored verbatim). It is used by `.pdscan` / `.pdsave` to
//!   produce real PDFs from images.
//! * **Real, subprocess**: page rendering via `pdftoppm` and page counts
//!   via `pdfinfo` (poppler) — Python shells out to the same tools
//!   through `pdf2image.convert_from_path`, which uses `pdfinfo` for the
//!   count and `pdftoppm` for the render. OCR via the `tesseract` binary
//!   (Python uses it through `pytesseract`). A missing binary surfaces
//!   as an honest error naming the binary.
//! * **Genuinely unimplementable with the available deps — honest
//!   errors, crate named**: everything Python does with `pypdf` /
//!   `PyPDF2` (`PdfReader` page access + `page.extract_text()`,
//!   `PdfWriter.add_page()` / `write()`) needs a PDF crate; there is
//!   none in `Cargo.toml` and PDF content streams are overwhelmingly
//!   Flate-compressed, so no std-only parser is possible. The crate
//!   that covers all of it is **`lopdf`** (read + `extract_text` +
//!   page insert/remove + write). Affected: `.pdtext` (text extraction),
//!   `.pdsplit` / `.pdmerge` / `.pdsend` (page copy + write), and the
//!   `.pdf` fallback that emits single-page PDFs when no renderer is
//!   available, plus `.pdocr`'s embedded-text fallback. Each of those
//!   sites returns `Err` naming `lopdf` + the exact Python call, only
//!   after doing all the real work around it (download, page-count,
//!   range validation).
//! * **Genuinely unimplementable — document scan**: `.pdscan` /
//!   `.pdsave` (image branch) run an OpenCV pipeline
//!   (`cv2.findContours` / `approxPolyDP` / `four_point_transform` /
//!   `skimage.filters.threshold_local` / `cv2.detailEnhance`). No
//!   OpenCV binding is in the deps; the scan step returns an honest
//!   error naming the **`opencv`** crate + the Python calls.
//! * **Telegram gate (TODO(responder-op))**: downloading the replied
//!   PDF/image (and, for `.pdmerge`, expanding a media-group album)
//!   needs the binary Responder op. `.pdsend` needs no Telegram op at
//!   all — it merges local `pdf/*.pdf` files — so it runs up to the
//!   `lopdf` merge step for real.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

const HELP: &str = "✘ Commands Available -\n\n\
• `.pdf <page num> <reply to pdf file>` — extract & send page(s) as image(s). All pages with plain `.pdf`; ranges like `.pdf 1-7`.\n\
• `.pdtext <page num> <reply to pdf file>` — extract text from the PDF.\n\
• `.pdocr [-r <range>] [-o <name>] <reply to pdf file>` — OCR-extract text (tesseract), falls back to embedded text.\n\
• `.pdsplit [-r <range>] [-o <name>] <reply to pdf file>` — split a PDF into separate files.\n\
• `.pdmerge [-o <name>] <reply to pdf/album>` — merge multiple PDFs into one.\n\
• `.pdscan <reply to image>` — scan, crop & send image(s) as PDF.\n\
• `.pdsave <reply to image/pdf>` — scan, crop & save file to merge.\n\
• `.pdsend [-o <name>]` — merge & send the PDF collected from .pdsave.";

const MAX_IMG_PAGES: usize = 50;
const MAX_OCR_PAGES: usize = 50;
const MAX_SPLIT_PARTS: usize = 30;

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("pdf", "pdf( (.*)|$)", HELP), |ctx| {
        Box::pin(pdf_cmd(ctx))
    });
    reg_add(registry, def("pdtext", "pdtext( (.*)|$)", HELP), |ctx| {
        Box::pin(pdtext_cmd(ctx))
    });
    reg_add(registry, def("pdocr", "pdocr( (.*)|$)", HELP), |ctx| {
        Box::pin(pdocr_cmd(ctx))
    });
    reg_add(registry, def("pdsplit", "pdsplit( (.*)|$)", HELP), |ctx| {
        Box::pin(pdsplit_cmd(ctx))
    });
    reg_add(registry, def("pdmerge", "pdmerge( (.*)|$)", HELP), |ctx| {
        Box::pin(pdmerge_cmd(ctx))
    });
    reg_add(registry, def("pdscan", "pdscan( (.*)|$)", HELP), |ctx| {
        Box::pin(pdscan_cmd(ctx))
    });
    reg_add(registry, def("pdsave", "pdsave( (.*)|$)", HELP), |ctx| {
        Box::pin(pdsave_cmd(ctx))
    });
    reg_add(registry, def("pdsend", "pdsend( (.*)|$)", HELP), |ctx| {
        Box::pin(pdsend_cmd(ctx))
    });
}

// ── small helpers (port of the Python helpers) ────────────────────────────

fn pdf_dir(ctx: &Ctx) -> PathBuf {
    PathBuf::from(&ctx.config.data_dir).join("pdf")
}

async fn ensure_pdf_dir(ctx: &Ctx) -> Result<PathBuf> {
    let dir = pdf_dir(ctx);
    tokio::fs::create_dir_all(&dir).await?;
    Ok(dir)
}

/// Port of `fns.check_filename`: append `_1`, `_2`, … before the extension.
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

fn sanitize_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = base
        .rsplit_once('.')
        .map(|(r, _)| r)
        .unwrap_or(&base)
        .replace(['/', '\\'], "_");
    if root.is_empty() {
        "pdf".to_string()
    } else {
        root
    }
}

fn cleanup(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

struct ParsedFlags {
    output: Option<String>,
    ranges: Option<String>,
    rest: String,
}

/// Port of `_parse_flags`: `-o/--output <name>`, `-r/--range <range>`.
fn parse_flags(text: &str) -> ParsedFlags {
    let mut out = ParsedFlags {
        output: None,
        ranges: None,
        rest: String::new(),
    };
    if text.is_empty() {
        return out;
    }
    // shlex-like split, tolerant of quoting errors (falls back to split()).
    let tokens: Vec<String> =
        shell_split(text).unwrap_or_else(|| text.split_whitespace().map(str::to_string).collect());
    let mut rest = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if (t == "-o" || t == "--output") && i + 1 < tokens.len() {
            out.output = Some(tokens[i + 1].clone());
            i += 2;
        } else if (t == "-r" || t == "--range") && i + 1 < tokens.len() {
            out.ranges = Some(tokens[i + 1].clone());
            i += 2;
        } else if let Some(v) = t.strip_prefix("-o") {
            if !v.is_empty() {
                out.output = Some(v.to_string());
            }
            i += 1;
        } else if let Some(v) = t.strip_prefix("-r") {
            if !v.is_empty() {
                out.ranges = Some(v.to_string());
            }
            i += 1;
        } else {
            rest.push(t.clone());
            i += 1;
        }
    }
    out.rest = rest.join(" ");
    out
}

fn shell_split(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut has_token = false;
    for c in text.chars() {
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
        } else if (c == ' ' || c == '\t') && !in_single && !in_double {
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
        return None;
    }
    if has_token || !cur.is_empty() {
        tokens.push(cur);
    }
    Some(tokens)
}

/// Port of `_parse_ranges`: `1-5,7,10-12` → sorted 0-based indices.
fn parse_ranges(spec: Option<&str>, total: usize) -> std::result::Result<Vec<usize>, String> {
    let Some(spec) = spec else {
        return Ok((0..total).collect());
    };
    let mut indices = std::collections::BTreeSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let (start, end): (usize, usize) = match (a.trim().parse(), b.trim().parse()) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return Err(format!("Invalid range: `{part}`")),
            };
            if start < 1 || end < start || end > total {
                return Err(format!("Range `{part}` out of bounds (1-{total})"));
            }
            indices.extend((start - 1)..end);
        } else {
            let p: usize = match part.parse() {
                Ok(p) => p,
                Err(_) => return Err(format!("Invalid page: `{part}`")),
            };
            if p < 1 || p > total {
                return Err(format!("Page `{part}` out of bounds (1-{total})"));
            }
            indices.insert(p - 1);
        }
    }
    Ok(indices.into_iter().collect())
}

/// Port of `_split_targets`: each comma part becomes one output file.
fn split_targets(
    spec: Option<&str>,
    total: usize,
) -> std::result::Result<Vec<(String, Vec<usize>)>, String> {
    let Some(spec) = spec else {
        return Ok((0..total).map(|i| ((i + 1).to_string(), vec![i])).collect());
    };
    let mut targets = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let (start, end): (usize, usize) = match (a.trim().parse(), b.trim().parse()) {
                (Ok(a), Ok(b)) => (a, b),
                _ => return Err(format!("Invalid range: `{part}`")),
            };
            if start < 1 || end < start || end > total {
                return Err(format!("Range `{part}` out of bounds (1-{total})"));
            }
            targets.push((format!("{start}-{end}"), (start - 1..end).collect()));
        } else {
            let p: usize = match part.parse() {
                Ok(p) => p,
                Err(_) => return Err(format!("Invalid page: `{part}`")),
            };
            if p < 1 || p > total {
                return Err(format!("Page `{part}` out of bounds (1-{total})"));
            }
            targets.push((p.to_string(), vec![p - 1]));
        }
    }
    Ok(targets)
}

/// Throttled progress edit (port of `_page_progress`, 3 s throttle).
async fn page_progress(
    ctx: &Ctx,
    msg_id: i64,
    current: usize,
    total: usize,
    action: &str,
    last: &mut Instant,
) {
    if total == 0 {
        return;
    }
    let now = Instant::now();
    let pct = current * 100 / total;
    if current == total || now.duration_since(*last) >= Duration::from_secs(3) {
        *last = now;
        let _ = ctx
            .edit(
                msg_id,
                &format!("`{action} {current}/{total} pages ({pct}%)...`"),
            )
            .await;
    }
}

// ── the honest-error constructors ─────────────────────────────────────────

/// A PDF operation Python does with `pypdf`/`PyPDF2` and no available
/// Rust crate can do. The covering crate is `lopdf`.
fn pdf_crate_error(op: &str, python_call: &str) -> Error {
    Error::Other(format!(
        "{op} needs a PDF crate (Python: {python_call}) — add `lopdf` to the workspace deps; no PDF library is available in this build and PDF content streams are Flate-compressed, so no std-only implementation is possible"
    ))
}

fn opencv_error(op: &str) -> Error {
    Error::Other(format!(
        "{op} needs OpenCV (Python: pdftools.py uses cv2.findContours / cv2.approxPolyDP / fns.four_point_transform / skimage.filters.threshold_local / cv2.detailEnhance) — add the `opencv` crate; no image-processing binding is available in this build"
    ))
}

// ── poppler / tesseract subprocesses ──────────────────────────────────────

/// Page count via `pdfinfo` (poppler). Python's `pdf2image` uses `pdfinfo`
/// for exactly this.
async fn pdf_page_count(path: &Path) -> Result<usize> {
    let out = Command::new("pdfinfo")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Other(
                    "pdfinfo not found — install poppler-utils (pdfinfo/pdftoppm)".to_string(),
                )
            } else {
                Error::Io(e)
            }
        })?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("Pages:") {
            if let Ok(n) = n.trim().parse::<usize>() {
                return Ok(n);
            }
        }
    }
    Err(Error::Other(format!(
        "pdfinfo could not read page count for {}",
        path.display()
    )))
}

/// Render 0-based page indices to PNGs via `pdftoppm` (poppler — the
/// renderer Python's `pdf2image.convert_from_path` shells out to).
/// Returns the rendered PNG paths, in page order.
async fn render_pages_to_images(
    path: &Path,
    indices: &[usize],
    dir: &Path,
) -> Result<Vec<PathBuf>> {
    let first = indices.iter().min().copied().unwrap_or(0) + 1;
    let last = indices.iter().max().copied().unwrap_or(0) + 1;
    let prefix = dir.join("ult");
    let out = Command::new("pdftoppm")
        .args([
            "-png",
            "-r",
            "150",
            "-f",
            &first.to_string(),
            "-l",
            &last.to_string(),
        ])
        .arg(path)
        .arg(&prefix)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Other(
                    "pdftoppm not found — install poppler-utils (pdfinfo/pdftoppm)".to_string(),
                )
            } else {
                Error::Io(e)
            }
        })?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "pdftoppm failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // pdftoppm names outputs `<prefix>-<pagenum>.png` (zero-padded).
    let mut rendered: Vec<(usize, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let num = name
                .strip_prefix("ult-")
                .and_then(|n| n.strip_suffix(".png"))
                .and_then(|n| n.trim_start_matches('0').parse::<usize>().ok());
            if let Some(num) = num {
                if num >= 1 && indices.contains(&(num - 1)) {
                    rendered.push((num - 1, e.path()));
                }
            }
        }
    }
    rendered.sort_by_key(|(n, _)| *n);
    Ok(rendered.into_iter().map(|(_, p)| p).collect())
}

/// Run `tesseract` on one PNG and return its text (Python: `pytesseract`).
async fn tesseract_png(png: &Path) -> Result<String> {
    let out = Command::new("tesseract")
        .arg(png)
        .arg("stdout")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Other("tesseract not found — install tesseract-ocr for .pdocr".to_string())
            } else {
                Error::Io(e)
            }
        })?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── Telegram gate ─────────────────────────────────────────────────────────

/// Download the replied PDF into the `pdf/` working dir (port of
/// `_download_reply_pdf`). The reply's media has to come from Telegram;
/// the filename is not on `Ctx` either.
async fn download_reply_pdf(_ctx: &Ctx) -> Result<PathBuf> {
    // TODO(responder-op): download the replied PDF message's document
    // (`fns.downloader(path, ok.media.document, ...)` — Python:
    // pdftools.py:_download_reply_pdf).
    Err(Error::Other(
        "download replied PDF file requires a binary Responder op (Python: pdftools.py:_download_reply_pdf)"
            .to_string(),
    ))
}

/// Resolve the `.pdf` / `.pdtext` page argument (empty → all pages,
/// `1-7` → range, `3` → single page) against the total page count.
fn resolve_page_arg(arg: &str, total: usize) -> std::result::Result<Vec<usize>, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Ok((0..total).collect());
    }
    if arg.contains('-') && !arg.contains(',') {
        let (u, d) = arg.split_once('-').unwrap();
        let (s, e): (usize, usize) = match (u.trim().parse(), d.trim().parse()) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return Err(format!("Invalid range: `{arg}`")),
        };
        if s < 1 || e < s || e > total {
            return Err(format!("Range out of bounds (1-{total})."));
        }
        return Ok((s - 1..e).collect());
    }
    let p: usize = match arg.trim().parse() {
        Ok(p) => p,
        Err(_) => return Err(format!("Invalid page: `{arg}`")),
    };
    if p < 1 || p > total {
        return Err(format!("Page out of bounds (1-{total})."));
    }
    Ok(vec![p - 1])
}

// ── commands ──────────────────────────────────────────────────────────────

async fn pdf_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to the PDF you want to extract pages from.`")
            .await?;
        return Ok(());
    }
    let msg = ctx.args.trim().to_string();
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let dir = ensure_pdf_dir(ctx).await?;
    let path = download_reply_pdf(ctx).await?;
    let total = pdf_page_count(&path).await?;
    if total == 0 {
        ctx.edit(status_id, "`This PDF has no pages (empty PDF).`")
            .await?;
        cleanup(&path);
        return Ok(());
    }
    let indices = match resolve_page_arg(&msg, total) {
        Ok(i) => i,
        Err(e) => {
            ctx.edit(status_id, &format!("`{e}`")).await?;
            cleanup(&path);
            return Ok(());
        }
    };
    if indices.len() > MAX_IMG_PAGES {
        ctx.edit(
            status_id,
            &format!(
                "`Too many pages ({}/{total}). Please specify a range (e.g. 1-{MAX_IMG_PAGES}).`",
                indices.len()
            ),
        )
        .await?;
        cleanup(&path);
        return Ok(());
    }
    ctx.edit(
        status_id,
        &format!("`Extracting {}/{total} page(s)...`", indices.len()),
    )
    .await?;
    let mut last = Instant::now();
    let rendered = render_pages_to_images(&path, &indices, &dir).await?;
    let mut out_files = Vec::new();
    for (n, png) in rendered.iter().enumerate() {
        out_files.push(png.clone());
        page_progress(ctx, status_id, n + 1, indices.len(), "Rendering", &mut last).await;
    }
    if out_files.is_empty() {
        // No renderer produced images — Python's fallback emits one
        // single-page PDF per requested page via PdfWriter.
        cleanup(&path);
        return Err(pdf_crate_error(
            "extract PDF pages as single-page PDFs (no renderer output)",
            "pdftools.py:pdfseimg — PdfWriter.add_page()/write()",
        ));
    }
    for f in &out_files {
        ctx.send_file(f, "").await?;
    }
    ctx.delete(status_id).await?;
    for f in &out_files {
        cleanup(f);
    }
    cleanup(&path);
    Ok(())
}

async fn pdtext_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to the PDF you want to extract text from.`")
            .await?;
        return Ok(());
    }
    let msg = ctx.args.trim().to_string();
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let _dir = ensure_pdf_dir(ctx).await?;
    let path = download_reply_pdf(ctx).await?;
    let total = pdf_page_count(&path).await?;
    let _indices = match resolve_page_arg(&msg, total) {
        Ok(i) => i,
        Err(e) => {
            ctx.edit(status_id, &format!("`{e}`")).await?;
            cleanup(&path);
            return Ok(());
        }
    };
    // Text extraction is `page.extract_text()` in Python (pypdf).
    cleanup(&path);
    Err(pdf_crate_error(
        "extract text from PDF pages",
        "pdftools.py:pdfsetxt — PdfReader.pages[i].extract_text()",
    ))
}

async fn pdocr_cmd(ctx: &Ctx) -> Result<()> {
    let flags = parse_flags(ctx.args.trim());
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a PDF file to OCR-extract its text.`")
            .await?;
        return Ok(());
    }
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let dir = ensure_pdf_dir(ctx).await?;
    let path = download_reply_pdf(ctx).await?;
    let total = pdf_page_count(&path).await?;
    let indices = match parse_ranges(flags.ranges.as_deref(), total) {
        Ok(i) => i,
        Err(e) => {
            ctx.edit(status_id, &format!("`{e}`")).await?;
            cleanup(&path);
            return Ok(());
        }
    };
    if indices.len() > MAX_OCR_PAGES {
        ctx.edit(
            status_id,
            &format!(
                "`Too many pages to OCR ({}/{total}). Use -r to select a range (e.g. -r 1-{MAX_OCR_PAGES}).`",
                indices.len()
            ),
        )
        .await?;
        cleanup(&path);
        return Ok(());
    }
    ctx.edit(
        status_id,
        &format!("`Running OCR on {}/{total} page(s)...`", indices.len()),
    )
    .await?;
    // Real OCR: render pages with pdftoppm, run tesseract per page.
    let rendered = render_pages_to_images(&path, &indices, &dir).await?;
    if rendered.is_empty() {
        cleanup(&path);
        return Err(Error::Other(
            "no pages could be rendered for OCR — pdftoppm produced no images".to_string(),
        ));
    }
    let mut last = Instant::now();
    let mut parts: Vec<String> = Vec::new();
    for (n, (idx, png)) in indices.iter().zip(rendered.iter()).enumerate() {
        let text = tesseract_png(png).await?;
        parts.push(format!("Page {}\n{}\n", idx + 1, "-".repeat(100)));
        parts.push(format!("{text}\n"));
        page_progress(ctx, status_id, n + 1, indices.len(), "OCR", &mut last).await;
        cleanup(png);
    }
    let text = parts.concat();
    if text.trim().is_empty() {
        ctx.edit(status_id, "`OCR returned no text.`").await?;
        cleanup(&path);
        return Ok(());
    }
    let base = flags.output.clone().unwrap_or_else(|| "file".to_string());
    let txtfile = check_filename(&PathBuf::from(format!(
        "{} (OCR).txt",
        sanitize_name(&base)
    )));
    tokio::fs::write(&txtfile, text.as_bytes()).await?;
    ctx.send_file(&txtfile, "").await?;
    ctx.edit(
        status_id,
        &format!("`Done — extracted via OCR, {} page(s).`", indices.len()),
    )
    .await?;
    cleanup(&txtfile);
    cleanup(&path);
    Ok(())
}

async fn pdsplit_cmd(ctx: &Ctx) -> Result<()> {
    let flags = parse_flags(ctx.args.trim());
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a PDF file to split it.`").await?;
        return Ok(());
    }
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let _dir = ensure_pdf_dir(ctx).await?;
    let path = download_reply_pdf(ctx).await?;
    let total = pdf_page_count(&path).await?;
    let targets = match split_targets(flags.ranges.as_deref(), total) {
        Ok(t) => t,
        Err(e) => {
            ctx.edit(status_id, &format!("`{e}`")).await?;
            cleanup(&path);
            return Ok(());
        }
    };
    if targets.len() > MAX_SPLIT_PARTS {
        ctx.edit(
            status_id,
            &format!(
                "`That would produce {} files (max {MAX_SPLIT_PARTS}). Use -r to group pages into fewer parts (e.g. -r 1-10,11-20).`",
                targets.len()
            ),
        )
        .await?;
        cleanup(&path);
        return Ok(());
    }
    let _base = flags.output.clone().unwrap_or_else(|| "file".to_string());
    ctx.edit(
        status_id,
        &format!(
            "`Splitting into {} part(s) from {total} pages...`",
            targets.len()
        ),
    )
    .await?;
    // Writing the split parts is `PdfWriter.add_page()` + `write()`.
    cleanup(&path);
    Err(pdf_crate_error(
        "split a PDF into per-page/per-range files",
        "pdftools.py:pdsplit_cmd — PdfWriter.add_page()/write()",
    ))
}

async fn pdmerge_cmd(ctx: &Ctx) -> Result<()> {
    let _flags = parse_flags(ctx.args.trim());
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a PDF (or an album of PDFs) to merge them.`")
            .await?;
        return Ok(());
    }
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let _dir = ensure_pdf_dir(ctx).await?;
    // TODO(responder-op): collect the replied PDF(s) — including the
    // media-group album expansion (`event.client.get_messages`) — and
    // download each (`fns.downloader`), then merge (Python:
    // pdftools.py:pdmerge_cmd / _collect_reply_pdfs / _merge_files).
    let _ = status_id;
    Err(Error::Other(
        "download replied PDF(s) for merge requires a binary Responder op (Python: pdftools.py:pdmerge_cmd)"
            .to_string(),
    ))
}

async fn pdscan_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a Image only...`").await?;
        return Ok(());
    }
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let dir = ensure_pdf_dir(ctx).await?;
    let img_path = download_reply_image(ctx).await?;
    let scanned = scan_document_image(&img_path)?;
    let (jpeg, w, h) = image_to_jpeg(&scanned).map_err(|e| Error::Other(e.to_string()))?;
    let stem = img_path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let out = check_filename(&dir.join(format!("Scanned {stem}.pdf")));
    write_image_pdf(&jpeg, w, h, &out)?;
    ctx.send_file(&out, "").await?;
    ctx.delete(status_id).await?;
    cleanup(&img_path);
    cleanup(&scanned);
    cleanup(&out);
    Ok(())
}

/// Download the replied image (port of the `ok.download_media()` call in
/// `pdftools.py:imgscan` / `savepdf`).
async fn download_reply_image(_ctx: &Ctx) -> Result<PathBuf> {
    // TODO(responder-op): download the replied image message's media
    // (Python: pdftools.py:imgscan / savepdf).
    Err(Error::Other(
        "download replied image requires a binary Responder op (Python: pdftools.py:imgscan)"
            .to_string(),
    ))
}

async fn pdsave_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to Images/pdf which u want to merge as a single pdf..`")
            .await?;
        return Ok(());
    }
    let dir = ensure_pdf_dir(ctx).await?;
    // The reply's file type (image vs PDF) is only known from the message
    // itself, so it is resolved behind the same download gate.
    let (downloaded, is_pdf) = download_reply_media(ctx).await?;
    let target = check_filename(&dir.join("scan.pdf"));
    if is_pdf {
        // PDF branch: save the file as-is for the later merge.
        tokio::fs::copy(&downloaded, &target).await?;
    } else {
        // Image branch: scan/crop, then save as a one-page PDF.
        let scanned = scan_document_image(&downloaded)?;
        let (jpeg, w, h) = image_to_jpeg(&scanned).map_err(|e| Error::Other(e.to_string()))?;
        write_image_pdf(&jpeg, w, h, &target)?;
        cleanup(&scanned);
    }
    ctx.reply(
        "Done, Now Reply Another Image/pdf if completed then use .pdsend to merge nd send all as pdf",
    )
    .await?;
    cleanup(&downloaded);
    Ok(())
}

/// Download the replied image/PDF and report whether it is a PDF (port of
/// the `ok.download_media()` call in `pdftools.py:savepdf`).
async fn download_reply_media(_ctx: &Ctx) -> Result<(PathBuf, bool)> {
    // TODO(responder-op): download the replied message's media and report
    // its kind (Python: pdftools.py:savepdf).
    Err(Error::Other(
        "download replied image/PDF requires a binary Responder op (Python: pdftools.py:savepdf)"
            .to_string(),
    ))
}

async fn pdsend_cmd(ctx: &Ctx) -> Result<()> {
    let flags = parse_flags(ctx.args.trim());
    let dir = pdf_dir(ctx);
    let mut afl: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("pdf") {
                afl.push(p);
            }
        }
    }
    afl.sort();
    if afl.is_empty() {
        ctx.reply(
            "first select pages by replying .pdsave of which u want to make multi page pdf file",
        )
        .await?;
        return Ok(());
    }
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    let _out = format!(
        "{}.pdf",
        sanitize_name(
            &flags
                .output
                .clone()
                .unwrap_or_else(|| "My PDF File".to_string())
        )
    );
    ctx.edit(status_id, &format!("`Merging {} file(s)...`", afl.len()))
        .await?;
    // Merging the collected PDFs is `PdfWriter.add_page()` + `write()`.
    Err(pdf_crate_error(
        "merge collected PDFs into one file",
        "pdftools.py:sendpdf / _merge_files — PdfWriter.add_page()/write()",
    ))
}

// ── document scan (OpenCV) + minimal PDF writer ──────────────────────────

/// The document-scan pipeline (Python: `cv2` resize → Y-channel → blur →
/// Canny → contours → `approxPolyDP` → `four_point_transform` →
/// `threshold_local`, else `cv2.detailEnhance`). Not implementable with
/// the available deps.
fn scan_document_image(_image_path: &Path) -> Result<PathBuf> {
    Err(opencv_error("scan/crop a document image"))
}

/// Write a one-page PDF embedding `jpeg` bytes (a JPEG file, e.g. produced
/// by the `image` crate) as a `DCTDecode` image XObject. Pure-std PDF 1.4
/// writer — the JPEG bytes are stored verbatim, so no compression library
/// is needed. Used by `.pdscan` / `.pdsave` to emit real PDFs from images
/// once the scan step (above) can run.
fn write_image_pdf(jpeg: &[u8], width: u32, height: u32, out: &Path) -> std::io::Result<()> {
    let mut offsets: Vec<usize> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n"
        )
        .as_bytes(),
    );
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(jpeg);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    let content = format!("q\n{width} 0 0 {height} 0 0 cm\n/Im0 Do\nQ\n");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
            content.len()
        )
        .as_bytes(),
    );
    let xref_pos = buf.len();
    buf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for off in &offsets {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n").as_bytes(),
    );
    std::fs::write(out, buf)
}

/// Encode an image file to JPEG bytes via the `image` crate (used with
/// `write_image_pdf` to produce the PDF for `.pdscan` / `.pdsave`).
fn image_to_jpeg(path: &Path) -> std::result::Result<(Vec<u8>, u32, u32), image::ImageError> {
    let img = image::open(path)?.into_rgb8();
    let (w, h) = (img.width(), img.height());
    let mut bytes: Vec<u8> = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut bytes);
    enc.encode_image(&img)?;
    Ok((bytes, w, h))
}
