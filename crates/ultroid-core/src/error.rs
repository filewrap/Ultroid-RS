//! Error taxonomy for Ultroid-RS.
//! Port of `pyUltroid/exceptions.py` — one error type for the whole workspace.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("database: {0}")]
    Database(String),
    #[error("telegram: {0}")]
    Telegram(String),
    #[error("rich messages are only available on the bot client")]
    RichOnlyBot,
    #[error("permission denied: {0}")]
    Denied(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("voice chat audio engine unavailable: {0}")]
    VcAudioUnavailable(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Other(format!("regex: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
