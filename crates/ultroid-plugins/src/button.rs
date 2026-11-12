//! Button-message builder — port of `plugins/button.py` (`.button`).
//!
//! The text/button-markup parsing (`fns.get_msg_button`) is ported for
//! real: `[label | url]` spans are extracted into URL-button rows
//! (`| same` appends to the previous row), and the spans are stripped
//! from the text.
//!
//! The send path cannot be expressed through `Ctx` in this wave:
//! * Replied media must be downloaded and — for photos/gifs/videos —
//!   uploaded to Telegraph (`telegraph.upload_file`) for a `graph.org`
//!   URL, or packed as a bot file id otherwise. There is no
//!   replied-media download op.
//! * The final send goes through `_inline.something`: an assistant-bot
//!   inline query (`stf<n>`) + click. There is no inline-query op.
//! Both are honest `TODO(responder-op)` errors at the exact call site.

use crate::util::{def, reg_add};
use regex::Regex;
use ultroid_core::{Ctx, Error, Registry, Result};

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

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
    if ctx.reply_to_id.is_some() {
        // TODO(responder-op): download the replied message's media and
        // Telegraph-upload photos/gifs/videos (or pack a bot file id)
        // (Python: button.py:butt -> wt.download_media + telegraph uf).
        tracing::debug!("button: replied media needs a Responder media-download op");
    }
    if text.trim().is_empty() {
        return ctx
            .reply("**Please give some text in correct format.**\n\n`.help button`")
            .await
            .map(|_| ());
    }
    let (clean_text, buttons) = get_msg_button(&text);
    let _ = (clean_text, buttons);
    // TODO(responder-op): deliver via assistant inline query
    // (`_inline.something` -> inline_query "stf<n>" + click)
    // (Python: button.py:butt -> something(event, text, media, buttons)).
    Err(todo_op(
        "assistant inline query 'stf<n>' + click (button message delivery)",
        "button.py:butt",
    ))
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
