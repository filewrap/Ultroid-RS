//! Sticker-set & premium (custom) emoji management — port of
//! `plugins/emojis.py` (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.emojipacks` — list installed sticker / custom-emoji packs.
//! - `.emojiview <pack-link-or-shortname>` — show the stickers /
//!   custom emoji of a set.
//! - `.emojisave [icon_name]` (reply to a sticker / premium emoji) —
//!   download the replied document, send it back with a caption, and
//!   write its metadata to a .txt file.
//! - `.emojiindex [pack_title]` — upload every saved emoji/icon
//!   document to a **live** bot custom-emoji set.
//!
//! The pack-list/fetch calls are wired to the `Responder` sticker ops
//! (`my_sticker_sets`, `sticker_set`); the replied document's
//! attributes are not exposed through `Ctx`, so `.emojisave` still
//! returns an honest TODO(responder-op) error, and uploading a new bot
//! custom-emoji set (`stickers.CreateStickerSet`) has no trait op, so
//! `.emojiindex` does too. The saved-emoji DB (`EMOJI_SAVED` /
//! `EMOJI_DOC_IDS`) is real JSON in udB. The inline frontier
//! (`@{assistant} emoji …`) and the pack-page callback are
//! assistant-crate surface (`in_pattern` / `@callback`), not user-plugin
//! commands, so they are not registered here.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "emojipacks",
            "emojipacks$",
            "List your installed sticker / custom-emoji packs.",
        ),
        |ctx| Box::pin(emoji_packs_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "emojiview",
            "emojiview( (.*)|$)",
            "Show the stickers / custom emoji of a set: .emojiview <pack-link-or-shortname>.",
        ),
        |ctx| Box::pin(emoji_view_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "emojisave",
            "emojisave( (.*)|$)",
            "Save a sticker/premium emoji: .emojisave [icon_name] (reply to a sticker / premium emoji).",
        ),
        |ctx| Box::pin(emoji_save_cmd(ctx)),
    );
    reg_add(
        registry,
        def(
            "emojiindex",
            "emojiindex( (.*)|$)",
            "Upload saved emoji/icon docs to a live bot custom-emoji set: .emojiindex [pack_title].",
        ),
        |ctx| Box::pin(emoji_index_cmd(ctx)),
    );
}

const EMOJI_SAVED_KEY: &str = "EMOJI_SAVED";
const EMOJI_DOC_IDS_KEY: &str = "EMOJI_DOC_IDS";
const PACK_PAGE_SIZE: usize = 20;

// ── data ──────────────────────────────────────────────────────────────────

/// One saved sticker / custom-emoji entry (Python `EMOJI_SAVED` list item).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiMeta {
    pub name: String,
    pub emoji: String,
    pub document_id: Option<i64>,
    pub file_id: String,
    pub pack: String,
    pub author: String,
    pub is_custom_emoji: bool,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_title: Option<String>,
}

/// A pack entry in the in-memory page cache (Python `_PACK_CACHE`).
#[derive(Debug, Clone)]
pub struct PackEntry {
    pub short: String,
    pub title: String,
    pub is_emoji: bool,
    /// (alt glyph, document id) per item, in pack order.
    pub docs: Option<Vec<(String, i64)>>,
}

static PACK_CACHE: OnceLock<Mutex<HashMap<u64, PackEntry>>> = OnceLock::new();
static PACK_SEQ: OnceLock<Mutex<u64>> = OnceLock::new();

fn pack_cache() -> &'static Mutex<HashMap<u64, PackEntry>> {
    PACK_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[allow(dead_code)]
fn next_pack_idx() -> u64 {
    let seq = PACK_SEQ.get_or_init(|| Mutex::new(0));
    let mut n = seq.lock().unwrap();
    let idx = *n;
    *n += 1;
    idx
}

// ── pure helpers (ported verbatim) ────────────────────────────────────────

/// UTF-16 code-unit length (entity offsets/lengths are UTF-16 based).
#[allow(dead_code)]
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn stickerset_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:https?://)?(?:t(?:elegram)?\.me)/add(?:emoji|stickers)/([A-Za-z0-9_]+)")
            .unwrap()
    })
}

fn shortname_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_]+$").unwrap())
}

