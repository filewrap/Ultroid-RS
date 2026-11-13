//! Public agent answers with native Layer-230 structure and safe nested
//! actions — port of `pyUltroid/agent/_presentation.py`.
//!
//! The Python source builds Telethon TL types (`types.PageBlock*`,
//! `types.Text*`) directly. This crate has no Telethon/grammers
//! dependency, so the same structure is built as plain Rust data
//! ([`RichText`] / [`PageBlock`]) — the nesting, budgets, copy/source
//! controls and URL-safety rules are ported exactly, and the binary
//! crate maps this model onto TL `PageBlock`/`RichText` values when it
//! sends them. The text-extraction logic (`answer_text`,
//! `answer_plain`, `render_answer`) is behavior-identical.

use serde_json::{Value, json};
use ultroid_core::error::{Error, Result};
use ultroid_core::notices::sanitize_error_text;

use crate::agent::AgentResponse;
use crate::telegram::{RichFormat, RichMarkupData, public_text, rich_output};

// ── answer source ─────────────────────────────────────────────────────────

/// Anything that can supply a public answer: an [`AgentResponse`], a
/// plain string, or any future response type.
pub trait AnswerSource {
    fn content(&self) -> &str;
    fn error(&self) -> Option<&str>;
    fn provider(&self) -> &str;
}

/// A plain answer with no error and no provider.
#[derive(Debug, Clone, Default)]
pub struct SimpleAnswer {
    pub content: String,
    pub error: Option<String>,
    pub provider: String,
}

impl SimpleAnswer {
    pub fn text(content: impl Into<String>) -> Self {
        SimpleAnswer {
            content: content.into(),
            error: None,
            provider: String::new(),
        }
    }
}

impl AnswerSource for SimpleAnswer {
    fn content(&self) -> &str {
        &self.content
    }
    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    fn provider(&self) -> &str {
        &self.provider
    }
}

impl AnswerSource for AgentResponse {
    fn content(&self) -> &str {
        &self.content
    }
    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    fn provider(&self) -> &str {
        &self.provider
    }
}

impl AnswerSource for str {
    fn content(&self) -> &str {
        self
    }
    fn error(&self) -> Option<&str> {
        None
    }
    fn provider(&self) -> &str {
        ""
    }
}

impl AnswerSource for String {
    fn content(&self) -> &str {
        self
    }
    fn error(&self) -> Option<&str> {
        None
    }
    fn provider(&self) -> &str {
        ""
    }
}

// ── HTML tree ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Node {
    pub tag: String,
    pub attrs: Vec<(String, Option<String>)>,
    pub children: Vec<NodeChild>,
}

#[derive(Debug, Clone)]
pub enum NodeChild {
    Text(String),
    Element(Node),
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.children_attr(name)
    }

    fn children_attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

const VOID_TAGS: &[&str] = &["br", "hr", "img", "input", "meta", "link"];

/// Minimal HTML parser mirroring `_Tree` (`html.parser.HTMLParser`
/// with `convert_charrefs=True`): depth cap 18, node cap 800, end-tag
/// unwinding to the matching open tag.
///
/// Python's parser hands the *same* node object to its parent's
/// children list and to the open-element stack, so a close tag only has
/// to pop the stack. That sharing is reproduced here with
/// `Rc<RefCell<...>>` during parsing; the finished tree is converted
/// to the owned [`Node`] model before it is returned.
pub struct HtmlTree;

