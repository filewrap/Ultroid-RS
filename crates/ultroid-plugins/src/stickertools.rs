//! Sticker tools — port of `plugins/stickertools.py` (749 LOC).
//!
//! * `.pamck [dir]` — kang a whole sticker pack (or a local dir of
//!   `.tgs`/`.webm` files) into a new pack.
//! * `.kang [emoji] [pack]` — kang the replied image/sticker/video to
//!   your pack via @Stickers.
//! * `.round` — extract a round sticker from replied media.
//! * `.destroy` — "destroy" a replied animated sticker (lottie mangling).
//! * `.tiny` — create a tiny sticker from replied media.
//! * `.emojiset <pack-link-or-shortname>` — list a custom-emoji pack.
//! * `.emojireply <shortname:index|doc_id> [text]` — reply with a custom emoji.
//!
//! The sticker-set fetch (`.emojiset`) is wired to the `Responder`
//! `sticker_set` op, and `.round` / `.destroy` / `.tiny` are wired to
//! `download_media` plus the pure media helpers in this file. The
//! @Stickers bot conversation (`.kang`, `.pamck` pack creation) and
//! sending a custom emoji (`.emojireply`, which needs a custom-emoji
//! message entity) have no trait op and still return honest
//! `TODO(responder-op)` errors, as does everything else whose op is
//! missing.
//! Local media work uses the `image` crate and ffmpeg; `.tgs` work
//! drives the `lottie_convert.py` CLI (same tool Python's `con` uses).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;

use crate::util::{def, reg_add};
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use regex::Regex;
use tokio::process::Command as AsyncCommand;
use ultroid_core::{Ctx, Error, Registry, Result};

const HELP: &str = "✘ Commands Available -\n\n• `{i}destroy <reply to animated sticker>`\n    To destroy the sticker.\n\n• `{i}tiny <reply to media>`\n    To create Tiny stickers.\n\n• `{i}kang <reply to image/sticker>`\n    Kang the sticker (add to your pack).\n\n• `{i}packkang <pack name>`\n    Kang the Complete sticker set (with custom name).\n\n• `{i}round <reply to any media>`\n    To extract round sticker.\n\n• `{i}emojiset <pack-link-or-shortname>`\n    List a custom-emoji pack's contents (index, glyph, document id).\n\n• `{i}emojireply <shortname:index or doc_id> [text]`\n    Reply with a custom emoji looked up via `{i}emojiset`.";

pub const KANGING_STR: [&str; 10] = [
    "Using Witchery to kang this sticker...",
    "Plagiarising hehe...",
    "Inviting this sticker over to my pack...",
    "Kanging this sticker...",
    "Hey that's a nice sticker!\nMind if I kang?!..",
    "Hehe me stel ur stiker...",
    "Ay look over there (☉｡☉)!→\nWhile I kang this...",
    "Roses are red violets are blue, kanging this sticker so my pack looks cool",
    "Imprisoning this sticker...",
    "Mr.Steal-Your-Sticker is stealing this sticker... ",
];

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("pamck", "pamck", HELP), |ctx| {
        Box::pin(pack_kang(ctx))
    });
    reg_add(registry, def("kang", "kang", HELP), |ctx| {
        Box::pin(kang(ctx))
    });
    reg_add(registry, def("round", "round$", HELP), |ctx| {
        Box::pin(round_cmd(ctx))
    });
    reg_add(registry, def("destroy", "destroy$", HELP), |ctx| {
        Box::pin(destroy_cmd(ctx))
    });
    reg_add(registry, def("tiny", "tiny$", HELP), |ctx| {
        Box::pin(tiny_cmd(ctx))
    });
    reg_add(
        registry,
        def("emojiset", "emojiset( (.*)|$)", HELP),
        |ctx| Box::pin(emoji_set_list(ctx)),
    );
    reg_add(
        registry,
        def("emojireply", "emojireply( (.*)|$)", HELP),
        |ctx| Box::pin(emoji_reply(ctx)),
    );
}

// ── small pure helpers ────────────────────────────────────────────────────

/// Python `random_chars(k=8)`: `k` random ASCII letters.
pub fn random_chars(k: usize) -> String {
    use rand::Rng;
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::thread_rng();
    (0..k)
        .map(|_| LETTERS[rng.gen_range(0..LETTERS.len())] as char)
        .collect()
}

