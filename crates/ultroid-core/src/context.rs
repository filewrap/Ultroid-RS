//! Handler context + responder trait — the per-update object plugins see.
//! Port of the Telethon event + `ultroid_cmd` wrapper surface.

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::rich::RichDoc;
use crate::strings::Strings;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatType {
    Private,
    Group,
    Supergroup,
    Channel,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// The user's own account (ultroid_bot).
    User,
    /// The assistant bot (asst) — the ONLY client that may send rich.
    AsstBot,
    /// Extra user accounts (SESSION2/3) or the VC account.
    ExtraUser,
}

/// What a handler can do back to Telegram. Implemented in the binary crate
/// over grammers `Client`s; the user-client implementation MUST refuse
/// `send_rich` with `Error::RichOnlyBot`.
#[async_trait]
pub trait Responder: Send + Sync {
    async fn reply(&self, text: &str) -> Result<i64>;
    async fn reply_html(&self, html: &str) -> Result<i64>;
    async fn edit(&self, msg_id: i64, text: &str) -> Result<()>;
    async fn delete(&self, msg_id: i64) -> Result<()>;
    async fn send_file(&self, path: &Path, caption: &str) -> Result<i64>;
    async fn send_rich(&self, doc: &RichDoc) -> Result<i64>;
    /// Chat id this responder targets.
    fn chat_id(&self) -> i64;
}

/// The context handed to every command handler.
pub struct Ctx {
    pub text: String,
    /// Text after the matched command (Python's `event.pattern_match.group(1)`
    /// equivalent for the common `cmd( (.*)|$)` shape; empty when none).
    pub args: String,
    /// All regex capture groups (group 0 is the whole match).
    pub captures: Vec<String>,
    /// Name of the matched command (`CommandDef.name`).
    pub command: &'static str,
    pub chat_id: i64,
    pub chat_type: ChatType,
    pub sender_id: i64,
    pub message_id: i64,
    pub reply_to_id: Option<i64>,
    pub reply_to_text: Option<String>,
    pub is_owner: bool,
    pub is_sudo: bool,
    pub is_dev: bool,
    pub is_guest: bool,
    pub via_bot: bool,
    pub db: Arc<dyn Database>,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub responder: Arc<dyn Responder>,
    pub lang: String,
    pub strings: Arc<Strings>,
    pub client_kind: ClientKind,
}

impl Ctx {
    pub async fn reply(&self, text: &str) -> Result<i64> {
        self.responder.reply(text).await
    }

    pub async fn reply_html(&self, html: &str) -> Result<i64> {
        self.responder.reply_html(html).await
    }

    pub async fn edit(&self, msg_id: i64, text: &str) -> Result<()> {
        self.responder.edit(msg_id, text).await
    }

    pub async fn delete(&self, msg_id: i64) -> Result<()> {
        self.responder.delete(msg_id).await
    }

    pub async fn send_file(&self, path: &Path, caption: &str) -> Result<i64> {
        self.responder.send_file(path, caption).await
    }

    /// Rich send — refuses on non-bot clients (`Error::RichOnlyBot`).
    pub async fn send_rich(&self, doc: &RichDoc) -> Result<i64> {
        self.responder.send_rich(doc).await
    }

    /// Rich send with explicit plain-text fallback on bot clients, and a
    /// plain-text send on user clients (the Python fallback behaviour).
    pub async fn send_rich_or_plain(&self, doc: &RichDoc) -> Result<i64> {
        match self.send_rich(doc).await {
            Ok(id) => Ok(id),
            Err(Error::RichOnlyBot) => self.reply(&doc.plain_text()).await,
            Err(e) => Err(e),
        }
    }

    pub fn get_str(&self, key: &str) -> String {
        self.strings.get(key)
    }

    /// First word of `args`, lowercased, for sub-command dispatch.
    pub fn subcommand(&self) -> &str {
        self.args.split_whitespace().next().unwrap_or("")
    }
}
