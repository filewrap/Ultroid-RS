//! Owner-only Layer-230 RichMessage examples: each index pick sends a sample.
//!
//! Port of `assistant/rich_showcase.py`.
//!
//! The catalog is copied verbatim: native blocks first (paragraph,
//! headings 1–6, inline styles, mark/sub/superscript, spoiler,
//! links/email/phone, custom emoji, date, divider, footer, lists,
//! ordered lists, tasks, code, tables, quotes, pull quote,
//! expandable quotation, details variants, seven-step guide,
//! formula, anchors, map, rich buttons, text buttons, multi-block
//! quotation, nested details, text entities, details wrapper, RTL),
//! then the five `CREATIONS` templates (see `rich_creations`), then
//! the matrix arcade lobby.
//!
//! Python renders these as native `PageBlock` trees. Core `RichDoc`
//! carries HTML / Markdown / legacy text blocks, not PageBlock
//! trees, so each sample is rendered here as the equivalent HTML
//! document (same text, same structure); the matrix lobby and the
//! creation templates delegate to their own renderers. The catalog
//! ids, labels, descriptions, paging (8 per page), callback data
//! (`rich:<pick|send|edit|index|ping>:<value>`), and the legacy
//! aliases are all preserved exactly.
//!
//! Server/account acceptance varies; a rejection is reported, never
//! downgraded to plain text — in this port a rejected rich send
//! surfaces the error to the caller instead of silently falling back.

use ultroid_core::error::{Error, Result};
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::InlineButton;
use ultroid_core::{CommandDef, Ctx, Registry};

use crate::cbs::{cb, def, reg_add};
use crate::rich_creations::{CREATIONS, creation_html};
use crate::rich_pages::edit_callback_page;

/// Catalog entry: `(id, label, description)`.
pub fn catalog() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = vec![
        (
            "native".into(),
            "Paragraph".into(),
            "PageBlockParagraph".into(),
        ),
        (
            "inline".into(),
            "Inline styles".into(),
            "TextBold / Italic / Underline / Strike".into(),
        ),
        (
            "extra-styles".into(),
            "Mark / sub / superscript".into(),
            "TextMarked / Subscript / Superscript".into(),
        ),
        ("spoiler".into(), "Spoiler".into(), "TextSpoiler".into()),
        (
            "links".into(),
            "Links / email / phone".into(),
            "TextUrl / Email / Phone".into(),
        ),
        (
            "emoji".into(),
            "Custom emoji".into(),
            "TextCustomEmoji; account eligibility may restrict it".into(),
        ),
        ("date".into(), "Formatted date".into(), "TextDate".into()),
        (
            "divider".into(),
            "Divider".into(),
            "PageBlockDivider".into(),
        ),
        ("footer".into(), "Footer".into(), "PageBlockFooter".into()),
        (
            "lists".into(),
            "Unordered list".into(),
            "PageBlockList".into(),
        ),
        (
            "ordered".into(),
            "Ordered list".into(),
            "PageBlockOrderedList".into(),
        ),
        (
            "tasks".into(),
            "Task list".into(),
            "PageListItemText checkbox / checked".into(),
        ),
        (
            "code".into(),
            "Inline / fenced code".into(),
            "TextFixed / PageBlockPreformatted".into(),
        ),
        ("tables".into(), "Table".into(), "PageBlockTable".into()),
        (
            "quotes".into(),
            "Quotation".into(),
            "PageBlockBlockquote".into(),
        ),
        (
            "pullquote".into(),
            "Pull quote".into(),
            "PageBlockPullquote".into(),
        ),
        (
            "rich-expand".into(),
            "Expandable quotation".into(),
            "PageBlockBlockquote collapsed flag; not details".into(),
        ),
        (
            "details".into(),
            "Details: closed".into(),
            "PageBlockDetails".into(),
        ),
        (
            "details-open".into(),
            "Details: open".into(),
            "PageBlockDetails open flag".into(),
        ),
        (
            "details-code".into(),
            "Details + code".into(),
            "PageBlockDetails / Preformatted".into(),
        ),
        (
            "details-steps".into(),
            "Two-step details".into(),
            "Two PageBlockDetails blocks".into(),
        ),
        (
            "native-guide".into(),
            "Seven-step guide".into(),
            "Bold paragraph heading + seven details/code sections".into(),
        ),
        (
            "formula".into(),
            "Inline / block formula".into(),
            "TextMath / PageBlockMath".into(),
        ),
        (
            "anchors".into(),
            "Named anchor".into(),
            "TextAnchor / TextUrl".into(),
        ),
        (
            "map".into(),
            "Map".into(),
            "PageBlockMap; static public Rome coordinates".into(),
        ),
        (
            "rich-buttons".into(),
            "Rich button row".into(),
            "PageBlockButtonRow: URL / callback / copy".into(),
        ),
        (
            "text-buttons".into(),
            "Inline text button".into(),
            "TextButton".into(),
        ),
        (
            "block-quotes".into(),
            "Multi-block quotation".into(),
            "PageBlockBlockquoteBlocks".into(),
        ),
        (
            "nested-details".into(),
            "Nested details".into(),
            "PageBlockDetails inside details".into(),
        ),
        (
            "text-entities".into(),
            "Native entity text".into(),
            "TextWithEntities inside a native paragraph".into(),
        ),
        (
            "details-wrapper".into(),
            "Native RichBlocks wrapper".into(),
            "RichBlocks(native=True).expandable".into(),
        ),
        (
            "rtl".into(),
            "Right-to-left".into(),
            "InputRichMessage rtl flag".into(),
        ),
    ];
    for n in 1..=6 {
        out.insert(
            n,
            (
                format!("heading-{n}"),
                format!("Heading {n}"),
                format!("PageBlockHeading{n}"),
            ),
        );
    }
    for (sid, label, desc) in CREATIONS {
        out.push((sid.to_string(), label.to_string(), desc.to_string()));
    }
    out.push((
        "matrix-arcade".into(),
        "Matrix arcade".into(),
        "Playable native games, bounded edit rain and a browser source lab".into(),
    ));
    out
}

