//! Assistant-side decorators and message helpers.
//!
//! Port of `pyUltroid/_misc/_assistant.py`: the plugin-facing half of
//! `_misc` — `asst_cmd`, `callback`, `in_pattern` plus the three helper
//! classes they used to call (`ButtonMapper`, `MessageFormatter`,
//! `AssistantEventPatcher`) and the `Branding` identity strings.
//!
//! What could be ported here, and what could not:
//!
//! * `ButtonMapper`, `MessageFormatter`, `Branding`, `fmtmsg` and the
//!   inline-error rich payload are pure logic and are ported in full.
//! * `asst_cmd`'s registration half is ported as
//!   [`register_asst_command`]: an assistant command is a slash command on
//!   the bot client, registered into the shared [`Registry`]. The
//!   per-event patching half (`AssistantEventPatcher`, which rewrote an
//!   event's `reply`/`respond`/`edit` in place) has no Rust equivalent —
//!   [`Ctx`] is a plain struct, not a dynamic event — so the same behaviour
//!   is available as [`reply_formatted`]: format the text with
//!   [`MessageFormatter::format`] and then reply.
//! * `callback` and `in_pattern` cannot be ported: there is no
//!   CallbackQuery or InlineQuery dispatch surface in `ultroid-core`
//!   (`Registry` dispatches message commands only), and no
//!   `InlineResultBuilder` equivalent. Their payload-building logic that
//!   does not need the wire (the inline-error rich document) is ported as
//!   [`inline_error_doc`].

use ultroid_core::error::Result;
use ultroid_core::rich::RichDoc;
use ultroid_core::router::{CommandDef, CommandHandler, Registry};
use ultroid_core::tg::{ButtonAction, ButtonStyle, InlineButton};
use ultroid_core::{Ctx, fns};

use async_trait::async_trait;
use std::sync::Arc;

/// Longest first line still treated as a heading by
/// [`MessageFormatter::format`] (Python `MAX_HEADING`).
pub const MAX_HEADING: usize = 40;

/// Valid rich-text tags that mean "the caller already produced HTML"
/// (Python `HTML_PREFIXES`).
pub const HTML_TAGS: &[&str] = &[
    "a",
    "b",
    "blockquote",
    "br",
    "code",
    "del",
    "div",
    "em",
    "i",
    "ins",
    "li",
    "ol",
    "p",
    "pre",
    "s",
    "span",
    "strong",
    "tg-emoji",
    "u",
    "ul",
];

/// Style probes that identify a fork "styled" button (Python
/// `STYLE_FIELDS`). In Rust a button is styled when its [`ButtonStyle`] is
/// anything other than [`ButtonStyle::Default`].
pub const STYLE_FIELDS: &[&str] = &["bg_primary", "bg_danger", "bg_success", "icon"];

/// Label fragments that imply a danger style (Python
/// `ButtonMapper.DANGER_HINTS`).
pub const DANGER_HINTS: &[&str] = &["back", "«", "cancel"];

/// Label fragments that imply a success style (Python
/// `ButtonMapper.SUCCESS_HINTS`).
pub const SUCCESS_HINTS: &[&str] = &["on", "approve"];

/// `fmtmsg(head, msgs=...)` — port of `custom/fmt.py::FmtKit.fmtmsg` for the
/// head + `msgs` shape only: a bold heading, then the body in an expandable
/// blockquote. Plain text is HTML-escaped, exactly as Python's
/// `_escape_or_raw` does for plain `str` values.
pub fn fmtmsg(head: &str, msgs: &str) -> String {
    let mut parts = vec![format!("<b>{}</b>", fns::escape_html(head))];
    if !msgs.is_empty() {
        parts.push(format!(
            "<blockquote expandable>{}</blockquote>",
            fns::escape_html(msgs)
        ));
    }
    parts.join("\n\n")
}

/// Normalises plugin-supplied buttons into an `ikm` result.
///
/// In Rust every button is already an [`InlineButton`]; `convert` is the
/// style-inference step Python applied to raw Telethon buttons: a styled
/// button passes through untouched, anything else gets a style inferred
/// from its label by [`ButtonMapper::style_for`].
#[derive(Debug, Clone, Default)]
pub struct ButtonMapper;

impl ButtonMapper {
    /// True for a fork styled button (it must not be rebuilt).
    pub fn is_styled(button: &InlineButton) -> bool {
        button.style != ButtonStyle::Default
    }

    /// Infer a style from the button label. Danger hints win over success
    /// hints, exactly as in Python.
    pub fn style_for(text: &str) -> ButtonStyle {
        let lowered = text.to_lowercase();
        if DANGER_HINTS.iter().any(|hint| lowered.contains(hint)) {
            return ButtonStyle::Danger;
        }
        if SUCCESS_HINTS.iter().any(|hint| lowered.contains(hint)) {
            return ButtonStyle::Success;
        }
        ButtonStyle::Primary
    }