impl HtmlTree {
    pub fn parse(text: &str) -> Result<Node> {
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Debug)]
        struct Mutable {
            tag: String,
            attrs: Vec<(String, Option<String>)>,
            children: Vec<MutableChild>,
        }
        #[derive(Debug)]
        enum MutableChild {
            Text(String),
            Element(Rc<RefCell<Mutable>>),
        }

        fn to_owned(node: &Rc<RefCell<Mutable>>) -> Node {
            let node = node.borrow();
            Node {
                tag: node.tag.clone(),
                attrs: node.attrs.clone(),
                children: node
                    .children
                    .iter()
                    .map(|c| match c {
                        MutableChild::Text(s) => NodeChild::Text(s.clone()),
                        MutableChild::Element(el) => NodeChild::Element(to_owned(el)),
                    })
                    .collect(),
            }
        }

        let root = Rc::new(RefCell::new(Mutable {
            tag: "root".to_string(),
            attrs: Vec::new(),
            children: Vec::new(),
        }));
        let mut stack: Vec<Rc<RefCell<Mutable>>> = vec![Rc::clone(&root)];
        let mut count = 0usize;
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '<' {
                // Comment: skip.
                if text[i..].starts_with("<!--") {
                    match text[i..].find("-->") {
                        Some(end) => {
                            i += end + 3;
                            continue;
                        }
                        None => break,
                    }
                }
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
                if j == start {
                    // Not a tag — literal '<'.
                    stack
                        .last()
                        .unwrap()
                        .borrow_mut()
                        .children
                        .push(MutableChild::Text("<".to_string()));
                    i += 1;
                    continue;
                }
                let tag: String = chars[start..j].iter().collect::<String>().to_lowercase();
                // Attributes until '>' (or '/>').
                let mut attrs: Vec<(String, Option<String>)> = Vec::new();
                let mut self_closing = false;
                while j < chars.len() && chars[j] != '>' {
                    if chars[j] == '/' && j + 1 < chars.len() && chars[j + 1] == '>' {
                        self_closing = true;
                        j += 1;
                        break;
                    }
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j >= chars.len() || chars[j] == '>' {
                        break;
                    }
                    let astart = j;
                    while j < chars.len()
                        && !chars[j].is_whitespace()
                        && chars[j] != '='
                        && chars[j] != '>'
                        && chars[j] != '/'
                    {
                        j += 1;
                    }
                    let name: String = chars[astart..j].iter().collect::<String>().to_lowercase();
                    if name.is_empty() {
                        j += 1;
                        continue;
                    }
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    let mut value: Option<String> = None;
                    if j < chars.len() && chars[j] == '=' {
                        j += 1;
                        while j < chars.len() && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
                            let quote = chars[j];
                            j += 1;
                            let vstart = j;
                            while j < chars.len() && chars[j] != quote {
                                j += 1;
                            }
                            let raw: String = chars[vstart..j].iter().collect();
                            value = Some(decode_charrefs(&raw));
                            if j < chars.len() {
                                j += 1;
                            }
                        } else {
                            let vstart = j;
                            while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '>' {
                                j += 1;
                            }
                            let raw: String = chars[vstart..j].iter().collect();
                            value = Some(decode_charrefs(&raw));
                        }
                    }
                    attrs.push((name, value));
                }
                i = if j < chars.len() { j + 1 } else { chars.len() };
                if closing {
                    // Unwind the stack to the matching open tag (it and
                    // everything above it leave the stack; the nodes
                    // themselves were already attached as children when
                    // their start tags were seen).
                    for k in (1..stack.len()).rev() {
                        if stack[k].borrow().tag == tag {
                            stack.truncate(k);
                            break;
                        }
                    }
                } else {
                    count += 1;
                    if stack.len() >= 18 || count > 800 {
                        return Err(Error::Other(
                            "The answer exceeds native structure limits".to_string(),
                        ));
                    }
                    let node = Rc::new(RefCell::new(Mutable {
                        tag: tag.clone(),
                        attrs,
                        children: Vec::new(),
                    }));
                    stack
                        .last()
                        .unwrap()
                        .borrow_mut()
                        .children
                        .push(MutableChild::Element(Rc::clone(&node)));
                    if !VOID_TAGS.contains(&tag.as_str()) && !self_closing {
                        stack.push(node);
                    }
                }
            } else {
                let start = i;
                while i < chars.len() && chars[i] != '<' {
                    i += 1;
                }
                let raw: String = chars[start..i].iter().collect();
                if !raw.is_empty() {
                    stack
                        .last()
                        .unwrap()
                        .borrow_mut()
                        .children
                        .push(MutableChild::Text(decode_charrefs(&raw)));
                }
            }
        }
        Ok(to_owned(&root))
    }
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
        ("&nbsp;", " "),
    ] {
        out = out.replace(from, to);
    }
    out
}

// ── answer text ───────────────────────────────────────────────────────────

