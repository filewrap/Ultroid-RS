//! Telethon-style markdown parsing for outgoing messages.
//!
//! Python's client sets a global parse mode (the fork's MarkdownV3), so
//! every plugin string — `**bold**`, `` `code` ``, `__italic__`,
//! `[text](url)` — is parsed into entities before sending. The Rust port
//! sent those strings as plain text, so the markers showed literally
//! (`.bash` output rendered `**☞ BASH` and `` `neofetch` `` verbatim).
//!
//! grammers' own `InputMessage::markdown` is CommonMark and is NOT a
//! substitute — verified against 0.12.0: it drops single newlines
//! (`"line1\nline2"` becomes `"line1line2"`) and collapses newlines
//! inside code spans to spaces, which would mangle command output.
//! This parser is delimiter-based like Telethon's: the text is preserved
//! exactly except for the delimiters themselves, and unmatched
//! delimiters stay literal.
//!
//! Supported constructs (the ones the plugins actually use):
//! `**bold**` (may span newlines), `` `code` `` (newlines preserved),
//! ` ```[lang]\npre\n``` `, `__italic__`, `[text](url)`,
//! `[text](tg://user?id=N)` (mention), `||spoiler||`, `~~strike~~`.
//! Single `*` is NOT italic (Telethon doesn't treat it as one either).
//! Entity offsets/lengths are UTF-16 code units, as Telegram requires.

use grammers_client::{message::InputMessage, tl};

/// Parse Telethon-style markdown into `(plain_text, entities)`.
pub fn parse_markdown(input: &str) -> (String, Vec<tl::enums::MessageEntity>) {
    let mut p = Parser::new(input);
    let len = p.chars.len();
    p.parse_span(0, len);
    p.finish()
}

/// Build an `InputMessage` from a markdown string, the way Python's
/// global parse mode did: delimiters become entities, everything else
/// is sent verbatim.
pub fn md_input(text: &str) -> InputMessage {
    let (text, entities) = parse_markdown(text);
    InputMessage::new().text(text).fmt_entities(entities)
}