/// Pull a sticker/emoji-set shortname out of a link or bare shortname.
fn parse_shortname(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(m) = stickerset_link_re().captures(raw) {
        return Some(m[1].to_string());
    }
    let raw = raw.trim_start_matches('@');
    if shortname_re().is_match(raw) {
        Some(raw.to_string())
    } else {
        None
    }
}

/// File extension hinted by a document MIME type.
#[allow(dead_code)]
fn doc_ext(mime: &str) -> &'static str {
    let mime = mime.to_lowercase();
    if mime.contains("webp") {
        ".webp"
    } else if mime.contains("tgs") || mime.contains("lottie") {
        ".tgs"
    } else if mime.contains("webm") {
        ".webm"
    } else if mime.contains("png") {
        ".png"
    } else if mime.contains("jpeg") || mime.contains("jpg") {
        ".jpg"
    } else {
        ""
    }
}

#[allow(dead_code)]
fn meta_text(m: &EmojiMeta) -> String {
    format!(
        "emoji: {}\nname: {}\ndocument_id: {}\nfile_id: {}\npack: {}\nauthor: {}\nis_custom_emoji: {}\nfile: {}\n",
        m.emoji,
        m.name,
        m.document_id.map(|d| d.to_string()).unwrap_or_default(),
        m.file_id,
        m.pack,
        m.author,
        m.is_custom_emoji,
        m.file,
    )
}

#[allow(dead_code)]
fn meta_caption(m: &EmojiMeta, txt_path: &str) -> String {
    let kind = if m.is_custom_emoji {
        "custom emoji"
    } else {
        "sticker"
    };
    format!(
        "{} saved {kind}\nname: {}\ndocument_id: {}\nfile_id: {}\npack: {}\nauthor: {}\nmetadata file: {txt_path}",
        m.emoji,
        m.name,
        m.document_id.map(|d| d.to_string()).unwrap_or_default(),
        m.file_id,
        m.pack,
        m.author,
    )
}

/// Render one pack page (Python `_render_pack_page`). Returns
/// (text, has_prev, has_next).
#[allow(dead_code)]
fn render_pack_page(idx: u64, page: usize) -> (String, bool, bool) {
    let cache = pack_cache().lock().unwrap();
    let Some(info) = cache.get(&idx) else {
        return (
            "This pack view expired — run `.emojipacks` / `.emojiview` again.".to_string(),
            false,
            false,
        );
    };
    let docs = info.docs.clone().unwrap_or_default();
    let total = docs.len();
    let pages = ((total + PACK_PAGE_SIZE - 1) / PACK_PAGE_SIZE).max(1);
    let page = page.min(pages - 1);
    let start = page * PACK_PAGE_SIZE;
    let kind = if info.is_emoji {
        "custom-emoji pack"
    } else {
        "sticker pack"
    };
    let mut lines = vec![
        format!("{} ({}) — {kind}, {total} items", info.title, info.short),
        format!("page {}/{pages}", page + 1),
        String::new(),
    ];
    for (i, (alt, doc_id)) in docs.iter().skip(start).take(PACK_PAGE_SIZE).enumerate() {
        lines.push(format!("  {:>3}. {alt}  doc: {doc_id}", start + i + 1));
    }
    (lines.join("\n"), page > 0, page < pages - 1)
}

// ── commands ──────────────────────────────────────────────────────────────

async fn emoji_packs_cmd(ctx: &Ctx) -> Result<()> {
    // Python: `messages.GetMyStickersRequest(offset_id=0, limit=100)`.
    // The trait returns no per-set count (the server does not report
    // one here), so only title / shortname / kind are listed.
    let sets = ctx.responder.my_sticker_sets().await?;
    if sets.is_empty() {
        ctx.reply("`No sticker / custom-emoji packs installed on this account.`")
            .await?;
        return Ok(());
    }
    let mut lines = vec![format!(
        "**Sticker / custom-emoji packs** ({}):",
        sets.len()
    )];
    for set in &sets {
        let kind = if set.emojis {
            "custom emoji"
        } else {
            "stickers"
        };
        lines.push(format!(
            "• **{}** (`{}`) — {kind}",
            set.title, set.short_name
        ));
    }
    lines.push(String::new());
    lines.push("Use `.emojiview <pack-link-or-shortname>` to see a pack's contents.".to_string());
    ctx.reply(&lines.join("\n")).await?;
    Ok(())
}

