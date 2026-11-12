//! Button-message builder — port of `plugins/button.py` (`.button`).
//!
//! The text/button-markup parsing (`fns.get_msg_button`) is ported for
//! real: `[label | url]` spans are extracted into URL-button rows
//! (`| same` appends to the previous row), and the spans are stripped
//! from the text.
//!
//! The send path: Python delivers through `_inline.something` — an
//! assistant-bot inline query (`stf<n>`) + click — but the `stf` result
//! is built from a `STUFF` entry registered in the assistant crate's
//! inline handler, which plugins cannot register. The same visible
//! result (text + URL buttons in this chat) is delivered directly with
//! `Responder::send_markup_to` instead. Replied media still cannot be
//! delivered: its kind (pic/gif/video vs other) is not exposed by any
//! trait op, and without it the Telegraph-upload vs bot-file-id branch
//! cannot be chosen, so a media reply is an honest error at that site.

use crate::util::{def, reg_add};
use regex::Regex;
use ultroid_core::tg::InlineButton;
use ultroid_core::{Ctx, Error, Registry, Result};

/// One URL-button row, as `fns.create_tl_btn` builds it:
/// `Vec` of rows, each row a `Vec` of `(label, url)`.
pub type ButtonRows = Vec<Vec<(String, String)>>;

/// Port of `fns.get_msg_button`: split `text` into `(clean_text, buttons)`.
/// A span `[label | url]` starts a new row; `[label | url | same]`
/// appends to the last row.
pub fn get_msg_button(text: &str) -> (String, ButtonRows) {
    let re = Regex::new(r"\[(.*?)\|(.*?)\]").unwrap();
    let mut buttons: ButtonRows = Vec::new();
    for cap in re.captures_iter(text) {
        let label = cap[1].trim().to_string();
        let url = cap[2].trim().to_string();
        let mut parts = url.splitn(2, '|');
        let main = parts.next().unwrap_or("").trim().to_string();
        let same = parts.next().is_some();
        if same {
            if let Some(row) = buttons.last_mut() {
                row.push((label, main));
            } else {
                buttons.push(vec![(label, main)]);
            }
        } else {
            buttons.push(vec![(label, main)]);
        }
    }
    let clean = re.replace_all(text, "").trim().to_string();
    (clean, buttons)
}

/// `.button <text with [label | url] spans>` — port of `button.py::butt`.
async fn button(ctx: &Ctx) -> Result<()> {
    // Reply text is the message body when no inline text is given.
    let mut text = ctx.args.clone();
    if text.trim().is_empty() {
        text = ctx.reply_to_text.clone().unwrap_or_default();
    }
    if let Some(reply_id) = ctx.reply_to_id {
        // A replied message with media needs the media-kind branch
        // (Telegraph upload for pics/gifs/videos, bot file id otherwise);
        // the trait exposes neither the kind nor the pack op.
        if let Some(summary) = ctx
            .responder
            .message_summary(ctx.chat_id, reply_id as i32)
            .await?
        {
            if summary.has_media {
                return Err(Error::Other(
                    "delivering a button message with replied media requires a binary Responder op (media kind + Telegraph upload / bot file id) (Python: button.py:butt)".to_string(),
                ));
            }
        }
    }
    if text.trim().is_empty() {
        return ctx
            .reply("**Please give some text in correct format.**\n\n`.help button`")
            .await
            .map(|_| ());
    }
    let (clean_text, buttons) = get_msg_button(&text);
    if buttons.is_empty() {
        ctx.responder.send_to(ctx.chat_id, &clean_text).await?;
    } else {
        let rows: Vec<Vec<InlineButton>> = buttons
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(label, url)| InlineButton::url(label, url))
                    .collect()
            })
            .collect();
        ctx.responder
            .send_markup_to(ctx.chat_id, &clean_text, rows)
            .await?;
    }
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "button",
            "button",
            "Create a message with URL buttons: .button text [label | url]",
        ),
        |ctx| Box::pin(button(ctx)),
    );
}
