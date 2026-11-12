//! Media info & rotation — port of `plugins/mediatools.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.mediainfo [-m] [-a] <reply to media>/<file path>/<url>`
//!   Get info about media. Flags: `-m` extended metadata (EXIF for
//!   images, codec info for video/audio); `-a` when replying to an album,
//!   show info for ALL files (default: first only).
//! - `.rotate <degree/angle> <reply to media>` — rotate any
//!   video/photo/media (video: angle must be a multiple of 90).
//!
//! A local file path or an `https://` URL may be passed as the argument;
//! replied-to Telegram media is downloaded via the Responder
//! (`download_media`). Resolving an album (`-a`) still needs a message
//! lookup the Responder does not offer (see the TODO in `mediainfo`).

use std::path::{Path, PathBuf};
use tokio::process::Command as ProcCommand;
use ultroid_core::fns::{humanbytes, time_formatter};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_get, db_set, def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "mediainfo",
            "mediainfo( (.*)|$)",
            "Media info: .mediainfo [-m] [-a] <reply to media>/<file path>/<url>.",
        ),
        |ctx| Box::pin(mediainfo(ctx)),
    );
    reg_add(
        registry,
        def(
            "rotate",
            "rotate( (.*)|$)",
            "Rotate media: .rotate <degrees> (reply to media).",
        ),
        |ctx| Box::pin(rotate(ctx)),
    );
}

// ── shared helpers ────────────────────────────────────────────────────────

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
    } else {
        None
    }
}

/// Rename `path` in place to carry the extension sniffed from its magic
/// bytes, when recognised — the extension-based type detection below
/// (`detect_type_from_path`) needs it.
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

/// Download the replied message's media via the Responder, with a sniffed
/// extension (see [`with_sniffed_ext`]).
async fn download_reply(ctx: &Ctx, stem: &str) -> Result<PathBuf> {
    let reply_id = ctx
        .reply_to_id
        .ok_or_else(|| Error::Other("reply to a message first".into()))?;
    let dest = tmp_file(stem);
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    Ok(with_sniffed_ext(&path))
}

fn tmp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ultroid-{}-{}", name, uuid::Uuid::new_v4()))
}

fn fmt_duration(seconds: f64) -> String {
    let ms = (seconds * 1000.0) as u64;
    let f = time_formatter(ms);
    if f.is_empty() {
        format!("{seconds:.1}s")
    } else {
        f
    }
}

const EXT_IMAGE: [&str; 11] = [
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "tif", "heic", "heif", "avif",
];
const EXT_VIDEO: [&str; 12] = [
    "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "3gp", "mpeg", "mpg", "ts",
];
const EXT_AUDIO: [&str; 10] = [
    "mp3", "wav", "flac", "ogg", "aac", "m4a", "opus", "wma", "alac", "aiff",
];

fn detect_type_from_path(path: &str) -> Option<&'static str> {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if EXT_IMAGE.iter().any(|e| *e == ext) {
        Some("image")
    } else if EXT_VIDEO.iter().any(|e| *e == ext) {
        Some("video")
    } else if EXT_AUDIO.iter().any(|e| *e == ext) {
        Some("audio")
    } else {
        None
    }
}

async fn ffprobe_json(path: &Path) -> Result<serde_json::Value> {
    let out = ProcCommand::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            path.to_str().unwrap_or(""),
        ])
        .output()
        .await
        .map_err(|e| Error::Other(format!("ffprobe not found or failed to run: {e}")))?;
    Ok(serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null))
}

fn parse_fps(rate: &str) -> Option<f64> {
    if rate.is_empty() || rate == "0/0" {
        return None;
    }
    let (n, d) = rate.split_once('/')?;
    let den: f64 = d.parse().ok().filter(|v| *v != 0.0)?;
    let num: f64 = n.parse().ok()?;
    Some((num / den * 100.0).round() / 100.0)
}