/// Extract public content without revealing provider errors or tool protocol.
pub fn answer_text(response: &dyn AnswerSource) -> String {
    if let Some(_err) = response.error() {
        if response.provider() != "runtime" {
            return "Sorry, the AI service couldn't complete that request. Please try again."
                .to_string();
        }
        let content = response.content();
        let fallback = if content.is_empty() {
            "The agent could not complete this request."
        } else {
            content
        };
        return sanitize_error_text(&public_text(fallback));
    }
    let content = response.content();
    if content.is_empty() {
        return public_text("No response was returned.");
    }
    public_text(content)
}

/// Preserve the answer's HTML or Markdown format for plain-text extraction.
pub fn render_answer(response: &dyn AnswerSource) -> RichMarkupData {
    rich_output(&answer_text(response))
}

/// Return readable answer text without synthetic action labels.
pub fn answer_plain(response: &dyn AnswerSource) -> String {
    let markup = render_answer(response);
    crate::telegram::markup_to_plain(&markup)
}

// ── native rich model ─────────────────────────────────────────────────────

/// One inline rich-text span — the `types.Text*` family as data.
#[derive(Debug, Clone, PartialEq)]
pub enum RichText {
    Empty,
    Plain(String),
    Bold(Box<RichText>),
    Italic(Box<RichText>),
    Underline(Box<RichText>),
    Strike(Box<RichText>),
    Fixed(Box<RichText>),
    Subscript(Box<RichText>),
    Superscript(Box<RichText>),
    Spoiler(Box<RichText>),
    Url(Box<RichText>, String),
    Concat(Vec<RichText>),
}

impl RichText {
    pub fn plain_text(&self) -> String {
        match self {
            RichText::Empty => String::new(),
            RichText::Plain(s) => s.clone(),
            RichText::Bold(inner)
            | RichText::Italic(inner)
            | RichText::Underline(inner)
            | RichText::Strike(inner)
            | RichText::Fixed(inner)
            | RichText::Subscript(inner)
            | RichText::Superscript(inner)
            | RichText::Spoiler(inner) => inner.plain_text(),
            RichText::Url(inner, _) => inner.plain_text(),
            RichText::Concat(parts) => parts.iter().map(|p| p.plain_text()).collect(),
        }
    }
}

