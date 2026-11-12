//! Telegram surface the agent speaks: text, entities and coloured
//! keyboards — port of `pyUltroid/agent/_telegram.py`.
//!
//! The pinned Telethon fork is not uniform about what a message hands
//! back, and the agent reads messages from two clients that were
//! configured differently. Every read path funnels through [`text_of`]
//! so no caller has to know which shape it got.
//!
//! Observed shapes, all of them real in this fork:
//!
//! * `str` — no parse mode on the client, or an unformatted message.
//! * `(text, entities)` — `Message.text` when `parse_mode` is set.
//! * `None` — `MessageService`, or a message with no body.
//! * the object itself — anything exposing `.message`/`.raw_text`.
//!
//! In Rust those shapes arrive as `serde_json::Value`: a string, a
//! `[text, entities]` array, `null`, or an object with `message` /
//! `raw_text` / `text` fields.
//!
//! Buttons: the fork extends every `Button.*` with `style=` (mapped to
//! `primary`/`danger`/`success`) and `icon=`. There are no Telethon
//! objects in this crate, so [`inline_rows`] builds [`ButtonSpec`]
//! values — plain data the binary crate maps onto real buttons later.

use regex::Regex;
use serde_json::{Value, json};
use ultroid_core::error::{Error, Result};

/// The style tokens the fork's `_get_style` accepts. Anything else raises
/// inside Telethon, so they are validated here where the message is useful.
pub const BUTTON_STYLES: &[&str] = &["primary", "danger", "success"];

// ── message text ──────────────────────────────────────────────────────────

/// Split any parse-mode result into `(text, entities)`.
///
/// The single chokepoint for the fork's tuple-returning parse modes.
/// Accepts a bare string, a `(text, entities)` pair, `null`, or any
/// object with a `.message`/`.raw_text` attribute. Never raises: a
/// message that cannot be read becomes *fallback*.
pub fn unwrap_pair(value: &Value, fallback: &str) -> (String, Vec<Value>) {
    match value {
        Value::Null => (fallback.to_string(), Vec::new()),
        Value::String(s) => (s.clone(), Vec::new()),
        Value::Array(items) => {
            if items.is_empty() {
                return (fallback.to_string(), Vec::new());
            }
            let text = match &items[0] {
                Value::String(s) => s.clone(),
                _ => fallback.to_string(),
            };
            let entities = match items.get(1) {
                Some(Value::Array(e)) => e.iter().filter(|v| !v.is_null()).cloned().collect(),
                _ => Vec::new(),
            };
            (text, entities)
        }
        Value::Object(obj) => {
            // An object that carries the message: prefer the raw body,
            // because `.text` may itself be the tuple we are here to unwrap.
            for attr in ["raw_text", "message"] {
                if let Some(Value::String(s)) = obj.get(attr) {
                    let entities = match obj.get("entities") {
                        Some(Value::Array(e)) => {
                            e.iter().filter(|v| !v.is_null()).cloned().collect()
                        }
                        _ => Vec::new(),
                    };
                    return (s.clone(), entities);
                }
                if let Some(inner) = obj.get(attr) {
                    if !inner.is_null() && !matches!(inner, Value::Number(_) | Value::Bool(_)) {
                        return unwrap_pair(inner, fallback);
                    }
                }
            }
            if let Some(text) = obj.get("text") {
                return unwrap_pair(text, fallback);
            }
            (fallback.to_string(), Vec::new())
        }
        _ => (fallback.to_string(), Vec::new()),
    }
}

/// The plain text of *message*, whatever shape the fork returned.
///
/// This is the only text accessor agent code is allowed to use. Reading
/// `message.text` directly is what puts `"('hi', [...])"` in a prompt.
pub fn text_of(message: &Value) -> String {
    text_of_fallback(message, "")
}

pub fn text_of_fallback(message: &Value, fallback: &str) -> String {
    unwrap_pair(message, fallback).0
}

/// The plain text of a bare string message.
pub fn text_of_str(message: &str) -> String {
    message.to_string()
}

/// The formatting entities of *message*, always a list, never null.
pub fn entities_of(message: &Value) -> Vec<Value> {
    unwrap_pair(message, "").1
}

/// The unformatted body, ignoring entities and any parse mode.
pub fn raw_message(message: &Value) -> String {
    if let Value::Object(obj) = message {
        for attr in ["message", "raw_text"] {
            if let Some(Value::String(s)) = obj.get(attr) {
                return s.clone();
            }
        }
    }
    if let Value::String(s) = message {
        return s.clone();
    }
    text_of(message)
}