struct Parser {
    chars: Vec<char>,
    out: String,
    /// UTF-16 length of `out` (Telegram entity offsets are UTF-16 units).
    out_utf16: usize,
    entities: Vec<tl::enums::MessageEntity>,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            out: String::with_capacity(input.len()),
            out_utf16: 0,
            entities: Vec::new(),
        }
    }

    fn finish(mut self) -> (String, Vec<tl::enums::MessageEntity>) {
        // Entities are emitted inner-first by the recursion; Telegram
        // expects offset order, outer (longer) first on ties.
        self.entities
            .sort_by_key(|e| (entity_offset(e), std::cmp::Reverse(entity_len(e))));
        (self.out, self.entities)
    }

    fn push(&mut self, c: char) {
        self.out.push(c);
        self.out_utf16 += c.len_utf16();
    }

    fn push_slice(&mut self, s: &[char]) {
        for &c in s {
            self.push(c);
        }
    }

    fn at(&self, i: usize, pat: &str) -> bool {
        let p: Vec<char> = pat.chars().collect();
        i + p.len() <= self.chars.len() && self.chars[i..i + p.len()] == p[..]
    }

    /// First index >= `from` where `pat` occurs before `end`, if any.
    fn find(&self, from: usize, end: usize, pat: &str) -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        if p.is_empty() || from + p.len() > end {
            return None;
        }
        (from..=end - p.len()).find(|&i| self.chars[i..i + p.len()] == p[..])
    }

    fn has_content(s: &[char]) -> bool {
        s.iter().any(|c| !c.is_whitespace())
    }

    /// Parse `chars[start..end]` into `out`, emitting entities.
    fn parse_span(&mut self, start: usize, end: usize) {
        let mut i = start;
        while i < end {
            // Fenced pre block first: ```[lang]\n...\n```
            if self.at(i, "```") {
                match self.try_pre(i, end) {
                    Some(next) => {
                        i = next;
                        continue;
                    }
                    None => {
                        self.push('`');
                        i += 1;
                        continue;
                    }
                }
            }
            let c = self.chars[i];
            // Delimited inline constructs: ** __ || ~~ `
            let (delim, kind): (&str, EntityKind) = match c {
                '*' if self.at(i, "**") => ("**", EntityKind::Bold),
                '_' if self.at(i, "__") => ("__", EntityKind::Italic),
                '|' if self.at(i, "||") => ("||", EntityKind::Spoiler),
                '~' if self.at(i, "~~") => ("~~", EntityKind::Strike),
                '`' => ("`", EntityKind::Code),
                _ => {
                    if c == '[' {
                        match self.try_link(i, end) {
                            Some(next) => {
                                i = next;
                                continue;
                            }
                            None => {
                                self.push(c);
                                i += 1;
                                continue;
                            }
                        }
                    }
                    self.push(c);
                    i += 1;
                    continue;
                }
            };
            match self.try_inline(i, end, delim, kind) {
                Some(next) => i = next,
                None => {
                    // No closer (or empty inner): opener stays literal.
                    self.push(c);
                    i += 1;
                }
            }
        }
    }

    /// Try `delim ... delim` starting at `i`. On success parses the inner
    /// span (recursively, unless it is code) and emits the entity.
    /// Returns the index just past the closing delimiter.
    fn try_inline(
        &mut self,
        i: usize,
        end: usize,
        delim: &str,
        kind: EntityKind,
    ) -> Option<usize> {
        let dlen = delim.chars().count();
        let close = self.find(i + dlen, end, delim)?;
        let inner: Vec<char> = self.chars[i + dlen..close].to_vec();
        if !Self::has_content(&inner) {
            return None;
        }
        let start = self.out_utf16;
        if kind == EntityKind::Code {
            // Code content is literal: no entity parsing inside.
            self.push_slice(&inner);
        } else {
            self.parse_span(i + dlen, close);
        }
        let len = self.out_utf16 - start;
        if len == 0 {
            return None;
        }
        self.entities.push(kind.entity(start, len));
        Some(close + dlen)
    }

    /// Try `[text](url)` starting at `i`. Returns the index just past `)`.
    fn try_link(&mut self, i: usize, end: usize) -> Option<usize> {
        let close_bracket = self.find(i + 1, end, "]")?;
        if close_bracket + 1 >= end || self.chars[close_bracket + 1] != '(' {
            return None;
        }
        let close_paren = self.find(close_bracket + 2, end, ")")?;
        let url: String = self.chars[close_bracket + 2..close_paren].iter().collect();
        if url.is_empty() || !Self::has_content(&self.chars[i + 1..close_bracket]) {
            return None;
        }
        let start = self.out_utf16;
        self.parse_span(i + 1, close_bracket);
        let len = self.out_utf16 - start;
        if len == 0 {
            return None;
        }
        if let Some(id) = url
            .strip_prefix("tg://user?id=")
            .and_then(|s| s.parse::<i64>().ok())
        {
            self.entities.push(
                tl::types::MessageEntityMentionName {
                    offset: start as i32,
                    length: len as i32,
                    user_id: id,
                }
                .into(),
            );
        } else {
            self.entities.push(
                tl::types::MessageEntityTextUrl {
                    offset: start as i32,
                    length: len as i32,
                    url,
                }
                .into(),
            );
        }
        Some(close_paren + 1)
    }

    /// Try a fenced pre block starting at `i`. Returns the index just
    /// past the closing fence.
    fn try_pre(&mut self, i: usize, end: usize) -> Option<usize> {
        let close = self.find(i + 3, end, "```")?;
        // Language is the first token of the opening line, if any.
        let mut content_start = i + 3;
        let mut language = String::new();
        if let Some(nl) = (i + 3..close).find(|&j| self.chars[j] == '\n') {
            let line: String = self.chars[i + 3..nl].iter().collect();
            language = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            content_start = nl + 1;
        }
        let mut content_end = close;
        // One trailing newline before the fence is the fence's own.
        if content_end > content_start && self.chars[content_end - 1] == '\n' {
            content_end -= 1;
        }
        if content_end <= content_start {
            return None;
        }
        let content: Vec<char> = self.chars[content_start..content_end].to_vec();
        let start = self.out_utf16;
        self.push_slice(&content);
        let len = self.out_utf16 - start;
        if len == 0 {
            return None;
        }
        self.entities.push(
            tl::types::MessageEntityPre {
                offset: start as i32,
                length: len as i32,
                language,
            }
            .into(),
        );
        Some(close + 3)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum EntityKind {
    Bold,
    Italic,
    Code,
    Spoiler,
    Strike,
}

impl EntityKind {
    fn entity(self, offset: usize, length: usize) -> tl::enums::MessageEntity {
        let (offset, length) = (offset as i32, length as i32);
        match self {
            EntityKind::Bold => tl::types::MessageEntityBold { offset, length }.into(),
            EntityKind::Italic => tl::types::MessageEntityItalic { offset, length }.into(),
            EntityKind::Code => tl::types::MessageEntityCode { offset, length }.into(),
            EntityKind::Spoiler => tl::types::MessageEntitySpoiler { offset, length }.into(),
            EntityKind::Strike => tl::types::MessageEntityStrike { offset, length }.into(),
        }
    }
}

fn entity_offset(e: &tl::enums::MessageEntity) -> i32 {
    use tl::enums::MessageEntity as ME;
    match e {
        ME::Bold(e) => e.offset,
        ME::Italic(e) => e.offset,
        ME::Code(e) => e.offset,
        ME::Pre(e) => e.offset,
        ME::TextUrl(e) => e.offset,
        ME::MentionName(e) => e.offset,
        ME::Spoiler(e) => e.offset,
        ME::Strike(e) => e.offset,
        _ => 0,
    }
}

fn entity_len(e: &tl::enums::MessageEntity) -> i32 {
    use tl::enums::MessageEntity as ME;
    match e {
        ME::Bold(e) => e.length,
        ME::Italic(e) => e.length,
        ME::Code(e) => e.length,
        ME::Pre(e) => e.length,
        ME::TextUrl(e) => e.length,
        ME::MentionName(e) => e.length,
        ME::Spoiler(e) => e.length,
        ME::Strike(e) => e.length,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityBold { offset, length }.into()
    }
    fn italic(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityItalic { offset, length }.into()
    }
    fn code(offset: i32, length: i32) -> tl::enums::MessageEntity {
        tl::types::MessageEntityCode { offset, length }.into()
    }

    #[test]
    fn bash_header_renders() {
        // The exact string from .bash (devtools.rs): bold spans the blank
        // line, the code span keeps its text, newlines survive.
        let (text, entities) =
            parse_markdown("**☞ BASH\n\n• COMMAND:**\n`neofetch` \n\n");
        assert_eq!(text, "☞ BASH\n\n• COMMAND:\nneofetch \n\n");
        assert_eq!(entities, vec![bold(0, 18), code(19, 8)]);
    }

    #[test]
    fn bash_error_renders() {
        let (text, entities) =
            parse_markdown("**• ERROR:** \n`sh: 1: neofetch: Permission denied`\n\n");
        assert_eq!(text, "• ERROR: \nsh: 1: neofetch: Permission denied\n\n");
        assert_eq!(
            entities,
            vec![bold(0, 8), code(10, 34)]
        );
    }

    #[test]
    fn newlines_preserved() {
        let (text, entities) = parse_markdown("line1\nline2\nline3");
        assert_eq!(text, "line1\nline2\nline3");
        assert!(entities.is_empty());
    }

    #[test]
    fn code_span_keeps_table() {
        let (text, entities) =
            parse_markdown("**• OUTPUT:**\n`Filesystem  Size\n/dev/root  28G`");
        assert_eq!(text, "• OUTPUT:\nFilesystem  Size\n/dev/root  28G");
        assert_eq!(entities, vec![bold(0, 9), code(10, 31)]);
    }

    #[test]
    fn unmatched_stays_literal() {
        let (text, entities) = parse_markdown("**oops no close and 2 * 3 = 6 and a_b");
        assert_eq!(text, "**oops no close and 2 * 3 = 6 and a_b");
        assert!(entities.is_empty());
    }

    #[test]
    fn italic_links_mention() {
        let (text, entities) =
            parse_markdown("__note__ [docs](https://example.com) [me](tg://user?id=123)");
        assert_eq!(text, "note docs me");
        assert_eq!(
            entities,
            vec![
                italic(0, 4),
                tl::types::MessageEntityTextUrl {
                    offset: 5,
                    length: 4,
                    url: "https://example.com".to_string(),
                }
                .into(),
                tl::types::MessageEntityMentionName {
                    offset: 10,
                    length: 2,
                    user_id: 123,
                }
                .into(),
            ]
        );
    }

    #[test]
    fn pre_block_with_language() {
        let (text, entities) = parse_markdown("before\n```rust\nfn main() {}\n```\nafter");
        assert_eq!(text, "before\nfn main() {}\nafter");
        assert_eq!(
            entities,
            vec![tl::types::MessageEntityPre {
                offset: 7,
                length: 12,
                language: "rust".to_string(),
            }
            .into()]
        );
    }

    #[test]
    fn emoji_offsets_are_utf16() {
        // 🦀 is 2 UTF-16 units; offsets after it must count 2, not 1.
        let (text, entities) = parse_markdown("**🦀 crab** `x`");
        assert_eq!(text, "🦀 crab x");
        assert_eq!(entities, vec![bold(0, 7), code(8, 1)]);
    }

    #[test]
    fn nested_bold_in_link() {
        let (text, entities) = parse_markdown("[**bold link**](https://x)");
        assert_eq!(text, "bold link");
        assert_eq!(
            entities,
            vec![
                bold(0, 9),
                tl::types::MessageEntityTextUrl {
                    offset: 0,
                    length: 9,
                    url: "https://x".to_string(),
                }
                .into(),
            ]
        );
    }

    #[test]
    fn spoiler_and_strike() {
        let (text, entities) = parse_markdown("||secret|| and ~~gone~~");
        assert_eq!(text, "secret and gone");
        assert_eq!(
            entities,
            vec![
                tl::types::MessageEntitySpoiler {
                    offset: 0,
                    length: 6,
                }
                .into(),
                tl::types::MessageEntityStrike {
                    offset: 11,
                    length: 4,
                }
                .into(),
            ]
        );
    }
}