/// Python `str(uuid4())[:16].replace("-", "")` — short-name seed for a new pack.
pub fn pack_short_name_seed() -> String {
    uuid::Uuid::new_v4()
        .to_string()
        .chars()
        .take(16)
        .filter(|c| *c != '-')
        .collect()
}

/// Check whether a sticker pack already exists on Telegram by fetching its
/// public page (Python `kang`'s `fns.async_searcher` check): the page
/// contains "A <strong>Telegram</strong> user has created the
/// <strong>Sticker&nbsp;Set</strong>." only when the pack does NOT exist.
pub async fn pack_exists(http: &reqwest::Client, packname: &str) -> Result<bool> {
    let resp = http
        .get(format!("http://t.me/addstickers/{packname}"))
        .send()
        .await?;
    let body = resp.text().await?;
    Ok(!body.contains(
        "  A <strong>Telegram</strong> user has created the <strong>Sticker&nbsp;Set</strong>.",
    ))
}

/// Parse `.kang [emoji] [pack]` args (Python `kang`'s `splat` logic).
/// Returns `(pack_number, emoji_override)`.
pub fn parse_kang_args(text: &str) -> (u32, Option<String>) {
    let splat: Vec<&str> = text.split_whitespace().collect();
    let mut pack = 1u32;
    let mut emoji: Option<String> = None;
    if splat.len() == 3 {
        pack = splat[2].parse().unwrap_or(1);
        emoji = Some(splat[1].to_string());
    } else if splat.len() == 2 {
        if splat[1].chars().all(|c| c.is_ascii_digit()) {
            pack = splat[1].parse().unwrap_or(1);
        } else {
            emoji = Some(splat[1].to_string());
        }
    }
    (pack, emoji)
}

/// Pack short-name / title for a kang (Python `kang`).
pub fn kang_pack_names(user_id: i64, username: &str, pack: u32, kind: &str) -> (String, String) {
    let mut packname = format!("ult_{user_id}_{pack}");
    let mut packnick = format!("{username}'s Pack {pack}");
    match kind {
        "video" => {
            packname += "_vid";
            packnick += " (Video)";
        }
        "animated" => {
            packname += "_anim";
            packnick += " (Animated)";
        }
        _ => {}
    }
    (packname, packnick)
}

/// Port of `con.resize_photo_sticker`: fit a photo into 512×512 for a
/// sticker. (Python's guard is `if (width and height) < 512`, which in
/// Python evaluates to `height < 512` — preserved verbatim.)
pub fn resize_photo_sticker(img: &DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    if h < 512 {
        let (nw, nh) = if w > h {
            let scale = 512.0 / w as f64;
            (512, (h as f64 * scale).floor() as u32)
        } else {
            let scale = 512.0 / h as f64;
            ((w as f64 * scale).floor() as u32, 512)
        };
        img.resize_exact(nw.max(1), nh.max(1), image::imageops::FilterType::Lanczos3)
    } else {
        img.resize(512, 512, image::imageops::FilterType::Lanczos3)
    }
}

/// Extract the first frame of a video to an image (Python's
/// `cv2.VideoCapture` + `imwrite` path, via ffmpeg).
pub async fn video_first_frame(video: &Path, out: &Path) -> Result<()> {
    let status = AsyncCommand::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            &video.to_string_lossy(),
            "-vframes",
            "1",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await?;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg first-frame extraction failed (exit {status})"
        )));
    }
    Ok(())
}