    /// One button -> one entry usable by `ikm`.
    pub fn convert(&self, button: InlineButton) -> InlineButton {
        if Self::is_styled(&button) {
            return button;
        }
        let style = Self::style_for(&button.text);
        button.styled(style)
    }

    /// The whole job. Returns an empty keyboard for empty input (Python
    /// returned `None`; Rust callers pass rows directly, so "no buttons"
    /// is an empty vec).
    pub fn map(&self, rows: Vec<Vec<InlineButton>>) -> Vec<Vec<InlineButton>> {
        rows.into_iter()
            .map(|row| row.into_iter().map(|b| self.convert(b)).collect())
            .collect()
    }
}

/// Decides whether to HTML-format a text, and formats it.
///
/// A plugin that passes its own non-HTML parse mode is left completely
/// alone; otherwise the first line is promoted to a heading and the rest
/// becomes the body, via [`fmtmsg`].
#[derive(Debug, Clone, Default)]
pub struct MessageFormatter;

impl MessageFormatter {
    /// True when the caller set a non-HTML parse mode themselves (Python
    /// `MessageFormatter.caller_chose_parse_mode`).
    pub fn caller_chose_parse_mode(parse_mode: Option<&str>) -> bool {
        matches!(parse_mode, Some(mode) if mode != "html")
    }

    /// True when the text already contains a rich-text HTML tag anywhere
    /// after leading whitespace (Python `MessageFormatter.already_html`,
    /// implemented here with a hand scanner — this crate has no `regex`
    /// dependency, so the Python `HTML_TAG_RE` is matched by hand).
    pub fn already_html(text: &str) -> bool {
        let trimmed = text.trim_start();
        let bytes = trimmed.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                let mut j = i + 1;
                if j < bytes.len() && bytes[j] == b'/' {
                    j += 1;
                }
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
                    j += 1;
                }
                if j > name_start {
                    let name = &trimmed[name_start..j];
                    // Next char must be a non-word char (`\b` in the regex),
                    // and a closing `>` must follow before any `<`
                    // (`[^<>]*>` in the regex).
                    let mut k = j;
                    let mut closed = false;
                    while k < bytes.len() {
                        if bytes[k] == b'>' {
                            closed = true;
                            break;
                        }
                        if bytes[k] == b'<' {
                            break;
                        }
                        k += 1;
                    }
                    let boundary = j >= bytes.len()
                        || bytes[j] == b'>'
                        || bytes[j].is_ascii_whitespace()
                        || bytes[j] == b'/';
                    if boundary
                        && closed
                        && HTML_TAGS.iter().any(|tag| tag.eq_ignore_ascii_case(name))
                    {
                        return true;
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
        false
    }

    /// Promote the first line to a heading and wrap the rest.
    ///
    /// A first line longer than [`MAX_HEADING`] is assumed not to be a
    /// heading: the whole text becomes the body and the default heading is
    /// used. Already-HTML text is returned untouched.
    pub fn format(text: &str, heading_default: &str) -> String {
        if text.is_empty() || Self::already_html(text) {
            return text.to_string();
        }
        let trimmed = text.trim();
        let mut split = trimmed.splitn(2, '\n');
        let head = split.next().unwrap_or("").trim().replace(['~', ':'], "");
        let head = head.trim();
        let body = split.next().unwrap_or("").trim();
        if head.chars().count() > MAX_HEADING {
            return fmtmsg(heading_default, text);
        }
        fmtmsg(head, body)
    }
}

/// Assistant identity strings, resolved against the live client.
///
/// Python reads `ultroid_bot.full_name` / `.uid` at call time; this crate
/// has no bot-user record in [`Ctx`] or [`ultroid_core::Config`], so the
/// owner name falls back to `"Ultroid"` exactly as Python's
/// `getattr(bot, "full_name", None) or "Ultroid"` does, and the owner id
/// comes from [`ultroid_core::Config::owner_id`] (`0` = unknown, rendered
/// without a `tg://user` link).
#[derive(Debug, Clone, Default)]
pub struct Branding;

impl Branding {
    /// The owner display name (`"Ultroid"` — no full name is available in
    /// this crate).
    pub fn owner() -> &'static str {
        "Ultroid"
    }

    /// The greeting card shown to non-owners on the inline surface
    /// (Python `Branding.msg`).
    pub fn msg(owner_id: i64) -> String {
        let owner = if owner_id != 0 {
            format!("[{}](tg://user?id={})", Self::owner(), owner_id)
        } else {
            Self::owner().to_string()
        };
        format!(
            "\n**Ultroid - UserBot**\n➖➖➖➖➖➖➖➖➖➖\n**Owner**: {owner}\n**Support**: @TeamUltroid\n➖➖➖➖➖➖➖➖➖➖\n"
        )
    }
}