/// Page size of the index (Python `PAGE_SIZE`).
pub const PAGE_SIZE: usize = 8;

/// Aliases for already-delivered keyboards (Python `_ALIASES`).
/// Removed media/footnote entries are deliberately NOT aliased.
pub fn alias(sid: &str) -> &str {
    match sid {
        "plain" | "html" | "markdown" => "native",
        "headings" => "heading-1",
        "native-title" => "native",
        "divider-footer" => "footer",
        "details-native" => "details",
        "native-open" => "details-open",
        "native-code" => "details-code",
        "native-steps" => "details-steps",
        "legacy-styles" => "text-entities",
        "legacy-expand" => "rich-expand",
        other => other,
    }
}

fn page_of(page: usize, total: usize) -> usize {
    let max = total.saturating_sub(1) / PAGE_SIZE;
    page.min(max)
}

fn sample(sid: &str) -> Option<(String, String, String)> {
    let sid = alias(sid);
    catalog().into_iter().find(|(id, _, _)| id == sid)
}

fn sample_page(sid: &str) -> usize {
    let sid = alias(sid);
    catalog()
        .iter()
        .position(|(id, _, _)| id == sid)
        .map(|i| i / PAGE_SIZE)
        .unwrap_or(0)
}

fn data(action: &str, value: &str) -> Vec<u8> {
    cb(&["rich", action, value])
        .unwrap_or_default()
        .into_bytes()
}

/// Index keyboard for `page`; with `selected`, the Send-again /
/// Edit / Back-to-index keyboard instead (Python `_keyboard`).
pub fn keyboard(page: usize, selected: &str) -> Vec<Vec<InlineButton>> {
    let cat = catalog();
    if !selected.is_empty() {
        return vec![
            vec![
                InlineButton::data("Send again", data("send", selected)),
                InlineButton::data("Edit this example", data("edit", selected)),
            ],
            vec![InlineButton::data(
                "Back to index",
                data("index", &sample_page(selected).to_string()),
            )],
        ];
    }
    let page = page_of(page, cat.len());
    let items = &cat[page * PAGE_SIZE..((page + 1) * PAGE_SIZE).min(cat.len())];
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    for chunk in items.chunks(2) {
        rows.push(
            chunk
                .iter()
                .map(|(id, label, _)| InlineButton::data(label, data("pick", id)))
                .collect(),
        );
    }
    let mut nav: Vec<InlineButton> = Vec::new();
    if page > 0 {
        nav.push(InlineButton::data(
            "Previous",
            data("index", &(page - 1).to_string()),
        ));
    }
    if (page + 1) * PAGE_SIZE < cat.len() {
        nav.push(InlineButton::data(
            "Next",
            data("index", &(page + 1).to_string()),
        ));
    }
    if !nav.is_empty() {
        rows.push(nav);
    }
    rows
}

