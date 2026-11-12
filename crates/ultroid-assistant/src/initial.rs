//! First-run guide pages.
//!
//! Port of `assistant/initial.py`: a 5-page "Welcome to Ultroid"
//! guide navigated by `assistant:initial:prev|next:<n>` callback
//! data. Page 1 is the welcome, 2 about, 3 quick links, 4 finding
//! commands (with the live handler prefix), 5 support.
//!
//! The handler takes the raw callback data (see `crate::cbs`): the
//! page number is the last colon segment. Out-of-range or
//! unparseable payloads are answered with the same alert texts
//! Python uses (delivered as a reply — `Ctx` has no alert channel).

use ultroid_core::Ctx;
use ultroid_core::error::Result;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};

use crate::cbs;
use crate::rich_pages::edit_callback_page;

/// Callback-data prefix for the guide (`cb_pattern("assistant", "initial")`).
pub const PAGE_PREFIX: [&str; 2] = ["assistant", "initial"];

/// The five guide pages, in order. Page 4 interpolates the live
/// handler prefix (Python builds it from `HNDLR` at import time).
pub fn page_text(page: usize, hndlr: &str) -> Option<String> {
    let h = ultroid_core::fns::escape_html(hndlr);
    match page {
        1 => Some(
            "<b>Welcome to Ultroid</b>\n\nYour userbot is online. This short guide covers the first commands, help, and support."
                .to_string(),
        ),
        2 => Some(
            "<b>About Ultroid</b>\n\nUltroid is a modular Telegram userbot built with Telethon. Its plugins add tools for messaging, moderation, media, and automation.\n\nMaintained by <a href=\"https://t.me/TeamUltroid\">TeamUltroid</a>."
                .to_string(),
        ),
        3 => Some(
            "<b>Quick links</b>\n\n<a href=\"https://t.me/UltroidUpdates/24\">Username tracker</a>\n<a href=\"https://t.me/UltroidUpdates/28\">Custom add-ons</a>\n<a href=\"https://t.me/UltroidUpdates/22\">Time zone setup</a>\n<a href=\"https://t.me/UltroidUpdates/21\">Inline PMPermit</a>\n<a href=\"https://t.me/UltroidUpdates/18\">Dual mode</a>\n<a href=\"https://t.me/UltroidUpdates/2\">Private-message bot</a>\n<a href=\"https://t.me/UltroidUpdates/14\">More guides</a>"
                .to_string(),
        ),
        4 => Some(format!(
            "<b>Find commands</b>\n\n<code>{h}help</code> opens the help menu.\n<code>{h}help &lt;plugin&gt;</code> shows a plugin's commands.\n<code>{h}cmds</code> opens the complete command index."
        )),
        5 => Some(
            "<b>Need a hand?</b>\n\nFor questions, suggestions, and troubleshooting, visit <a href=\"https://t.me/UltroidSupportChat\">Ultroid Support</a>.\n\nYou're all set."
                .to_string(),
        ),
        _ => None,
    }
}

/// Number of guide pages.
pub const PAGE_COUNT: usize = 5;

/// Navigation buttons for `current` (Python `_page_buttons`):
/// Previous when past page 1, Next when before the last page, and
/// "Back to start" on the last page.
pub fn page_buttons(current: usize) -> Vec<Vec<InlineButton>> {
    let mut row: Vec<InlineButton> = Vec::new();
    if current > 1 {
        let data = cbs::cb(&["assistant", "initial", "prev", &(current - 1).to_string()])
            .unwrap_or_default();
        row.push(InlineButton::data("Previous", data.into_bytes()).styled(ButtonStyle::Primary));
    }
    if current < PAGE_COUNT {
        let data = cbs::cb(&["assistant", "initial", "next", &(current + 1).to_string()])
            .unwrap_or_default();
        row.push(InlineButton::data("Next", data.into_bytes()).styled(ButtonStyle::Primary));
    } else if current == PAGE_COUNT {
        let data = cbs::cb(&["assistant", "initial", "prev", "1"]).unwrap_or_default();
        row.push(InlineButton::data("Back to start", data.into_bytes()));
    }
    if row.is_empty() {
        Vec::new()
    } else {
        vec![row]
    }
}

/// Render one guide page into the callback message (Python
/// `_render_page`): clamps to 1..=PAGE_COUNT, edits with the page
/// HTML and its navigation buttons, no link preview.
pub async fn render_page(ctx: &Ctx, current: usize) -> Result<()> {
    let current = current.clamp(1, PAGE_COUNT);
    let Some(text) = page_text(current, &ctx.config.hndlr) else {
        return Ok(());
    };
    let doc = RichDoc::html(text)?;
    edit_callback_page(ctx, &doc, page_buttons(current)).await
}

/// Dispatch callback data owned by this module. The page number is
/// the last colon segment of `assistant:initial:<prev|next>:<n>`.
/// Returns `true` when the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    let Some(tail) = cbs::cb_tail(data, &PAGE_PREFIX)? else {
        return Ok(false);
    };
    let last = tail.rsplit(':').next().unwrap_or("");
    let Ok(current) = last.parse::<usize>() else {
        ctx.reply("This guide page could not be opened.").await?;
        return Ok(true);
    };
    if !(1..=PAGE_COUNT).contains(&current) {
        ctx.reply("This guide page no longer exists.").await?;
        return Ok(true);
    }
    render_page(ctx, current).await?;
    Ok(true)
}
