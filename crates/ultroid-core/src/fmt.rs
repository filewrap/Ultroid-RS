//! Text formatting helpers — port of `custom/fmt.py` and `custom/rich.py`
//! (`parse_rich`/`unparse_rich`, MarkdownV3 + `||spoiler||`, premium emoji).

/// Wrap text in Telegram HTML bold.
pub fn bold(text: &str) -> String {
    format!("<b>{}</b>", crate::fns::escape_html(text))
}

/// Wrap text in Telegram HTML italic.
pub fn italic(text: &str) -> String {
    format!("<i>{}</i>", crate::fns::escape_html(text))
}

/// Inline code.
pub fn code(text: &str) -> String {
    format!("<code>{}</code>", crate::fns::escape_html(text))
}

/// Pre-formatted block.
pub fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", crate::fns::escape_html(text))
}

/// Spoiler.
pub fn spoiler(text: &str) -> String {
    format!("<tg-spoiler>{}</tg-spoiler>", crate::fns::escape_html(text))
}

/// Link with visible text.
pub fn link(text: &str, url: &str) -> String {
    format!(
        "<a href=\"{}\">{}</a>",
        crate::fns::escape_html(url),
        crate::fns::escape_html(text)
    )
}

/// Mention a user by id (`tg://user?id=…`).
pub fn mention(user_id: i64, name: &str) -> String {
    format!(
        "<a href=\"tg://user?id={}\">{}</a>",
        user_id,
        crate::fns::escape_html(name)
    )
}