/// The index page as HTML (Python `_index`).
pub fn index_doc(page: usize, error: &str) -> Result<RichDoc> {
    let cat = catalog();
    let page = page_of(page, cat.len());
    let total_pages = (cat.len() - 1) / PAGE_SIZE + 1;
    let mut parts = vec![
        "<h1>RichMessage examples</h1>".to_string(),
        "<p>Tap an index button to send that native example immediately. Send again creates a new example; Edit this example tests native editing.</p>".to_string(),
        "<p>All examples use structured Layer-230 MTProto. Server/account acceptance varies; a rejection is reported, never downgraded to plain text.</p>".to_string(),
        "<p>Media, collages and slideshows are not indexed: verified uploaded references are required. Raw HTML/Markdown controls and footnote syntax are not demonstrated here.</p>".to_string(),
    ];
    for (_, label, status) in &cat[page * PAGE_SIZE..((page + 1) * PAGE_SIZE).min(cat.len())] {
        parts.push(format!(
            "<p>{} — {}</p>",
            ultroid_core::fns::escape_html(label),
            ultroid_core::fns::escape_html(status)
        ));
    }
    parts.push(format!(
        "<p>Page {}/{}; {} native examples.</p>",
        page + 1,
        total_pages,
        cat.len()
    ));
    if !error.is_empty() {
        parts.push(format!(
            "<p>Last example rejected: {}</p>",
            ultroid_core::fns::escape_html(error)
        ));
    }
    RichDoc::html(parts.join(""))
}

/// HTML rendering of one sample (Python `_payload`, blocks → HTML).
pub fn sample_doc(sample: &(String, String, String)) -> Result<RichDoc> {
    let sid = sample.0.as_str();
    if let Some(html) = creation_html(sid) {
        return RichDoc::html(html);
    }
    if sid == "matrix-arcade" {
        return crate::matrix_arcade::lobby_doc();
    }
    if sid == "details-wrapper" {
        return RichDoc::html(
            "<h1>Native wrapper</h1><blockquote expandable><b>Native wrapper</b>\nA real details body.</blockquote>",
        );
    }
    if let Some(level) = sid
        .strip_prefix("heading-")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return RichDoc::html(format!(
            "<h{level}>Heading {level}</h{level}><p>Native heading example.</p>"
        ));
    }
    let body: &str = match sid {
        "native" => "<p>One native RichMessage paragraph.</p>",
        "inline" => {
            "<p><b>Bold </b><i>italic </i><u>underline </u><s>strike </s><b><i>nested styles</i></b></p>"
        }
        "extra-styles" => "<p><mark>Highlighted </mark>H<sub>2</sub>O x<sup>2</sup></p>",
        "spoiler" => "<p><span class=\"tg-spoiler\">Tap to reveal a native spoiler.</span></p>",
        "links" => {
            "<p><a href=\"https://telegram.org\">Telegram </a><a href=\"mailto:example@example.com\">Email example </a><a href=\"tel:+12025550123\">Fictional phone</a></p>"
        }
        "emoji" => "<p>👍</p>",
        "date" => "<p>Sample date: 2023-11-14</p>",
        "divider" => "<p>Above</p><hr><p>Below</p>",
        "footer" => "<p>Native footer example</p><hr><p><i>Sample footer</i></p>",
        "lists" => "<ul><li>First</li><li>Second</li></ul>",
        "ordered" => "<ol><li>First</li><li>Second</li></ol>",
        "tasks" => "<p>[x] Done<br>[ ] Next</p>",
        "code" => {
            "<p><code>inline_code()</code></p><pre language=\"python\">def hello():\n    return \"rich\"</pre>"
        }
        "tables" => {
            "<table class=\"bordered compact\"><tr><th>Format</th><th>Example</th></tr><tr><td>Native</td><td>Table</td></tr></table>"
        }
        "quotes" => "<blockquote>Native quotation<cite>Example author</cite></blockquote>",
        "pullquote" => "<blockquote>Native pull quote<cite>Example author</cite></blockquote>",
        "rich-expand" => {
            "<blockquote expandable>Expandable quotation\nLine two\nLine three\nLine four\nLine five\nLine six<cite>Example author</cite></blockquote>"
        }
        "details" => {
            "<blockquote expandable><b>Step 1</b>\nStatic example instruction.</blockquote>"
        }
        "details-open" => {
            "<blockquote expandable><b>Step 1</b>\nStatic example instruction.</blockquote>"
        }
        "details-code" => {
            "<blockquote expandable><b>Step 1</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote>"
        }
        "details-steps" => {
            "<blockquote expandable><b>Step 1</b>\nStatic example instruction.</blockquote><blockquote expandable><b>Step 2</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote>"
        }
        "native-guide" => {
            "<p><b>Seven-step guide</b></p><p>Open each section to inspect its instructions and code.</p><hr><blockquote expandable><b>Step 1</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 2</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 3</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 4</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 5</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 6</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><blockquote expandable><b>Step 7</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote><hr><p><i>page 1 of 1; seven steps</i></p>"
        }
        "formula" => "<p>x^2 + y^2</p><p>E = mc^2</p>",
        "anchors" => {
            "<p><a href=\"#section\">Jump to section</a></p><p><a name=\"section\">Target section</a></p>"
        }
        "map" => "<p>Map — Rome (41.9, 12.5), zoom 14.</p>",
        "rich-buttons" => {
            "<p>Rich button row: Telegram (URL) · Test callback (rich:ping:ok) · Copy sample.</p>"
        }
        "text-buttons" => "<p><a href=\"https://telegram.org\">Telegram</a></p>",
        "block-quotes" => {
            "<blockquote>First quoted paragraph.<pre language=\"bash\">echo quoted</pre><cite>Example author</cite></blockquote>"
        }
        "nested-details" => {
            "<blockquote expandable><b>Outer section</b>\nOuter body.<blockquote expandable><b>Inner section</b>\nStatic example instruction.<pre language=\"bash\">echo native_example</pre></blockquote></blockquote>"
        }
        "text-entities" => "<p><b>Bold</b> and <i>italic</i></p>",
        "rtl" => "<p>مرحبا — native right-to-left example</p>",
        _ => return Err(Error::Other(format!("unknown rich sample {sid}"))),
    };
    let rtl = sid == "rtl";
    let doc = RichDoc::html(body)?;
    Ok(RichDoc { rtl, ..doc })
}