/// One native page block — the `types.PageBlock*` family as data.
#[derive(Debug, Clone, PartialEq)]
pub enum PageBlock {
    Paragraph(RichText),
    Details {
        blocks: Vec<PageBlock>,
        title: RichText,
        open: bool,
    },
    Preformatted {
        text: String,
    },
    Blockquote {
        blocks: Vec<PageBlock>,
    },
    List {
        items: Vec<Vec<PageBlock>>,
    },
    OrderedList {
        items: Vec<(String, Vec<PageBlock>)>,
    },
    Table {
        rows: Vec<Vec<TableCell>>,
    },
    Divider,
    ButtonRow {
        buttons: Vec<PageButton>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub text: RichText,
    pub header: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageButton {
    pub label: String,
    pub kind: ButtonKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ButtonKind {
    Copy(String),
    Url(String),
}

/// A complete native answer: blocks plus the action controls the
/// Python source appends. The binary crate wraps this in
/// `rich_message_blocks` / `send_rich`.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeAnswer {
    pub blocks: Vec<PageBlock>,
}

fn safe_url(value: &str) -> bool {
    // Scheme http/https, a hostname, no userinfo, and no credential-ish
    // query keys; the value must survive error sanitisation unchanged.
    let Some(after_scheme) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    if after_scheme.is_empty() {
        return false;
    }
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let query = after_scheme
        .split('?')
        .nth(1)
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("");
    for pair in query.split('&') {
        let key = pair.split('=').next().unwrap_or("").to_lowercase();
        for word in ["token", "secret", "password", "api_key", "session"] {
            if key.contains(word) {
                return false;
            }
        }
    }
    sanitize_error_text(value) == value
}

fn plain_nodes(nodes: &[NodeChild]) -> String {
    let mut out = String::new();
    for item in nodes {
        match item {
            NodeChild::Text(s) => out.push_str(s),
            NodeChild::Element(node) => out.push_str(&plain_nodes(&node.children)),
        }
    }
    out
}

fn inline_nodes(nodes: &[NodeChild]) -> RichText {
    let mut result: Vec<RichText> = Vec::new();
    for node in nodes {
        match node {
            NodeChild::Text(s) => result.push(RichText::Plain(s.clone())),
            NodeChild::Element(el) => match el.tag.as_str() {
                "script" | "style" | "img" | "iframe" => continue,
                "br" => result.push(RichText::Plain("\n".to_string())),
                "b" | "strong" => result.push(RichText::Bold(Box::new(inline_nodes(&el.children)))),
                "i" | "em" => result.push(RichText::Italic(Box::new(inline_nodes(&el.children)))),
                "u" => result.push(RichText::Underline(Box::new(inline_nodes(&el.children)))),
                "s" | "del" => result.push(RichText::Strike(Box::new(inline_nodes(&el.children)))),
                "code" => result.push(RichText::Fixed(Box::new(inline_nodes(&el.children)))),
                "sub" => result.push(RichText::Subscript(Box::new(inline_nodes(&el.children)))),
                "sup" => result.push(RichText::Superscript(Box::new(inline_nodes(&el.children)))),
                "tg-spoiler" => {
                    result.push(RichText::Spoiler(Box::new(inline_nodes(&el.children))))
                }
                "a" => {
                    let href = el.attr("href").unwrap_or("").to_string();
                    if safe_url(&href) {
                        result.push(RichText::Url(Box::new(inline_nodes(&el.children)), href));
                    } else {
                        result.push(inline_nodes(&el.children));
                    }
                }
                _ => result.push(inline_nodes(&el.children)),
            },
        }
    }
    if result.is_empty() {
        return RichText::Empty;
    }
    if result.len() == 1 {
        return result.into_iter().next().unwrap();
    }
    RichText::Concat(result)
}

fn blocks_nodes(nodes: &[NodeChild]) -> Vec<PageBlock> {
    let mut output: Vec<PageBlock> = Vec::new();
    let mut inline: Vec<NodeChild> = Vec::new();

    macro_rules! flush {
        () => {
            if !plain_nodes(&inline).trim().is_empty() {
                output.push(PageBlock::Paragraph(inline_nodes(&inline)));
            }
            inline.clear();
        };
    }

    for node in nodes {
        match node {
            NodeChild::Text(_) => {
                inline.push(node.clone());
                continue;
            }
            NodeChild::Element(el) => {
                if matches!(
                    el.tag.as_str(),
                    "script" | "style" | "img" | "iframe" | "summary"
                ) {
                    continue;
                }
                if !matches!(
                    el.tag.as_str(),
                    "html"
                        | "body"
                        | "div"
                        | "p"
                        | "section"
                        | "article"
                        | "details"
                        | "pre"
                        | "blockquote"
                        | "ul"
                        | "ol"
                        | "table"
                        | "hr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                ) {
                    inline.push(node.clone());
                    continue;
                }
                flush!();
                match el.tag.as_str() {
                    "details" => {
                        let summary = el.children.iter().find_map(|item| match item {
                            NodeChild::Element(n) if n.tag == "summary" => Some(n),
                            _ => None,
                        });
                        let title = match summary {
                            Some(s) => inline_nodes(&s.children),
                            None => RichText::Plain("Details".to_string()),
                        };
                        output.push(PageBlock::Details {
                            blocks: blocks_nodes(&el.children),
                            title,
                            open: el.has_attr("open"),
                        });
                    }
                    "pre" => {
                        output.push(PageBlock::Preformatted {
                            text: plain_nodes(&el.children),
                        });
                    }
                    "blockquote" => {
                        output.push(PageBlock::Blockquote {
                            blocks: blocks_nodes(&el.children),
                        });
                    }
                    "ul" | "ol" => {
                        let items: Vec<&Node> = el
                            .children
                            .iter()
                            .filter_map(|item| match item {
                                NodeChild::Element(n) if n.tag == "li" => Some(n),
                                _ => None,
                            })
                            .collect();
                        if el.tag == "ul" {
                            output.push(PageBlock::List {
                                items: items.iter().map(|it| blocks_nodes(&it.children)).collect(),
                            });
                        } else {
                            output.push(PageBlock::OrderedList {
                                items: items
                                    .iter()
                                    .enumerate()
                                    .map(|(index, it)| {
                                        ((index + 1).to_string(), blocks_nodes(&it.children))
                                    })
                                    .collect(),
                            });
                        }
                    }
                    "table" => {
                        fn rows<'a>(children: &'a [NodeChild], out: &mut Vec<&'a Node>) {
                            for item in children {
                                if let NodeChild::Element(n) = item {
                                    if n.tag == "tr" {
                                        out.push(n);
                                    } else if matches!(n.tag.as_str(), "thead" | "tbody" | "tfoot")
                                    {
                                        rows(&n.children, out);
                                    }
                                }
                            }
                        }
                        let mut row_nodes: Vec<&Node> = Vec::new();
                        rows(&el.children, &mut row_nodes);
                        let mut native_rows: Vec<Vec<TableCell>> = Vec::new();
                        for row in row_nodes {
                            let cells: Vec<TableCell> = row
                                .children
                                .iter()
                                .filter_map(|item| match item {
                                    NodeChild::Element(cell)
                                        if cell.tag == "td" || cell.tag == "th" =>
                                    {
                                        Some(TableCell {
                                            text: inline_nodes(&cell.children),
                                            header: cell.tag == "th",
                                        })
                                    }
                                    _ => None,
                                })
                                .collect();
                            native_rows.push(cells);
                        }
                        output.push(PageBlock::Table { rows: native_rows });
                    }
                    "hr" => output.push(PageBlock::Divider),
                    _ if el.tag.starts_with('h')
                        && el.tag[1..].chars().all(|c| c.is_ascii_digit()) =>
                    {
                        output.push(PageBlock::Paragraph(RichText::Bold(Box::new(
                            inline_nodes(&el.children),
                        ))));
                    }
                    _ => {
                        output.extend(blocks_nodes(&el.children));
                    }
                }
            }
        }
    }
    flush!();
    output
}

fn markdown_links(text: &str) -> Vec<String> {
    // Collect `[label](url)` targets the way telethon's markdown parser
    // exposes `entity.url` on link entities.
    let mut urls: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        let url = &after[..close];
        if safe_url(url) && !urls.contains(&url.to_string()) {
            urls.push(url.to_string());
        }
        rest = &after[close + 1..];
    }
    urls
}