/// ffprobe dimensions + bitrate for a video file (Python `ToolKit.metadata`).
async fn video_metadata(file: &Path) -> Result<(u32, u32, String)> {
    let out = AsyncCommand::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-select_streams",
            "v:0",
            &file.to_string_lossy(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let stream = v
        .pointer("/streams/0")
        .ok_or_else(|| Error::Other("ffprobe found no video stream".to_string()))?;
    let w = stream.get("width").and_then(|x| x.as_u64()).unwrap_or(512) as u32;
    let h = stream.get("height").and_then(|x| x.as_u64()).unwrap_or(512) as u32;
    let bitrate = stream
        .get("bit_rate")
        .and_then(|x| x.as_str())
        .unwrap_or("256k")
        .to_string();
    Ok((w, h, bitrate))
}

/// Port of `con.create_webm`: transcode to a 512×512 VP9 WebM ≤ 3 s.
pub async fn create_webm(file: &Path, name: &str) -> Result<PathBuf> {
    let (w, h, bitrate) = video_metadata(file).await?;
    let (mut h, mut w) = (h, w);
    if h == w && h != 512 {
        h = 512;
        w = 512;
    }
    if h != 512 || w != 512 {
        if h > w {
            h = 512;
            w = 0; // ffmpeg `-1` keeps aspect; expressed as 0 here, fixed below
        }
        if w > h {
            h = 0;
            w = 512;
        }
    }
    // ffmpeg scale uses -1 (not 0) for "keep aspect".
    let scale = format!(
        "scale={}:{},fps=30",
        if w == 0 {
            "-1".to_string()
        } else {
            w.to_string()
        },
        if h == 0 {
            "-1".to_string()
        } else {
            h.to_string()
        }
    );
    let out = PathBuf::from(format!("{name}.webm"));
    let status = AsyncCommand::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            &file.to_string_lossy(),
            "-preset",
            "fast",
            "-an",
            "-to",
            "00:00:03",
            "-crf",
            "30",
            "-bufsize",
            "256k",
            "-b:v",
            &bitrate,
            "-vf",
            &scale,
            "-c:v",
            "libvpx-vp9",
        ])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await?;
    if !status.success() {
        return Err(Error::Other(format!(
            "ffmpeg webm conversion failed (exit {status})"
        )));
    }
    Ok(out)
}

/// Run `lottie_convert.py` (Python `con.animated_sticker` / `TgConverter`).
pub async fn lottie_convert(input: &Path, output: &Path) -> Result<PathBuf> {
    let mut args: Vec<String> = Vec::new();
    if output.extension().and_then(|e| e.to_str()) == Some("webp") {
        args.extend([
            "--webp-quality".into(),
            "100".into(),
            "--webp-skip-frames".into(),
            "100".into(),
        ]);
    }
    args.push(input.to_string_lossy().into_owned());
    args.push(output.to_string_lossy().into_owned());
    let status = AsyncCommand::new("lottie_convert.py")
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .await;
    match status {
        Ok(s) if s.success() => Ok(output.to_path_buf()),
        Ok(s) => Err(Error::Other(format!("lottie_convert.py failed (exit {s})"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Other(
            "lottie_convert.py is not installed on PATH (Python: stickertools.py \
             destroy/tiny via con.animated_sticker)"
                .to_string(),
        )),
        Err(e) => Err(Error::Other(format!("lottie_convert.py spawn failed: {e}"))),
    }
}

/// Python `destroy`'s JSON mangling: speed up every Lottie keyframe value.
pub fn destroy_mangle_json(jsn: &str) -> String {
    jsn.replace("[100]", "[200]")
        .replace("[10]", "[40]")
        .replace("[-1]", "[-10]")
        .replace("[0]", "[15]")
        .replace("[1]", "[20]")
        .replace("[2]", "[17]")
        .replace("[3]", "[40]")
        .replace("[4]", "[37]")
        .replace("[5]", "[60]")
        .replace("[6]", "[70]")
        .replace("[7]", "[40]")
        .replace("[8]", "[37]")
        .replace("[9]", "[110]")
}

/// Port of `.round`'s circular-mask math (PIL + numpy): circular alpha
/// mask over the image, saved as WebP.
pub fn round_sticker_image(input: &Path, output: &Path) -> Result<()> {
    let img = image::open(input)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let mut out = RgbaImage::new(w, h);
    // PIL `draw.pieslice([0, 0, h, w], 0, 360, fill=255)` — note the
    // width/height swap in the Python source; the bounding box is square
    // only when w == h, else it is the (h, w) ellipse. Preserve it.
    let (ew, eh) = (h as f64, w as f64);
    for (x, y, p) in img.enumerate_pixels() {
        let dx = (x as f64 + 0.5 - ew / 2.0) / (ew / 2.0);
        let dy = (y as f64 + 0.5 - eh / 2.0) / (eh / 2.0);
        let a = if dx * dx + dy * dy <= 1.0 { 255u8 } else { 0u8 };
        out.put_pixel(x, y, Rgba([p[0], p[1], p[2], a]));
    }
    out.save(output)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(())
}

/// `.tiny` size math: the resized thumbnail size for a source of (z, d).
/// (Python's formula can go ≤ 0 for extreme aspect ratios and then crash
/// inside PIL's resize; here it is clamped to ≥ 1 instead.)
pub fn tiny_thumb_size(z: u32, d: u32) -> (u32, u32) {
    if z == d {
        return (200, 200);
    }
    let t = (z + d) as f64;
    let a = z as f64 / t;
    let b = d as f64 / t;
    let aa = a * 100.0 - 50.0;
    let bb = b * 100.0 - 50.0;
    (
        ((200.0 + 5.0 * aa) as i64).max(1) as u32,
        ((200.0 + 5.0 * bb) as i64).max(1) as u32,
    )
}

/// `.tiny` for a static image: resize per `tiny_thumb_size`, paste onto
/// `resources/extras/ultroid_blank.png` at (150, 0), save as WebP.
pub fn tiny_static_image(input: &Path, output: &Path) -> Result<()> {
    let blank = PathBuf::from("resources/extras/ultroid_blank.png");
    if !blank.exists() {
        return Err(Error::Other(
            "resources/extras/ultroid_blank.png not found (Python: stickertools.py:tiny)"
                .to_string(),
        ));
    }
    let mut base = image::open(&blank)
        .map_err(|e| Error::Other(format!("image open: {e}")))?
        .to_rgba8();
    let im = image::open(input).map_err(|e| Error::Other(format!("image open: {e}")))?;
    let (z, d) = im.dimensions();
    let (tw, th) = tiny_thumb_size(z, d);
    let thumb = im
        .resize_exact(tw.max(1), th.max(1), image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    image::imageops::overlay(&mut base, &thumb, 150, 0);
    base.save(output)
        .map_err(|e| Error::Other(format!("image save: {e}")))?;
    Ok(())
}

/// `.tiny` for a `.tgs`: lottie → JSON, replace `512` with `2000`, back to `.tgs`.
pub async fn tiny_tgs(input: &Path) -> Result<PathBuf> {
    let json = PathBuf::from("json.json");
    lottie_convert(input, &json).await?;
    let jsn = tokio::fs::read_to_string(&json).await?;
    let jsn = jsn.replace("512", "2000");
    tokio::fs::write(&json, jsn).await?;
    let out = PathBuf::from("ult.tgs");
    let res = lottie_convert(&json, &out).await;
    let _ = tokio::fs::remove_file(&json).await;
    res
}

// ── emoji-pack parsing (pure) ─────────────────────────────────────────────

static EMOJISET_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:https?://)?(?:t(?:elegram)?\.me)/add(?:emoji|stickers)/([A-Za-z0-9_]+)")
        .unwrap()
});
static SHORTNAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]+$").unwrap());