/// Flat codec/container metadata from an ffprobe JSON dict
/// (Python `_codec_info`).
fn codec_info(probe: &serde_json::Value) -> Vec<(String, String)> {
    let mut info = Vec::new();
    let fmt = probe.get("format").cloned().unwrap_or_default();
    if let Some(v) = fmt.get("format_name").and_then(|v| v.as_str()) {
        info.push(("container".into(), v.to_string()));
    }
    if let Some(v) = fmt.get("duration").and_then(|v| v.as_str()) {
        info.push(("duration".into(), v.to_string()));
    }
    if let Some(v) = fmt.get("bit_rate").and_then(|v| v.as_str()) {
        info.push(("bitrate".into(), v.to_string()));
    }
    let streams = probe
        .get("streams")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let v = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some("video"));
    let a = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some("audio"));
    if let Some(v) = v {
        if let Some(c) = v.get("codec_name").and_then(|v| v.as_str()) {
            info.push(("video_codec".into(), c.to_string()));
        }
        if let (Some(w), Some(h)) = (
            v.get("width").and_then(|v| v.as_u64()),
            v.get("height").and_then(|v| v.as_u64()),
        ) {
            info.push(("width".into(), w.to_string()));
            info.push(("height".into(), h.to_string()));
        }
        let rate = v
            .get("avg_frame_rate")
            .and_then(|v| v.as_str())
            .or_else(|| v.get("r_frame_rate").and_then(|v| v.as_str()))
            .unwrap_or("");
        if let Some(fps) = parse_fps(rate) {
            info.push(("fps".into(), fps.to_string()));
        }
        if let Some(b) = v.get("bit_rate").and_then(|v| v.as_str()) {
            info.push(("video_bitrate".into(), b.to_string()));
        }
    }
    if let Some(a) = a {
        if let Some(c) = a.get("codec_name").and_then(|v| v.as_str()) {
            info.push(("audio_codec".into(), c.to_string()));
        }
        if let Some(s) = a.get("sample_rate").and_then(|v| v.as_str()) {
            info.push(("sample_rate".into(), s.to_string()));
        }
        if let Some(c) = a.get("channels").and_then(|v| v.as_u64()) {
            info.push(("channels".into(), c.to_string()));
        }
        if let Some(b) = a.get("bit_rate").and_then(|v| v.as_str()) {
            info.push(("audio_bitrate".into(), b.to_string()));
        }
    }
    info
}

fn fmt_metadata(meta: &[(String, String)]) -> String {
    if meta.is_empty() {
        return "_No extended metadata available._".to_string();
    }
    let get = |k: &str| {
        meta.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let mut lines = Vec::new();
    if let Some(v) = get("format") {
        lines.push(format!("  • Format: {v}"));
    }
    if let Some(v) = get("container") {
        lines.push(format!("  • Container: {v}"));
    }
    if let Some(v) = get("camera") {
        lines.push(format!("  • Camera: {v}"));
    }
    if let Some(v) = get("date_taken") {
        lines.push(format!("  • Date Taken: {v}"));
    }
    if let Some(v) = get("software") {
        lines.push(format!("  • Software: {v}"));
    }
    if let Some(v) = get("gps") {
        lines.push(format!("  • GPS: {v}"));
    }
    if let Some(v) = get("video_codec") {
        lines.push(format!("  • Video Codec: {v}"));
    }
    if let (Some(w), Some(h)) = (get("width"), get("height")) {
        lines.push(format!("  • Resolution: {w}x{h}"));
    }
    if let Some(v) = get("fps") {
        lines.push(format!("  • FPS: {v}"));
    }
    if let Some(v) = get("audio_codec") {
        lines.push(format!("  • Audio Codec: {v}"));
    }
    if let Some(v) = get("sample_rate") {
        lines.push(format!("  • Sample Rate: {v} Hz"));
    }
    if let Some(v) = get("channels") {
        let ch: u64 = v.parse().unwrap_or(0);
        let label = match ch {
            1 => "Mono".to_string(),
            2 => "Stereo".to_string(),
            _ => format!("{ch}ch"),
        };
        lines.push(format!("  • Channels: {label}"));
    }
    if let Some(v) = get("duration") {
        if let Ok(d) = v.parse::<f64>() {
            lines.push(format!("  • Duration: {}", fmt_duration(d)));
        }
    }
    for key in ["bitrate", "video_bitrate", "audio_bitrate"] {
        if let Some(v) = get(key) {
            if let Ok(b) = v.parse::<u64>() {
                let label = match key {
                    "bitrate" => "Bitrate",
                    "video_bitrate" => "Video Bitrate",
                    _ => "Audio Bitrate",
                };
                lines.push(format!("  • {label}: {}/s", humanbytes(b / 8)));
            }
        }
    }
    if lines.is_empty() {
        return "_No extended metadata available._".to_string();
    }
    format!("**Metadata:**\n{}", lines.join("\n"))
}

// ── minimal EXIF reader (JPEG/TIFF; no exif crate in workspace deps) ─────

fn read_u16(b: &[u8], off: usize, le: bool) -> u16 {
    if le {
        u16::from_le_bytes([b[off], b[off + 1]])
    } else {
        u16::from_be_bytes([b[off], b[off + 1]])
    }
}

fn read_u32(b: &[u8], off: usize, le: bool) -> u32 {
    if le {
        u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
    } else {
        u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
    }
}

fn exif_ascii(tiff: &[u8], base: usize, off: usize, count: u32, le: bool) -> Option<String> {
    let count = count as usize;
    let bytes = if count <= 4 {
        // Value stored inline in the 4-byte value field.
        let start = base + off;
        tiff.get(start..start + count)?.to_vec()
    } else {
        let ptr = read_u32(tiff, base + off, le) as usize;
        tiff.get(ptr..ptr + count)?.to_vec()
    };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec()).ok()
}

