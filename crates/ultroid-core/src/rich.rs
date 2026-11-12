//! Rich-message *data* — port of the `RichBlocks` builder surface in
//! `pyUltroid/_core/richmsg.py`.
//!
//! This is pure data (no grammers dependency): a document plus its plain-text
//! fallback. The grammers conversion lives in `ultroid-rich`.
//! Limits mirror Python: MAX_RICH_MESSAGE_CHARS = 32_768,
//! MAX_RICH_MESSAGE_BLOCKS = 500, nesting 16, table cols 20.

use crate::error::{Error, Result};

pub const MAX_RICH_MESSAGE_CHARS: usize = 32_768;
pub const MAX_RICH_MESSAGE_BLOCKS: usize = 500;
pub const MAX_RICH_NESTING: usize = 16;
pub const MAX_TABLE_COLS: usize = 20;

/// One media binding for a rich document (`tg://photo?id=…` /
/// `tg://document?id=…`). Only bot-sent rich may carry these.
#[derive(Debug, Clone)]
pub struct RichMedia {
    pub kind: RichMediaKind,
    pub id: i64,
    pub access_hash: i64,
    pub file_reference: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichMediaKind {
    Photo,
    Video,
    Audio,
    Document,
}

/// A rich document: exactly one of markdown / html / blocks, as in
/// `InputRichMessage` (Bot API 10.1 / grammers 0.12).
#[derive(Debug, Clone, Default)]
pub struct RichDoc {
    pub markdown: Option<String>,
    pub html: Option<String>,
    /// Legacy PageBlock text lines (Instant View blocks). Used only where the
    /// Python code built blocks; never invent Bot API 10.2 `InputRichBlock`
    /// types — they do not exist in grammers 0.12.
    pub blocks: Vec<String>,
    pub rtl: bool,
    pub no_autolink: bool,
    pub media: Vec<RichMedia>,
}

impl RichDoc {
    pub fn markdown(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        Self::check_len(&text)?;
        Ok(RichDoc {
            markdown: Some(text),
            ..Default::default()
        })
    }

    pub fn html(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        Self::check_len(&text)?;
        Ok(RichDoc {
            html: Some(text),
            ..Default::default()
        })
    }

    fn check_len(text: &str) -> Result<()> {
        if text.chars().count() > MAX_RICH_MESSAGE_CHARS {
            return Err(Error::Other(format!(
                "rich message exceeds {MAX_RICH_MESSAGE_CHARS} chars"
            )));
        }
        Ok(())
    }

    /// Plain-text fallback — same rule as Python: every rich send also
    /// renders to plain text + entities. Strips markdown/html markers
    /// conservatively (headings, bold/italic/code markers, links keep text).
    pub fn plain_text(&self) -> String {
        let raw = self
            .markdown
            .clone()
            .or_else(|| self.html.clone())
            .unwrap_or_else(|| self.blocks.join("\n"));
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '#' => {
                    // heading markers at line start: drop run of '#'
                    while chars.peek() == Some(&'#') {
                        chars.next();
                    }
                    if chars.peek() == Some(&' ') {
                        chars.next();
                    }
                }
                '*' | '_' | '`' | '~' => {
                    // bold/italic/code/strike markers: drop doubled or single
                    if chars.peek() == Some(&c) {
                        chars.next();
                    }
                }
                '<' => {
                    // strip <tag> ... keep inner text
                    let mut tag = String::new();
                    let mut closed = false;
                    for c2 in chars.by_ref() {
                        if c2 == '>' {
                            closed = true;
                            break;
                        }
                        tag.push(c2);
                    }
                    if !closed {
                        out.push('<');
                        out.push_str(&tag);
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.markdown.as_deref().unwrap_or("").is_empty()
            && self.html.as_deref().unwrap_or("").is_empty()
            && self.blocks.is_empty()
    }
}