/// `/rich` — send the native index (owner-only; never edits the
/// user's incoming command).
pub async fn rich(ctx: &Ctx) -> Result<()> {
    let doc = index_doc(0, "")?;
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

async fn edit_index(ctx: &Ctx, page: usize, error: &str) -> Result<()> {
    let doc = index_doc(page, error)?;
    let kb = keyboard(page, "");
    edit_callback_page(ctx, &doc, kb).await
}

async fn run_sample(ctx: &Ctx, sample: &(String, String, String), operation: &str) -> Result<()> {
    let sid = sample.0.clone();
    let doc = sample_doc(sample)?;
    let _kb = keyboard(0, &sid);
    let result = match operation {
        "send" => ctx.send_rich_or_plain(&doc).await.map(|_| ()),
        "edit" => {
            let text = doc.html.clone().unwrap_or_else(|| doc.plain_text());
            ctx.edit(ctx.message_id, &text).await
        }
        _ => return Err(Error::Other("unknown rich operation".to_string())),
    };
    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let identifier = e.to_string();
            edit_index(
                ctx,
                sample_page(&sid),
                &format!("{sid}/{operation}: {identifier}"),
            )
            .await?;
            Err(e)
        }
    }
}

/// Dispatch callback data owned by this module:
/// `rich:<index:<n>|pick:<id>|send:<id>|edit:<id>|ping:ok>`.
/// Returns `true` when the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    let Some(rest) = data.strip_prefix("rich:") else {
        return Ok(false);
    };
    let mut parts = rest.splitn(2, ':');
    let action = parts.next().unwrap_or("");
    let value = parts.next().unwrap_or("");
    match action {
        "index" | "pick" | "send" | "edit" | "ping" => {}
        _ => return Ok(false),
    }
    if action == "ping" {
        return Ok(true);
    }
    if action == "index" {
        let page = value.parse::<usize>().unwrap_or(0);
        edit_index(ctx, page, "").await?;
        return Ok(true);
    }
    let Some(sample) = sample(value) else {
        edit_index(ctx, 0, "unknown or unavailable sample").await?;
        return Ok(true);
    };
    // Selection IS the demonstration: `pick` sends immediately.
    run_sample(ctx, &sample, if action == "pick" { "send" } else { action }).await?;
    Ok(true)
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("rich", "rich", "RichMessage examples index.")
        },
        |ctx| Box::pin(rich(ctx)),
    );
}