async fn emoji_view_cmd(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        ctx.reply("**Usage:** `.emojiview <pack-link-or-shortname>`")
            .await?;
        return Ok(());
    }
    let Some(short) = parse_shortname(raw) else {
        ctx.reply(
            "Couldn't read a pack shortname. Try a `t.me/addemoji/Name` or `t.me/addstickers/Name` link, or the bare shortname.",
        )
        .await?;
        return Ok(());
    };
    // Python: `messages.GetStickerSetRequest(InputStickerSetShortName)`.
    // The fetched set is cached so the pack-page renderer (and its
    // callback) can page through it, exactly as in Python.
    let set = ctx.responder.sticker_set(&short).await?;
    let idx = next_pack_idx();
    pack_cache().lock().unwrap().insert(
        idx,
        PackEntry {
            short: set.short_name.clone(),
            title: set.title.clone(),
            is_emoji: set.emojis,
            docs: Some(
                set.stickers
                    .iter()
                    .map(|s| (s.emoji.clone(), s.document_id))
                    .collect(),
            ),
        },
    );
    let (text, _, _) = render_pack_page(idx, 0);
    ctx.reply(&text).await?;
    Ok(())
}

async fn emoji_save_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("**Usage:** reply to a sticker or premium emoji with `.emojisave [icon_name]`")
            .await?;
        return Ok(());
    }
    // Python reads the replied document's attributes (alt glyph,
    // stickerset, custom-emoji flag), downloads the file, writes the
    // metadata .txt, stores the entry in udB `EMOJI_SAVED`, and sends
    // the file + .txt back. The document attributes and the download
    // are not exposed through Ctx.
    // TODO(responder-op): read replied document attributes + download media
    // (Python: emojis.py:emoji_save_cmd).
    Err(Error::Other(
        "reading the replied sticker/emoji document + downloading it requires a binary Responder op (Python: emojis.py:emoji_save_cmd)".into(),
    ))
}

async fn emoji_index_cmd(ctx: &Ctx) -> Result<()> {
    let saved: Vec<EmojiMeta> = db_get_json::<Vec<EmojiMeta>>(&*ctx.db, EMOJI_SAVED_KEY)
        .await?
        .unwrap_or_default();
    if saved.is_empty() {
        ctx.reply(
            "No saved emoji/icon documents. Use `.emojisave` (reply to a sticker/premium emoji) first.",
        )
        .await?;
        return Ok(());
    }
    // Python uploads each saved file via the assistant bot
    // (`messages.UploadMedia` + `stickers.CreateStickerSetRequest` /
    // `AddStickerToSetRequest`), then persists the new doc ids to udB
    // `EMOJI_DOC_IDS` (add-only), writes back icon ids into
    // `custom/icons.py::ICON_DOCUMENT_IDS`, and posts the index to
    // LOG_CHANNEL. All of that needs the bot client.
    // TODO(responder-op): upload saved emoji docs to a bot custom-emoji set
    // (Python: emojis.py:_upload_emoji_set / emoji_index_cmd).
    Err(Error::Other(format!(
        "uploading {} saved emoji/icon document(s) to a bot custom-emoji set requires a binary Responder op (stickers.CreateStickerSetRequest) (Python: emojis.py:emoji_index_cmd)",
        saved.len()
    )))
}

/// Add-only merge of new doc ids into udB `EMOJI_DOC_IDS` — the real
/// persistence step of `.emojiindex`, kept for the Responder wiring pass.
#[allow(dead_code)]
async fn persist_doc_ids(ctx: &Ctx, new_ids: &HashMap<String, i64>) -> Result<()> {
    let mut existing: HashMap<String, i64> =
        db_get_json::<HashMap<String, i64>>(&*ctx.db, EMOJI_DOC_IDS_KEY)
            .await?
            .unwrap_or_default();
    for (n, d) in new_ids {
        existing.entry(n.clone()).or_insert(*d);
    }
    db_set_json(&*ctx.db, EMOJI_DOC_IDS_KEY, &existing).await
}