/// Pull a sticker/emoji-set shortname out of a link or bare shortname
/// (Python `_parse_stickerset_shortname`).
pub fn parse_stickerset_shortname(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(m) = EMOJISET_LINK_RE.captures(raw) {
        return Some(m[1].to_string());
    }
    let raw = raw.trim_start_matches('@');
    if SHORTNAME_RE.is_match(raw) {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Render an emoji-pack listing (Python `emoji_set_list`'s message body).
/// `docs` is `(index, alt_glyph, document_id)`; at most 100 shown.
pub fn format_emoji_list(title: &str, short_name: &str, docs: &[(String, i64)]) -> String {
    const LIMIT: usize = 100;
    let mut lines = vec![format!(
        "**{title}** (`{short_name}`) — {} custom emoji",
        docs.len()
    )];
    for (idx, (alt, doc_id)) in docs.iter().take(LIMIT).enumerate() {
        lines.push(format!("`{:>2}.` {alt}  doc id: `{doc_id}`", idx + 1));
    }
    if docs.len() > LIMIT {
        lines.push(format!("`… +{} more (list truncated)`", docs.len() - LIMIT));
    }
    lines.push(format!(
        "\nUse `.emojireply {short_name}:<index>` or `.emojireply <doc_id>` \
         to reply with one (needs Premium on this account to render)."
    ));
    lines.join("\n")
}

// ── commands ──────────────────────────────────────────────────────────────

async fn pack_kang(ctx: &Ctx) -> Result<()> {
    let cmdtext = ctx.args.trim().to_string();
    let local = !cmdtext.is_empty() && Path::new(&cmdtext).is_dir();
    if !local && ctx.reply_to_id.is_none() {
        ctx.reply("`Please reply to an sticker.`").await?;
        return Ok(());
    }
    let _processing = ctx.reply(&ctx.get_str("com_1")).await?;
    let _packname = if !cmdtext.is_empty() {
        cmdtext.clone()
    } else {
        format!("Pack By {}", ctx.sender_id)
    };
    if local {
        // TODO(responder-op): upload each .tgs/.webm in the dir via the
        // assistant + UploadMediaRequest, then CreateStickerSetRequest
        // (Python: stickertools.py:pack_kangish, local branch)
        return Err(Error::Other(
            "uploading local sticker files + creating the sticker set requires \
             a binary Responder op (Python: stickertools.py:pack_kangish)"
                .to_string(),
        ));
    }
    // TODO(responder-op): messages.GetStickerSetRequest by the replied
    // sticker's set id/access_hash, then stickers.CreateStickerSetRequest
    // via the assistant (Python: stickertools.py:pack_kangish)
    Err(Error::Other(
        "reading the replied sticker's set + creating the sticker set requires \
         a binary Responder op (Python: stickertools.py:pack_kangish)"
            .to_string(),
    ))
}

async fn kang(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply(&ctx.get_str("sts_6")).await?;
        return Ok(());
    }
    let _processing = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): download the replied message's media + read its
    // type/attributes (photo / image / video ≤10 s / .tgs / text → quotly),
    // then run the @Stickers bot conversation (Python: stickertools.py:hehe)
    Err(Error::Other(
        "downloading replied media + the @Stickers conversation requires a \
         binary Responder op (Python: stickertools.py:hehe)"
            .to_string(),
    ))
}

async fn round_cmd(ctx: &Ctx) -> Result<()> {
    let reply_id = match ctx.reply_to_id {
        Some(id) => id,
        None => {
            ctx.reply(&ctx.get_str("sts_10")).await?;
            return Ok(());
        }
    };
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    // Download replied media, then apply the circular mask
    // (Python: stickertools.py:ultdround — `download_media` + `con.convert`).
    let dest = std::env::temp_dir().join(format!("ultroid-round-{}", uuid::Uuid::new_v4()));
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    let out = std::env::temp_dir().join(format!("ultroid-round-{}.webp", uuid::Uuid::new_v4()));
    let res = round_sticker_image(&path, &out);
    let _ = tokio::fs::remove_file(&path).await;
    res?;
    ctx.send_file(&out, "").await?;
    let _ = ctx.delete(processing).await;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

async fn destroy_cmd(ctx: &Ctx) -> Result<()> {
    let reply_id = match ctx.reply_to_id {
        Some(id) => id,
        None => {
            ctx.reply(&ctx.get_str("sts_2")).await?;
            return Ok(());
        }
    };
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    // Download the replied animated sticker as .tgs, mangle its Lottie
    // JSON, convert back, send it (Python: stickertools.py:ultdestroy).
    // The "animated" check is implicit: `lottie_convert` fails honestly
    // if the download is not a Lottie file.
    let tag = uuid::Uuid::new_v4();
    let input = std::env::temp_dir().join(format!("ultroid-destroy-{tag}.tgs"));
    ctx.responder
        .download_media(ctx.chat_id, reply_id as i32, &input)
        .await?;
    let json_path = std::env::temp_dir().join(format!("ultroid-destroy-{tag}.json"));
    let out = std::env::temp_dir().join(format!("ultroid-destroyed-{tag}.tgs"));
    let res = async {
        lottie_convert(&input, &json_path).await?;
        let jsn = tokio::fs::read_to_string(&json_path).await?;
        tokio::fs::write(&json_path, destroy_mangle_json(&jsn)).await?;
        lottie_convert(&json_path, &out).await
    }
    .await;
    let _ = tokio::fs::remove_file(&input).await;
    let _ = tokio::fs::remove_file(&json_path).await;
    let out = res?;
    ctx.send_file(&out, "").await?;
    let _ = ctx.delete(processing).await;
    let _ = tokio::fs::remove_file(&out).await;
    Ok(())
}

/// Is this file a gzipped Lottie (`.tgs`) file? (gzip magic bytes.)
fn is_gzip(path: &Path) -> bool {
    std::fs::read(path)
        .map(|b| b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b)
        .unwrap_or(false)
}

async fn tiny_cmd(ctx: &Ctx) -> Result<()> {
    let reply_id = match ctx.reply_to_id {
        Some(id) => id,
        None => {
            ctx.reply(&ctx.get_str("sts_10")).await?;
            return Ok(());
        }
    };
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    // Download replied media (Python: stickertools.py:ultiny —
    // `reply.download_media()`), then branch on what it actually is,
    // as Python branches on the downloaded file's extension:
    // `.tgs` -> Lottie path, video -> first frame, else static image.
    let dest = std::env::temp_dir().join(format!("ultroid-tiny-{}", uuid::Uuid::new_v4()));
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    let out_path: PathBuf = if is_gzip(&path) {
        tiny_tgs(&path).await?
    } else if image::open(&path).is_ok() {
        let out = std::env::temp_dir().join(format!("ultroid-tiny-{}.webp", uuid::Uuid::new_v4()));
        tiny_static_image(&path, &out)?;
        out
    } else {
        let frame =
            std::env::temp_dir().join(format!("ultroid-tiny-frame-{}.png", uuid::Uuid::new_v4()));
        video_first_frame(&path, &frame).await?;
        let out = std::env::temp_dir().join(format!("ultroid-tiny-{}.webp", uuid::Uuid::new_v4()));
        let res = tiny_static_image(&frame, &out);
        let _ = tokio::fs::remove_file(&frame).await;
        res?;
        out
    };
    let _ = tokio::fs::remove_file(&path).await;
    ctx.send_file(&out_path, "").await?;
    let _ = ctx.delete(processing).await;
    // `tiny_tgs` returns the fixed `ult.tgs` in the working directory;
    // the static/video branches return a temp path. Either way the
    // sent file is removed after sending.
    let _ = tokio::fs::remove_file(&out_path).await;
    Ok(())
}

async fn emoji_set_list(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        ctx.reply("`Usage: .emojiset <pack-link-or-shortname>`")
            .await?;
        return Ok(());
    }
    let Some(short_name) = parse_stickerset_shortname(raw) else {
        ctx.reply(
            "`Couldn't read a pack shortname out of that. Try a link like \
             t.me/addemoji/PackName or just the shortname.`",
        )
        .await?;
        return Ok(());
    };
    let processing = ctx.reply(&ctx.get_str("com_1")).await?;
    // messages.GetStickerSetRequest with InputStickerSetShortName
    // (Python: stickertools.py:_get_emoji_stickerset).
    let set = ctx.responder.sticker_set(&short_name).await?;
    if !set.emojis {
        ctx.reply(&format!(
            "`\"{}\" ({short_name}) is a regular sticker pack, not a custom-emoji pack — try .pamck/.kang for stickers instead.`",
            set.title
        ))
        .await?;
        return Ok(());
    }
    if set.stickers.is_empty() {
        ctx.reply(&format!(
            "`\"{}\" ({short_name}) has no emoji in it.`",
            set.title
        ))
        .await?;
        return Ok(());
    }
    let docs: Vec<(String, i64)> = set
        .stickers
        .iter()
        .map(|s| (s.emoji.clone(), s.document_id))
        .collect();
    let text = format_emoji_list(&set.title, &set.short_name, &docs);
    ctx.reply(&text).await?;
    let _ = ctx.delete(processing).await;
    Ok(())
}

async fn emoji_reply(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        ctx.reply(
            "`Usage: .emojireply <shortname:index or doc_id> [extra text]` \
             (see .emojiset to list a pack's indices/ids)",
        )
        .await?;
        return Ok(());
    }
    let mut bits = raw.splitn(2, char::is_whitespace);
    let reference = bits.next().unwrap_or("");
    let _extra_text = bits.next().unwrap_or("").trim();
    if reference.contains(':') {
        let (short, _, idx_s) = {
            let mut p = reference.splitn(2, ':');
            (p.next().unwrap_or(""), ":", p.next().unwrap_or(""))
        };
        let _ = short;
        if parse_stickerset_shortname(short).is_none()
            || idx_s.is_empty()
            || !idx_s.chars().all(|c| c.is_ascii_digit())
        {
            ctx.reply("`Bad reference — expected <shortname>:<index>, e.g. MyPack:3`")
                .await?;
            return Ok(());
        }
    } else if !reference.chars().all(|c| c.is_ascii_digit()) || reference.is_empty() {
        ctx.reply("`Usage: .emojireply <shortname:index or doc_id> [extra text]`")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): messages.GetStickerSetRequest (shortname:index) or
    // messages.GetCustomEmojiDocumentsRequest (doc id), Premium probe via
    // get_me, then send with the custom-emoji entity (Python:
    // stickertools.py:emoji_reply)
    Err(Error::Other(
        "custom-emoji lookup + send requires a binary Responder op (Python: stickertools.py:emoji_reply)"
            .to_string(),
    ))
}