fn tool_trace_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?is)(?:\[\[\s*tool_calls?\s*\]\]|<\|tool_calls?\|>|<tool_calls?>|(?:^|\n)\s*tool_calls?\s*[:=]).*",
        )
        .expect("static regex")
    })
}

/// Remove a serialized tool-protocol tail from an agent response.
pub fn public_text(value: &str) -> String {
    let text = value.trim().to_string();
    match tool_trace_re().find(&text) {
        Some(m) => text[..m.start()].trim().to_string(),
        None => text,
    }
}

// ── rich markup ───────────────────────────────────────────────────────────

/// Whether `rich_output` detected HTML or Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichFormat {
    Html,
    Markdown,
}

/// An agent answer preserved as native rich markup: unwrapped text plus
/// the format it was written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichMarkupData {
    pub text: String,
    pub format: RichFormat,
}

fn rich_html_tag_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?is)</?(?:html|body|p|div|h[1-6]|b|strong|i|em|u|s|del|ins|blockquote|ul|ol|li|table|caption|thead|tbody|tfoot|tr|td|th|details|summary|button|pre|code|a|br|hr|span|tg-spoiler|tg-emoji)\b[^>]*>",
        )
        .expect("static regex")
    })
}

fn fence_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?is)\A```(html|md|markdown)[ \t]*\n([\s\S]*?)\n```\z").expect("static regex")
    })
}

fn code_span_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```[\s\S]*?```|`[^`]*`").expect("static regex"))
}

/// Preserve an agent's HTML/Markdown answer as native rich markup.
///
/// Explicit whole-response `html`/`md` fences are unwrapped. Markdown
/// code blocks are excluded from HTML detection so code samples are not
/// mistaken for message markup. Plain text is treated as Markdown
/// because it is valid unformatted Markdown and remains safe in the
/// fallback renderer.
pub fn rich_output(value: &str) -> RichMarkupData {
    let text = value.trim().to_string();
    let mut format: Option<RichFormat> = None;
    let mut text = text;
    if let Some(caps) = fence_re().captures(&text) {
        // Extract everything borrowed from `text` first, then assign.
        let inner = caps[2].trim().to_string();
        let fmt = if caps[1].eq_ignore_ascii_case("html") {
            RichFormat::Html
        } else {
            RichFormat::Markdown
        };
        text = inner;
        format = Some(fmt);
    }
    if format.is_none() {
        let probe = code_span_re().replace_all(&text, "");
        format = Some(if rich_html_tag_re().is_match(&probe) {
            RichFormat::Html
        } else {
            RichFormat::Markdown
        });
    }
    RichMarkupData {
        text,
        format: format.unwrap_or(RichFormat::Markdown),
    }
}

/// Readable answer text from a [`RichMarkupData`] value, without
/// synthetic action labels.
///
/// Markdown is stripped to plain text; HTML is flattened (script/style
/// hidden, block tags become newlines, table cells tab-separated).
pub fn markup_to_plain(markup: &RichMarkupData) -> String {
    match markup.format {
        RichFormat::Markdown => strip_markdown(&markup.text),
        RichFormat::Html => html_to_plain(&markup.text),
    }
}

/// Strip Markdown markers, keeping the readable text.
pub fn strip_markdown(text: &str) -> String {
    // Fenced blocks keep their inner text; inline code keeps its text.
    let fence = Regex::new(r"(?s)```[^\n]*\n([\s\S]*?)```").unwrap();
    let mut out = fence.replace_all(text, "$1").to_string();
    let inline_code = Regex::new(r"`([^`]*)`").unwrap();
    out = inline_code.replace_all(&out, "$1").to_string();
    // Links keep their label.
    let link = Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap();
    out = link.replace_all(&out, "$1").to_string();
    let bold = Regex::new(r"(\*\*|__)(.+?)\1").unwrap();
    out = bold.replace_all(&out, "$2").to_string();
    let strike = Regex::new(r"~~(.+?)~~").unwrap();
    out = strike.replace_all(&out, "$1").to_string();
    let em = Regex::new(r"(\*|_)(.+?)\1").unwrap();
    out = em.replace_all(&out, "$2").to_string();
    // Headings keep their text.
    let heading = Regex::new(r"(?m)^#{1,6}\s+").unwrap();
    out = heading.replace_all(&out, "").to_string();
    out.trim().to_string()
}

/// Flatten HTML to plain text, following the `Plain` parser in
/// `_presentation.py`: script/style content is hidden, block tags add
/// newlines, table cells are tab-separated.
pub fn html_to_plain(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut hidden = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let block_open = [
        "br", "hr", "p", "div", "li", "tr", "pre", "summary", "h1", "h2", "h3", "h4", "h5", "h6",
    ];
    let block_close = [
        "p", "div", "li", "tr", "pre", "summary", "h1", "h2", "h3", "h4", "h5", "h6",
    ];
    while i < chars.len() {
        if chars[i] == '<' {
            // Read a tag.
            let mut j = i + 1;
            let mut closing = false;
            if j < chars.len() && chars[j] == '/' {
                closing = true;
                j += 1;
            }
            let start = j;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '-') {
                j += 1;
            }
            let tag: String = chars[start..j].iter().collect::<String>().to_lowercase();
            // Skip to '>'.
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            if tag == "script" || tag == "style" {
                if closing {
                    hidden = hidden.saturating_sub(1);
                } else {
                    hidden += 1;
                }
            } else if hidden == 0 {
                if !closing && block_open.contains(&tag.as_str()) {
                    parts.push("\n".to_string());
                } else if closing && block_close.contains(&tag.as_str()) {
                    parts.push("\n".to_string());
                } else if !closing && (tag == "td" || tag == "th") {
                    // Tabs are added on close in Python; add on open is
                    // equivalent for plain text (cell content follows).
                } else if closing && (tag == "td" || tag == "th") {
                    parts.push("\t".to_string());
                }
            }
            i = if j < chars.len() { j + 1 } else { chars.len() };
        } else {
            // Text run until the next tag, with charrefs decoded.
            let start = i;
            while i < chars.len() && chars[i] != '<' {
                i += 1;
            }
            if hidden == 0 {
                let raw: String = chars[start..i].iter().collect();
                parts.push(decode_charrefs(&raw));
            }
        }
    }
    parts.concat().trim_matches('\n').to_string()
}