fn gps_decimal(vals: &[f64], reference: &str) -> Option<f64> {
    if vals.len() < 3 {
        return None;
    }
    let mut dec = vals[0] + vals[1] / 60.0 + vals[2] / 3600.0;
    if reference.eq_ignore_ascii_case("S") || reference.eq_ignore_ascii_case("W") {
        dec = -dec;
    }
    Some(dec)
}

/// Parse TIFF IFD0 + GPS IFD. Returns flat (key, value) pairs
/// (Python `_extract_exif` subset: format/camera/date/software/gps).
fn extract_exif(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(data) = std::fs::read(path) else {
        return out;
    };
    // Find TIFF header: JPEG APP1 ("Exif\0\0" + TIFF) or raw TIFF.
    let tiff_start = if data.len() > 12 && &data[0..2] == b"\xff\xd8" {
        // Scan JPEG segments for APP1 Exif.
        let mut i = 2usize;
        let mut found = None;
        while i + 4 <= data.len() {
            if data[i] != 0xFF {
                break;
            }
            let marker = data[i + 1];
            if marker == 0xDA {
                break; // SOS
            }
            let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            if marker == 0xE1 && i + 4 + 6 <= data.len() && &data[i + 4..i + 10] == b"Exif\0\0" {
                found = Some(i + 10);
                break;
            }
            if len < 2 {
                break;
            }
            i += 2 + len;
        }
        found
    } else if data.len() > 8 && (&data[0..2] == b"II" || &data[0..2] == b"MM") {
        Some(0)
    } else {
        None
    };
    let Some(start) = tiff_start else {
        return out;
    };
    let tiff = &data[start..];
    if tiff.len() < 8 {
        return out;
    }
    let le = &tiff[0..2] == b"II";
    if read_u16(tiff, 2, le) != 42 {
        return out;
    }
    let ifd0_off = read_u32(tiff, 4, le) as usize;
    let Some((tags, gps_off)) = read_ifd(tiff, ifd0_off, le) else {
        return out;
    };
    let get_ascii = |tag: u16| -> Option<String> {
        let (typ, count, voff) = tags.get(&tag)?;
        if *typ != 2 {
            return None;
        }
        exif_ascii(tiff, 0, *voff, *count, le)
    };
    // Image format comes from the image crate, not EXIF.
    let make = get_ascii(0x010F);
    let model = get_ascii(0x0110);
    let camera = [make, model]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if !camera.trim().is_empty() {
        out.push(("camera".into(), camera.trim().to_string()));
    }
    if let Some(v) = get_ascii(0x0132) {
        out.push(("date_taken".into(), v));
    }
    if let Some(v) = get_ascii(0x0131) {
        out.push(("software".into(), v));
    }
    if let Some(gps) = gps_off {
        if let Some((gtags, _)) = read_ifd(tiff, gps, le) {
            let lat_ref = gtags
                .get(&1)
                .and_then(|(t, c, o)| {
                    if *t == 2 {
                        exif_ascii(tiff, 0, *o, *c, le)
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let lon_ref = gtags
                .get(&3)
                .and_then(|(t, c, o)| {
                    if *t == 2 {
                        exif_ascii(tiff, 0, *o, *c, le)
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let rationals = |tag: u16| -> Vec<f64> {
                let Some((typ, count, voff)) = gtags.get(&tag) else {
                    return vec![];
                };
                if *typ != 5 {
                    return vec![];
                }
                let ptr = read_u32(tiff, *voff, le) as usize;
                (0..*count)
                    .filter_map(|i| {
                        let n = read_u32(tiff, ptr + (i as usize) * 8, le) as f64;
                        let d = read_u32(tiff, ptr + (i as usize) * 8 + 4, le) as f64;
                        if d == 0.0 { None } else { Some(n / d) }
                    })
                    .collect()
            };
            if let (Some(lat), Some(lon)) = (
                gps_decimal(&rationals(2), &lat_ref),
                gps_decimal(&rationals(4), &lon_ref),
            ) {
                out.push(("gps".into(), format!("{lat:.6}, {lon:.6}")));
            }
        }
    }
    out
}

/// Read one IFD. Returns (tag -> (type, count, value_offset), gps_ifd_offset).
fn read_ifd(
    tiff: &[u8],
    off: usize,
    le: bool,
) -> Option<(
    std::collections::HashMap<u16, (u16, u32, usize)>,
    Option<usize>,
)> {
    if off + 2 > tiff.len() {
        return None;
    }
    let count = read_u16(tiff, off, le) as usize;
    let mut tags = std::collections::HashMap::new();
    let mut gps = None;
    for i in 0..count {
        let e = off + 2 + i * 12;
        if e + 12 > tiff.len() {
            break;
        }
        let tag = read_u16(tiff, e, le);
        let typ = read_u16(tiff, e + 2, le);
        let cnt = read_u32(tiff, e + 4, le);
        let voff = e + 8;
        if tag == 0x8825 {
            gps = Some(read_u32(tiff, voff, le) as usize);
        }
        tags.insert(tag, (typ, cnt, voff));
    }
    Some((tags, gps))
}

async fn collect_metadata(path: &Path, mtype: &str) -> Vec<(String, String)> {
    let mut meta = Vec::new();
    let m = mtype.to_lowercase();
    let is_image =
        (m.contains("image") || m.contains("pic") || m.contains("photo")) && !m.contains("gif");
    let is_av = m.contains("video") || m.contains("audio") || m.contains("gif");
    if is_image {
        if let Ok(img) = image::open(path) {
            meta.push(("format".into(), format!("{:?}", img.color())));
        }
        meta.extend(extract_exif(path));
    }
    if is_av || !is_image {
        if let Ok(probe) = ffprobe_json(path).await {
            if !probe.is_null() {
                meta.extend(codec_info(&probe));
            }
        }
    }
    meta
}

// ── telegraph (JSON API, no telegraph crate in workspace deps) ───────────

async fn telegraph_token(ctx: &Ctx) -> Result<String> {
    if let Some(tok) = db_get(ctx, "TELEGRAPH_TOKEN").await? {
        if !tok.is_empty() {
            return Ok(tok);
        }
    }
    let resp = ctx
        .http
        .post("https://api.telegra.ph/createAccount")
        .json(&serde_json::json!({"short_name": "Ultroid", "author_name": "Ultroid"}))
        .send()
        .await?;
    let val: serde_json::Value = resp.json().await?;
    let tok = val
        .get("result")
        .and_then(|r| r.get("access_token"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("telegra.ph createAccount failed: {val}")))?
        .to_string();
    db_set(ctx, "TELEGRAPH_TOKEN", &tok).await?;
    Ok(tok)
}

async fn make_html_telegraph(ctx: &Ctx, title: &str, html: &str) -> Result<String> {
    let token = telegraph_token(ctx).await?;
    // Telegraph createPage content nodes: a single <p> with raw HTML is not
    // accepted; convert lines to <p>/<h3> text nodes.
    let mut nodes = Vec::new();
    for line in html.lines() {
        let text = line
            .replace("<br>", "")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        let tag = if line.trim_start().starts_with("<h3>") {
            "h3"
        } else {
            "p"
        };
        let clean = text
            .replace("<h3>", "")
            .replace("</h3>", "")
            .replace("<p>", "")
            .replace("</p>", "")
            .replace("<img ", "[image] ")
            .replace('\'', "");
        nodes.push(serde_json::json!({"tag": tag, "children": [clean]}));
    }
    let resp = ctx
        .http
        .post("https://api.telegra.ph/createPage")
        .json(&serde_json::json!({
            "access_token": token,
            "title": title,
            "content": nodes,
        }))
        .send()
        .await?;
    let val: serde_json::Value = resp.json().await?;
    let url = val
        .get("result")
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("telegra.ph createPage failed: {val}")))?
        .to_string();
    Ok(url)
}

/// `mediainfo` binary output if present, else a ffprobe-derived summary.
async fn mediainfo_cli(path: &Path) -> Option<String> {
    let out = ProcCommand::new("mediainfo")
        .arg(path.to_str().unwrap_or(""))
        .output()
        .await
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

// ── commands ──────────────────────────────────────────────────────────────

fn parse_flags(raw: &str) -> (bool, bool, String) {
    let mut show_meta = false;
    let mut show_all = false;
    let mut remaining = Vec::new();
    for token in raw.split_whitespace() {
        match token {
            "-m" | "--meta" | "--metadata" => show_meta = true,
            "-a" | "--all" => show_all = true,
            _ => remaining.push(token),
        }
    }
    (show_meta, show_all, remaining.join(" "))
}

async fn download_url(ctx: &Ctx, url: &str) -> Result<Option<PathBuf>> {
    let resp = ctx.http.get(url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let bytes = resp.bytes().await?;
    let name = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("file");
    let path = std::env::temp_dir().join(format!(
        "ultroid-mi_{}_{name}",
        chrono::Utc::now().timestamp_millis()
    ));
    tokio::fs::write(&path, &bytes).await?;
    Ok(Some(path))
}

async fn mediainfo(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.clone();
    let (show_meta, show_all, matched) = parse_flags(&raw);
    if show_all {
        // Album expansion needs the replied message's grouped_id and a
        // message-window lookup — both Telegram-client ops.
        // TODO(responder-op): resolve album by grouped_id
        // (Python: mediatools.py:_resolve_album).
        if ctx.reply_to_id.is_some() {
            return Err(Error::Other(
                "resolving an album by grouped_id requires a binary Responder op (Python: mediatools.py:_resolve_album)".into(),
            ));
        }
    }
    // Resolve the target: local file, URL, or replied media.
    let (path, temp, mtype): (PathBuf, bool, String) =
        if !matched.is_empty() && Path::new(&matched).is_file() {
            let t = detect_type_from_path(&matched)
                .unwrap_or("file")
                .to_string();
            (PathBuf::from(&matched), false, t)
        } else if matched.starts_with("https://") {
            match download_url(ctx, &matched).await? {
                Some(p) => {
                    let t = detect_type_from_path(&matched)
                        .unwrap_or("file")
                        .to_string();
                    (p, true, t)
                }
                None => {
                    ctx.reply("Media Info — Error: Could not download URL")
                        .await?;
                    return Ok(());
                }
            }
        } else if ctx.reply_to_id.is_some() {
            let p = download_reply(ctx, "media").await?;
            let t = detect_type_from_path(&p.to_string_lossy())
                .unwrap_or("file")
                .to_string();
            (p, true, t)
        } else {
            ctx.reply("Reply to media or give a file path / URL")
                .await?;
            return Ok(());
        };

    let msg = ctx.reply("Media Info — Status: Loading...").await?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let size = tokio::fs::metadata(&path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut basic = format!(
        "**Type:** {mtype}\n**Name:** `{name}`\n**Size:** {}",
        humanbytes(size)
    );
    // Image dimensions via the image crate.
    if mtype == "image" {
        if let Ok(img) = image::open(&path) {
            basic.push_str(&format!(
                "\n**Dimensions:** {}x{}",
                img.width(),
                img.height()
            ));
        }
    }
    // Extended metadata (-m).
    let mut meta_block = String::new();
    if show_meta {
        let meta = collect_metadata(&path, &mtype).await;
        meta_block = format!("\n\n{}", fmt_metadata(&meta));
    }
    // Full `mediainfo` output -> telegraph page, like Python.
    let mut link = String::new();
    if let Some(out) = mediainfo_cli(&path).await {
        let mut makehtml = String::new();
        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() {
                makehtml.push_str("<br>\n");
            } else if !line.contains(':') {
                makehtml.push_str(&format!("<h3>{line}</h3>\n"));
            } else {
                makehtml.push_str(&format!("<p>{line}</p>\n"));
            }
        }
        if let Ok(url) = make_html_telegraph(ctx, "Mediainfo", &makehtml).await {
            link = format!("\n\n[Full Details]({url})");
        }
    }
    ctx.edit(msg, &format!("{basic}{meta_block}{link}")).await?;
    if temp {
        let _ = tokio::fs::remove_file(&path).await;
    }
    Ok(())
}

async fn rotate(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a media...`").await?;
        return Ok(());
    }
    let first = ctx.args.split_whitespace().next().unwrap_or("");
    let angle: i64 = match first.parse() {
        Ok(v) => v,
        Err(_) => {
            ctx.reply("`Please provide a valid angle to rotate media..`")
                .await?;
            return Ok(());
        }
    };
    // The replied media (video file or photo) must be downloaded first.
    let input = match ctx
        .args
        .split_whitespace()
        .skip(1)
        .map(PathBuf::from)
        .find(|p| p.is_file())
    {
        Some(p) => p,
        None => download_reply(ctx, "rotate").await?,
    };
    let msg = ctx.reply("Processing...").await?;
    let is_video = matches!(
        detect_type_from_path(&input.to_string_lossy()),
        Some("video")
    );
    if is_video {
        // Python: ffmpeg -c copy -metadata:s:v:0 rotate=<angle>.
        let out = tmp_file("rotated.mp4");
        let run = ProcCommand::new("ffmpeg")
            .args([
                "-i",
                input.to_str().unwrap_or(""),
                "-c",
                "copy",
                "-metadata:s:v:0",
                &format!("rotate={angle}"),
                out.to_str().unwrap_or(""),
                "-y",
            ])
            .output()
            .await
            .map_err(|e| Error::Other(format!("ffmpeg not found or failed to run: {e}")))?;
        if !run.status.success() {
            return Err(Error::Other(format!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&run.stderr).trim()
            )));
        }
        ctx.send_file(&out, "rotated video").await?;
        let _ = tokio::fs::remove_file(&out).await;
    } else {
        let img = image::open(&input).map_err(|e| Error::Other(format!("image open: {e}")))?;
        let rotated = match angle.rem_euclid(360) {
            90 => img.rotate90(),
            180 => img.rotate180(),
            270 => img.rotate270(),
            _ => rotate_any(&img, angle as f64),
        };
        let out = tmp_file("ult.png");
        rotated
            .save(&out)
            .map_err(|e| Error::Other(format!("image save: {e}")))?;
        ctx.send_file(&out, "rotated image").await?;
        let _ = tokio::fs::remove_file(&out).await;
    }
    ctx.delete(msg).await?;
    Ok(())
}

/// Nearest-neighbour arbitrary rotation (same approach as imaging.rs;
/// kept local so this module stays self-contained).
fn rotate_any(img: &image::DynamicImage, degrees: f64) -> image::DynamicImage {
    use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba};
    let (w, h) = img.dimensions();
    let rad = degrees.to_radians();
    let (sin, cos) = (rad.sin(), rad.cos());
    let nw = ((w as f64 * cos.abs() + h as f64 * sin.abs()).ceil() as u32).max(1);
    let nh = ((w as f64 * sin.abs() + h as f64 * cos.abs()).ceil() as u32).max(1);
    let src = img.to_rgba8();
    let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(nw, nh);
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let (ncx, ncy) = (nw as f64 / 2.0, nh as f64 / 2.0);
    for y in 0..nh {
        for x in 0..nw {
            let dx = x as f64 - ncx;
            let dy = y as f64 - ncy;
            let sx = (cos * dx + sin * dy + cx).round() as i64;
            let sy = (-sin * dx + cos * dy + cy).round() as i64;
            if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                out.put_pixel(x, y, *src.get_pixel(sx as u32, sy as u32));
            }
        }
    }
    DynamicImage::ImageRgba8(out)
}
