//! Small native RichMessage helpers for assistant menu pages.
//!
//! Port of `assistant/rich_pages.py`. A page is an `<h1>` title, an
//! optional description, a bordered table, a divider, and the shared
//! lower-case page index (`page_footer`). Content is built as HTML
//! and carried in a core `RichDoc`; sending/editing goes through
//! `Ctx::send_rich_or_plain` / `Ctx::edit` (the `Responder` contract
//! has no button channel, so `buttons` arguments are accepted as
//! data — see `crate::cbs` — and returned to the caller where the
//! Python code edits a message with buttons attached).
//!
//! Media note: Python attaches a `RichFile("assistant-page-media",
//! source, kind)` alongside the `<figure>` markup. Core `RichDoc`
//! can only bind media by numeric Telegram id / access-hash /
//! file-reference, which a saved file-id string does not carry, so
//! the figure markup is rendered exactly as Python emits it and the
//! file binding itself is not representable through this contract;
//! the binary crate must resolve the saved `STARTMEDIA` file id into
//! a `RichMedia` binding before sending if media is required.

use ultroid_core::Ctx;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::InlineButton;

/// The shared lower-case page index and item count.
pub fn page_footer(page: i64, pages: i64, total: i64) -> String {
    let pages = pages.max(1);
    let page = page.clamp(1, pages);
    format!("page {page} of {pages} • total {}", total.max(0))
}

/// Build a large-heading, divider, table, divider, indexed
/// RichMessage page (Python `rich_page`).
#[allow(clippy::too_many_arguments)]
pub fn rich_page(
    title: &str,
    rows: &[Vec<String>],
    description: Option<&str>,
    columns: &[&str],
    page: i64,
    pages: i64,
    total: Option<usize>,
    media: Option<(&str, &str, Option<&str>)>,
) -> Result<RichDoc> {
    let total_items = total.unwrap_or(rows.len()) as i64;
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("<h1>{}</h1>", escape_html(title)));
    if let Some(d) = description {
        if !d.is_empty() {
            parts.push(format!("<p>{}</p>", escape_html(d)));
        }
    }
    if let Some((kind, _source, caption)) = media {
        if kind != "photo" && kind != "document" {
            return Err(Error::Other(
                "media kind must be 'photo' or 'document'".to_string(),
            ));
        }
        let file_id = "assistant-page-media";
        if kind == "photo" {
            let alt = escape_html(caption.unwrap_or_default());
            parts.push(format!(
                "<figure><img src=\"{file_id}\" alt=\"{alt}\"/></figure>"
            ));
        } else {
            parts.push(format!(
                "<figure><tg-document src=\"{file_id}\"></tg-document></figure>"
            ));
        }
        if let Some(c) = caption {
            if !c.is_empty() {
                parts.push(format!("<p>{}</p>", escape_html(c)));
            }
        }
    }
    parts.push("<hr>".to_string());
    parts.push("<table class=\"bordered compact\">".to_string());
    let cols: Vec<String> = columns.iter().map(|c| escape_html(c)).collect();
    parts.push(format!(
        "<tr>{}</tr>",
        cols.iter()
            .map(|c| format!("<th>{c}</th>"))
            .collect::<Vec<_>>()
            .join("")
    ));
    for row in rows {
        let mut cells: Vec<String> = row.iter().map(|c| escape_html(c)).collect();
        while cells.len() < cols.len() {
            cells.push(String::new());
        }
        cells.truncate(cols.len());
        parts.push(format!(
            "<tr>{}</tr>",
            cells
                .iter()
                .map(|c| format!("<td>{c}</td>"))
                .collect::<Vec<_>>()
                .join("")
        ));
    }
    parts.push("</table><hr>".to_string());
    parts.push(format!(
        "<p><i>{}</i></p>",
        escape_html(&page_footer(page, pages, total_items))
    ));
    RichDoc::html(parts.join(""))
}

/// Wrap already-sanitized native HTML in a standard indexed page
/// (Python `body_page`).
pub fn body_page(
    title: &str,
    body_html: &str,
    page: i64,
    pages: i64,
    total: i64,
    media: Option<(&str, &str, Option<&str>)>,
) -> Result<RichDoc> {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("<h1>{}</h1><hr>{body_html}", escape_html(title)));
    if let Some((kind, _source, caption)) = media {
        if kind != "photo" && kind != "document" {
            return Err(Error::Other(
                "media kind must be 'photo' or 'document'".to_string(),
            ));
        }
        let file_id = "assistant-page-media";
        if kind == "photo" {
            let alt = escape_html(caption.unwrap_or_default());
            parts.push(format!(
                "<figure><img src=\"{file_id}\" alt=\"{alt}\"/></figure>"
            ));
        } else {
            parts.push(format!(
                "<figure><tg-document src=\"{file_id}\"></tg-document></figure>"
            ));
        }
        if let Some(c) = caption {
            if !c.is_empty() {
                parts.push(format!("<p>{}</p>", escape_html(c)));
            }
        }
    }
    parts.push("<hr>".to_string());
    parts.push(format!(
        "<p><i>{}</i></p>",
        escape_html(&page_footer(page, pages, total))
    ));
    RichDoc::html(parts.join(""))
}

/// Return the actual callback message target (Python
/// `callback_edit_target`): for an inline callback message the query
/// id would be the target, but `Ctx` carries only the chat/message
/// ids of a normal message, so this returns `(chat_id, message_id)`.
/// The binary crate must override this for inline-message callbacks.
pub fn callback_edit_target(ctx: &Ctx) -> (i64, i64) {
    (ctx.chat_id, ctx.message_id)
}

/// Edit a callback message through the rich path (Python
/// `edit_callback_page`): edits `ctx.message_id` in `ctx.chat_id`.
/// `Ctx` has no rich-edit method, so the page HTML is passed to
/// `Ctx::edit` (the binary applies the same HTML parse mode as
/// `reply_html`); the keyboard is returned to the caller — see the
/// module docs — because `Ctx::edit` takes text only.
pub async fn edit_callback_page(
    ctx: &Ctx,
    content: &RichDoc,
    _buttons: Vec<Vec<InlineButton>>,
) -> Result<()> {
    let (_peer, message) = callback_edit_target(ctx);
    let text = content
        .html
        .clone()
        .or_else(|| content.markdown.clone())
        .unwrap_or_else(|| content.plain_text());
    ctx.edit(message, &text).await
}