fn decode_charrefs(text: &str) -> String {
    let mut out = text.to_string();
    for (from, to) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
    ] {
        out = out.replace(from, to);
    }
    // Numeric charrefs: &#123; and &#x1F;
    let re = Regex::new(r"&#(\d+);|&#x([0-9a-fA-F]+);").unwrap();
    let mut result = String::new();
    let mut last = 0;
    for caps in re.captures_iter(&out.clone()) {
        let m = caps.get(0).unwrap();
        result.push_str(&out[last..m.start()]);
        let code = if let Some(d) = caps.get(1) {
            d.as_str().parse::<u32>().ok()
        } else {
            u32::from_str_radix(caps.get(2).unwrap().as_str(), 16).ok()
        };
        match code.and_then(char::from_u32) {
            Some(c) => result.push(c),
            None => result.push_str(m.as_str()),
        }
        last = m.end();
    }
    result.push_str(&out[last..]);
    result
}

// ── inline keyboards ──────────────────────────────────────────────────────

/// One normalized inline-button cell. The binary crate maps this onto a
/// real Telethon button (or a grammers equivalent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonSpec {
    pub text: String,
    pub data: Option<Vec<u8>>,
    pub url: Option<String>,
    pub style: Option<String>,
    pub icon: Option<String>,
}

impl ButtonSpec {
    pub fn text_data(text: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        ButtonSpec {
            text: text.into(),
            data: Some(data.into()),
            url: None,
            style: None,
            icon: None,
        }
    }
}

/// One keyboard cell as a caller may supply it.
#[derive(Debug, Clone)]
pub enum ButtonCell {
    /// A bare label: `{"text": label, "data": label}`.
    Text(String),
    /// A full spec.
    Spec(ButtonSpec),
    /// A documented `ikm` tuple, as strings:
    /// `(text, data)` / `(text, data, style)` / `(text, data, style, icon)`
    /// / `(text, "url", url[, style[, icon]])`.
    Tuple(Vec<String>),
}

