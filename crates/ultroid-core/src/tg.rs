//! Inline buttons / keyboards — port of `custom/tg.py` (`ButtonKit.ikm`
//! / `rkm`) and `pyUltroid/_core/buttonkit.py`.
//!
//! Data-only builders: rows of buttons with styles. The binary converts
//! these into grammers `Button`/`ReplyMarkup`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonStyle {
    Default,
    Primary,
    Success,
    Danger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ButtonAction {
    /// Callback data (max 64 bytes on the wire).
    Data(Vec<u8>),
    Url(String),
    SwitchInline(String),
    SwitchInlineElsewhere(String),
    WebApp(String),
    Copy(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineButton {
    pub text: String,
    pub action: ButtonAction,
    #[serde(default)]
    pub style: ButtonStyle,
}

impl Default for ButtonStyle {
    fn default() -> Self {
        ButtonStyle::Default
    }
}

impl InlineButton {
    pub fn data(text: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        InlineButton {
            text: text.into(),
            action: ButtonAction::Data(data.into()),
            style: ButtonStyle::Default,
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        InlineButton {
            text: text.into(),
            action: ButtonAction::Url(url.into()),
            style: ButtonStyle::Default,
        }
    }

    pub fn web_app(text: impl Into<String>, url: impl Into<String>) -> Self {
        InlineButton {
            text: text.into(),
            action: ButtonAction::WebApp(url.into()),
            style: ButtonStyle::Default,
        }
    }

    /// Switch to an inline query in the same chat (Telethon
    /// `Button.switch_inline(..., same_peer=True)`).
    pub fn switch_inline(text: impl Into<String>, query: impl Into<String>) -> Self {
        InlineButton {
            text: text.into(),
            action: ButtonAction::SwitchInline(query.into()),
            style: ButtonStyle::Default,
        }
    }

    /// Switch to an inline query in another chat.
    pub fn switch_inline_elsewhere(text: impl Into<String>, query: impl Into<String>) -> Self {
        InlineButton {
            text: text.into(),
            action: ButtonAction::SwitchInlineElsewhere(query.into()),
            style: ButtonStyle::Default,
        }
    }

    pub fn styled(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }
}

/// Build a keyboard from rows. Mirrors `ikm(data)` where `data` is a list of
/// rows of `(text, data)` tuples.
pub fn ikm(rows: Vec<Vec<InlineButton>>) -> Vec<Vec<InlineButton>> {
    rows
}

/// Keyboard from `(text, callback_data)` tuples, one row per tuple.
pub fn ikm_col(pairs: Vec<(&str, &str)>) -> Vec<Vec<InlineButton>> {
    pairs
        .into_iter()
        .map(|(t, d)| vec![InlineButton::data(t, d.as_bytes().to_vec())])
        .collect()
}
