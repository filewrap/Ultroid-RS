//! Message entities — port of `custom/entities.py` and
//! `pyUltroid/_core/entities.py`.
//!
//! Telegram entity offsets are UTF-16 code units. These helpers convert
//! between Rust `char` indices and UTF-16 offsets.

/// Convert a byte index in a UTF-8 string to a UTF-16 offset.
pub fn byte_to_utf16(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().map(|c| c.len_utf16()).sum()
}

/// Convert a UTF-16 offset to a byte index.
pub fn utf16_to_byte(text: &str, utf16_offset: usize) -> usize {
    let mut off = 0usize;
    let mut byte = 0usize;
    for c in text.chars() {
        if off >= utf16_offset {
            break;
        }
        off += c.len_utf16();
        byte += c.len_utf8();
    }
    byte
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub kind: EntityKind,
    pub offset_utf16: usize,
    pub length_utf16: usize,
    pub url: Option<String>,
    pub custom_emoji_id: Option<i64>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Code,
    Pre,
    Url,
    TextLink,
    Mention,
    CustomEmoji,
    Spoiler,
    Blockquote,
}