fn build_cell(
    cell: &ButtonCell,
    default_style: Option<&str>,
    default_icon: Option<&str>,
) -> Result<ButtonSpec> {
    match cell {
        ButtonCell::Text(text) => Ok(ButtonSpec {
            text: text.clone(),
            data: Some(text.clone().into_bytes()),
            url: None,
            style: default_style.map(|s| s.to_string()),
            icon: default_icon.map(|s| s.to_string()),
        }),
        ButtonCell::Spec(spec) => {
            let mut spec = spec.clone();
            if spec.style.is_none() {
                spec.style = default_style.map(|s| s.to_string());
            }
            if spec.icon.is_none() {
                spec.icon = default_icon.map(|s| s.to_string());
            }
            validate_style(spec.style.as_deref())?;
            Ok(spec)
        }
        ButtonCell::Tuple(items) => build_cell(
            &ButtonCell::Spec(tuple_to_spec(items)?),
            default_style,
            default_icon,
        ),
    }
}

fn validate_style(style: Option<&str>) -> Result<()> {
    if let Some(style) = style {
        if !BUTTON_STYLES.contains(&style) {
            return Err(Error::Other(format!(
                "unknown button style {style:?}; expected one of {:?}",
                BUTTON_STYLES.iter().collect::<Vec<_>>()
            )));
        }
    }
    Ok(())
}

fn tuple_to_spec(cell: &[String]) -> Result<ButtonSpec> {
    if cell.is_empty() {
        return Err(Error::Other("empty button tuple".to_string()));
    }
    let text = cell[0].clone();
    let rest = &cell[1..];
    if rest.len() >= 2 && rest[0] == "url" {
        let mut spec = ButtonSpec {
            text,
            data: None,
            url: Some(rest[1].clone()),
            style: None,
            icon: None,
        };
        if rest.len() > 2 && !rest[2].is_empty() {
            spec.style = Some(rest[2].clone());
        }
        if rest.len() > 3 && !rest[3].is_empty() {
            spec.icon = Some(rest[3].clone());
        }
        return Ok(spec);
    }
    if rest.is_empty() {
        return Ok(ButtonSpec {
            text: text.clone(),
            data: Some(text.into_bytes()),
            url: None,
            style: None,
            icon: None,
        });
    }
    let mut spec = ButtonSpec {
        text,
        data: Some(rest[0].clone().into_bytes()),
        url: None,
        style: None,
        icon: None,
    };
    if rest.len() > 1 && !rest[1].is_empty() {
        spec.style = Some(rest[1].clone());
    }
    if rest.len() > 2 && !rest[2].is_empty() {
        spec.icon = Some(rest[2].clone());
    }
    Ok(spec)
}

/// Build an inline keyboard, normalised, validated and style-defaulted.
///
/// Each cell may be a [`ButtonCell::Text`], a [`ButtonCell::Spec`], or a
/// [`ButtonCell::Tuple`] in any of the documented `ikm` shapes. A single
/// cell is promoted to one row so callers may pass `["Yes", "No"]` for a
/// two-button row.
///
/// Rows are returned as a list of lists — Telethon accepts that
/// directly, and returning a pre-built `ReplyInlineMarkup` instead
/// raises `ReplyMarkupInvalidError`.
///
/// Note: Python also passes pre-built Telethon buttons through
/// untouched (`_is_built_button`). There is no pre-built-button type in
/// this crate; a [`ButtonSpec`] *is* the built form.
pub fn inline_rows(
    rows: Vec<Vec<ButtonCell>>,
    default_style: Option<&str>,
    default_icon: Option<&str>,
) -> Result<Vec<Vec<ButtonSpec>>> {
    let mut built: Vec<Vec<ButtonSpec>> = Vec::new();
    for row in rows {
        let mut out_row: Vec<ButtonSpec> = Vec::new();
        for cell in &row {
            out_row.push(build_cell(cell, default_style, default_icon)?);
        }
        if !out_row.is_empty() {
            built.push(out_row);
        }
    }
    Ok(built)
}

/// One keyboard row from the given cells.
pub fn row_of(cells: Vec<ButtonCell>) -> Result<Vec<Vec<ButtonSpec>>> {
    inline_rows(vec![cells], None, None)
}

/// A flat list of cells as a single row (the Python promotion of a
/// non-nested `rows` argument).
pub fn inline_row(cells: Vec<ButtonCell>) -> Result<Vec<Vec<ButtonSpec>>> {
    inline_rows(vec![cells], None, None)
}

/// JSON view of a button spec, for diagnostics.
pub fn button_to_json(spec: &ButtonSpec) -> Value {
    json!({
        "text": spec.text,
        "data": spec.data.as_ref().map(|d| String::from_utf8_lossy(d).to_string()),
        "url": spec.url,
        "style": spec.style,
        "icon": spec.icon,
    })
}