/// The inline-surface button rows from `get_bttns()`.
///
/// NOTE: Python's fourth row starts with `("me?", "user", ultroid_bot.uid,
/// "primary")` — a request-peer/user button. [`ButtonAction`] has no user
/// variant, so that one button is omitted here; the other six buttons are
/// faithful, with Python's colour names mapped onto [`ButtonStyle`]
/// (`"blue"` → Primary, `"red"` → Danger, `"green"` → Success).
pub fn get_bttns() -> Vec<Vec<InlineButton>> {
    vec![
        vec![
            InlineButton::url("repo", "https://github.com/TeamUltroid/Ultroid")
                .styled(ButtonStyle::Primary),
            InlineButton::url("Support", "https://t.me/UltroidSupportChat")
                .styled(ButtonStyle::Primary),
        ],
        vec![
            InlineButton {
                text: "help?".to_string(),
                action: ButtonAction::SwitchInline("help".to_string()),
                style: ButtonStyle::Danger,
            },
            InlineButton {
                text: "telethon?".to_string(),
                action: ButtonAction::SwitchInline("tl".to_string()),
                style: ButtonStyle::Danger,
            },
        ],
        vec![InlineButton {
            text: "copy".to_string(),
            action: ButtonAction::Copy("im gay!:)".to_string()),
            style: ButtonStyle::Success,
        }],
        vec![InlineButton {
            text: "hehe".to_string(),
            action: ButtonAction::SwitchInline("test_1".to_string()),
            style: ButtonStyle::Default,
        }],
    ]
}

/// The rich payload [`_answer_inline_error`] answered a failed inline
/// query with: a level-2 "Inline error" header, the query, a "Traceback"
/// paragraph and the traceback as a Python code block, plus a Report URL
/// button. The answering itself needs the inline-query surface (not
/// available in this crate — see the module docs), but the document is
/// portable and is built here.
pub fn inline_error_doc(query: &str, traceback_text: &str) -> Result<RichDoc> {
    RichDoc::markdown(format!(
        "## Inline error\n\nQuery: {query}\n\nTraceback\n\n```python\n{traceback_text}\n```"
    ))
}

/// Format `text` the way the patched assistant event would (heading
/// promotion via [`MessageFormatter`]) and reply with it.
///
/// This is the Rust shape of `AssistantEventPatcher`: there is no
/// per-event patching in Rust, so handlers call this instead of patching
/// `ctx.reply`.
pub async fn reply_formatted(ctx: &Ctx, text: &str, heading_default: &str) -> Result<i64> {
    let formatted = MessageFormatter::format(text, heading_default);
    ctx.reply(&formatted).await
}

/// Register one assistant command — the registration half of Python's
/// `asst_cmd(pattern, owner, ...)`.
///
/// Python compiled `^/{pattern}(?:@\w+)?(?:[ \t]|$)` (slash only); the
/// shared [`Registry`] compiles each pattern for both `.` and `/` handlers,
/// so assistant commands registered here also answer to the `.` handler,
/// exactly like every other command on the shared router. `owner_only`
/// mirrors the decorator's `owner` flag (sender must be in
/// owner-and-sudos; [`Registry::dispatch`] enforces it).
pub fn register_asst_command(
    registry: &mut Registry,
    name: &'static str,
    pattern: &'static str,
    help: &'static str,
    owner_only: bool,
    handler: Arc<dyn CommandHandler>,
) {
    registry.add(
        CommandDef {
            name,
            pattern,
            help,
            owner_only,
            ..Default::default()
        },
        handler,
    );
}

/// The handler-function type: given a borrowed [`Ctx`], return a future
/// that may borrow it for the duration of the `handle` call.
pub type HandlerFn = Arc<
    dyn for<'a> Fn(
            &'a Ctx,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>
        + Send
        + Sync,
>;

/// A [`CommandHandler`] adapter for an async handler function, so
/// assistant modules can register `async fn(&Ctx) -> Result<()>` handlers
/// without a newtype per command. The stored closure is higher-ranked over
/// the `&Ctx` borrow: the future it returns may borrow the context for the
/// duration of the `handle` call, exactly like `async_trait`'s own
/// desugaring of `handle(&self, ctx: &Ctx)`.
pub struct FnHandler(pub HandlerFn);

#[async_trait]
impl CommandHandler for FnHandler {
    async fn handle(&self, ctx: &Ctx) -> Result<()> {
        (self.0)(ctx).await
    }
}

/// Wrap an async handler fn into an `Arc<dyn CommandHandler>`.
/// Usage: `fn_handler(|ctx| Box::pin(my_handler(ctx)))`.
pub fn fn_handler<F>(f: F) -> Arc<dyn CommandHandler>
where
    F: for<'a> Fn(
            &'a Ctx,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(FnHandler(Arc::new(f)))
}
