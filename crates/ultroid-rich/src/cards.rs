//! Rich "cards" — structured message builders.
//! Port of `pyUltroid/_core/cards.py`: a card is a titled block with
//! key/value rows and an optional footer, rendered as a `RichDoc`
//! (markdown form) with a plain-text twin.

use ultroid_core::rich::RichDoc;
use ultroid_core::Result;

/// A structured card: title, key/value rows, optional footer lines.
#[derive(Debug, Clone, Default)]
pub struct Card {
    pub title: String,
    pub rows: Vec<(String, String)>,
    pub footer: Vec<String>,
}

impl Card {
    pub fn new(title: impl Into<String>) -> Self {
        Card {
            title: title.into(),
            rows: Vec::new(),
            footer: Vec::new(),
        }
    }

    pub fn row(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.rows.push((key.into(), value.into()));
        self
    }

    pub fn foot(mut self, line: impl Into<String>) -> Self {
        self.footer.push(line.into());
        self
    }

    /// Render as markdown rich (headings + bold keys).
    pub fn to_rich(&self) -> Result<RichDoc> {
        let mut md = format!("## {}\n\n", self.title);
        for (k, v) in &self.rows {
            md.push_str(&format!("**{k}:** {v}\n"));
        }
        for line in &self.footer {
            md.push_str(&format!("\n{line}"));
        }
        RichDoc::markdown(md)
    }

    /// Plain-text twin (same content, no markers).
    pub fn to_plain(&self) -> String {
        let mut out = format!("{}\n", self.title);
        for (k, v) in &self.rows {
            out.push_str(&format!("{k}: {v}\n"));
        }
        for line in &self.footer {
            out.push_str(&format!("{line}\n"));
        }
        out
    }
}