/// Build a native answer with nested copy and safe public-source URL controls.
pub fn native_answer(response: &dyn AnswerSource) -> Result<NativeAnswer> {
    let markup = render_answer(response);
    let mut urls: Vec<String> = Vec::new();
    let mut body: Vec<PageBlock>;
    if markup.format == RichFormat::Html {
        let tree = HtmlTree::parse(&markup.text)?;
        body = blocks_nodes(&tree.children);
        fn visit(nodes: &[NodeChild], urls: &mut Vec<String>) {
            for node in nodes {
                if let NodeChild::Element(el) = node {
                    let href = el.attr("href").unwrap_or("");
                    if el.tag == "a" && safe_url(href) && !urls.contains(&href.to_string()) {
                        urls.push(href.to_string());
                    }
                    visit(&el.children, urls);
                }
            }
        }
        visit(&tree.children, &mut urls);
    } else {
        let plain = crate::telegram::strip_markdown(&markup.text);
        body = vec![PageBlock::Paragraph(RichText::Plain(plain))];
        for url in markdown_links(&markup.text) {
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    let plain = answer_plain(response);
    let mut controls: Vec<PageBlock> = Vec::new();
    // Small individual copy values keep the native controls usable on long answers.
    for start in (0..plain.len()).step_by(2048) {
        let index = start / 2048 + 1;
        let label = if plain.len() <= 2048 {
            "Copy answer".to_string()
        } else {
            format!("Copy part {index}")
        };
        let end = (start + 2048).min(plain.len());
        // Copy on a UTF-8 boundary: step_by(2048) counts bytes, so snap
        // the end back to a char boundary (Python slices str by chars).
        let mut end = end;
        while end > start && !plain.is_char_boundary(end) {
            end -= 1;
        }
        controls.push(PageBlock::ButtonRow {
            buttons: vec![PageButton {
                label,
                kind: ButtonKind::Copy(plain[start..end].to_string()),
            }],
        });
        if controls.len() == 8 {
            break;
        }
    }
    for (index, url) in urls.iter().take(4).enumerate() {
        controls.push(PageBlock::ButtonRow {
            buttons: vec![PageButton {
                label: format!("Open source {}", index + 1),
                kind: ButtonKind::Url(url.clone()),
            }],
        });
    }
    if !controls.is_empty() {
        body.push(PageBlock::Details {
            blocks: vec![PageBlock::Details {
                blocks: controls,
                title: RichText::Plain("Copy and sources".to_string()),
                open: false,
            }],
            title: RichText::Plain("Answer actions".to_string()),
            open: false,
        });
    }
    Ok(NativeAnswer { blocks: body })
}

/// JSON helpers for diagnostics (the block model is data, not TL).
pub fn native_answer_to_json(answer: &NativeAnswer) -> Value {
    fn rich_json(rt: &RichText) -> Value {
        match rt {
            RichText::Empty => serde_json::json!({"t": "empty"}),
            RichText::Plain(s) => serde_json::json!({"t": "plain", "text": s}),
            RichText::Bold(i) => serde_json::json!({"t": "bold", "inner": rich_json(i)}),
            RichText::Italic(i) => serde_json::json!({"t": "italic", "inner": rich_json(i)}),
            RichText::Underline(i) => serde_json::json!({"t": "underline", "inner": rich_json(i)}),
            RichText::Strike(i) => serde_json::json!({"t": "strike", "inner": rich_json(i)}),
            RichText::Fixed(i) => serde_json::json!({"t": "fixed", "inner": rich_json(i)}),
            RichText::Subscript(i) => serde_json::json!({"t": "sub", "inner": rich_json(i)}),
            RichText::Superscript(i) => serde_json::json!({"t": "sup", "inner": rich_json(i)}),
            RichText::Spoiler(i) => serde_json::json!({"t": "spoiler", "inner": rich_json(i)}),
            RichText::Url(i, u) => serde_json::json!({"t": "url", "inner": rich_json(i), "url": u}),
            RichText::Concat(parts) => {
                serde_json::json!({"t": "concat", "parts": parts.iter().map(rich_json).collect::<Vec<_>>()})
            }
        }
    }
    fn block_json(b: &PageBlock) -> Value {
        match b {
            PageBlock::Paragraph(rt) => {
                serde_json::json!({"t": "paragraph", "text": rich_json(rt)})
            }
            PageBlock::Details {
                blocks,
                title,
                open,
            } => {
                serde_json::json!({"t": "details", "title": rich_json(title), "open": open, "blocks": blocks.iter().map(block_json).collect::<Vec<_>>()})
            }
            PageBlock::Preformatted { text } => serde_json::json!({"t": "pre", "text": text}),
            PageBlock::Blockquote { blocks } => {
                serde_json::json!({"t": "blockquote", "blocks": blocks.iter().map(block_json).collect::<Vec<_>>()})
            }
            PageBlock::List { items } => {
                serde_json::json!({"t": "list", "items": items.iter().map(|it| it.iter().map(block_json).collect::<Vec<_>>()).collect::<Vec<_>>()})
            }
            PageBlock::OrderedList { items } => {
                let rendered: Vec<Value> = items
                    .iter()
                    .map(|(n, it)| {
                        json!({"num": n, "blocks": it.iter().map(block_json).collect::<Vec<_>>()})
                    })
                    .collect();
                json!({"t": "olist", "items": rendered})
            }
            PageBlock::Table { rows } => {
                serde_json::json!({"t": "table", "rows": rows.iter().map(|r| r.iter().map(|c| serde_json::json!({"text": rich_json(&c.text), "header": c.header})).collect::<Vec<_>>()).collect::<Vec<_>>()})
            }
            PageBlock::Divider => serde_json::json!({"t": "divider"}),
            PageBlock::ButtonRow { buttons } => {
                serde_json::json!({"t": "button_row", "buttons": buttons.iter().map(|b| serde_json::json!({"label": b.label, "kind": format!("{:?}", b.kind)})).collect::<Vec<_>>()})
            }
        }
    }
    serde_json::json!({"blocks": answer.blocks.iter().map(block_json).collect::<Vec<_>>()})
}
