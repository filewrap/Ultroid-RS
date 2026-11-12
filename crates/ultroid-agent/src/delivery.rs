//! Rooted agent delivery with native nested actions and complete long
//! answers — port of `pyUltroid/agent/_delivery.py`.
//!
//! Telegram sending is abstracted behind [`DeliverySink`]: this module
//! must not depend on grammers directly (that wiring lives in the
//! binary crate later). Rich sending/editing goes through the sink too,
//! because the RichMessage construction lives in the binary crate.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use ultroid_core::error::{Error, Result};

use crate::presentation::{
    AnswerSource, NativeAnswer, SimpleAnswer, answer_plain, answer_text, native_answer,
};

/// Identify the failed delivery phase without exposing underlying credentials.
#[derive(Debug, Clone)]
pub struct DeliveryError {
    pub phase: String,
    pub cause: String,
}

impl DeliveryError {
    pub fn new(phase: &str, cause: impl fmt::Display) -> Self {
        DeliveryError {
            phase: phase.to_string(),
            cause: cause.to_string(),
        }
    }

    /// Return the underlying error for sanitized diagnostic handling.
    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "delivery failed at phase {:?}", self.phase)
    }
}

impl std::error::Error for DeliveryError {}

impl From<DeliveryError> for Error {
    fn from(e: DeliveryError) -> Self {
        Error::Other(e.to_string())
    }
}

/// The minimal Telegram surface delivery needs. The binary crate
/// implements this against its real client; message ids stand in for
/// the Telethon `Message` objects Python returns.
#[async_trait]
pub trait DeliverySink: Send + Sync {
    /// Send plain text to *chat_id*, replying to *reply_to* when given.
    /// Returns the sent message id.
    async fn send_message(&self, chat_id: i64, text: &str, reply_to: Option<i64>) -> Result<i64>;
    /// Edit a plain-text message.
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()>;
    /// Send a file (bytes + filename) as a document.
    async fn send_file(
        &self,
        chat_id: i64,
        bytes: &[u8],
        filename: &str,
        reply_to: Option<i64>,
        caption: Option<&str>,
    ) -> Result<i64>;
    /// Whether this client can send native RichMessages (bot account or
    /// confirmed Telegram Premium user).
    async fn can_send_rich(&self) -> Result<bool>;
    /// Send a native rich answer. Returns the sent message id.
    async fn send_rich(
        &self,
        chat_id: i64,
        answer: &NativeAnswer,
        reply_to: Option<i64>,
    ) -> Result<i64>;
    /// Edit a message into a native rich answer.
    async fn edit_rich(&self, chat_id: i64, message_id: i64, answer: &NativeAnswer) -> Result<()>;
}

/// Edit one placeholder in its original thread without duplicate
/// fallback replies.
pub struct DeliveryStrategy {
    sink: Arc<dyn DeliverySink>,
    chat_id: i64,
    trigger_message_id: i64,
    root: Option<i64>,
}

impl DeliveryStrategy {
    /// *trigger_message_id* is the id of the event that started the
    /// turn; *anchor* is the persistent session root (DM session card),
    /// when the turn belongs to one.
    pub fn new(
        sink: Arc<dyn DeliverySink>,
        chat_id: i64,
        trigger_message_id: i64,
        anchor: Option<i64>,
    ) -> Self {
        DeliveryStrategy {
            sink,
            chat_id,
            trigger_message_id,
            root: anchor,
        }
    }

    /// Return the originating event's message id.
    pub fn trigger_message_id(&self) -> i64 {
        self.trigger_message_id
    }

    /// Return the persistent session root or the initiating event.
    pub fn reply_to(&self) -> i64 {
        self.root.unwrap_or(self.trigger_message_id)
    }

    /// Create a placeholder replying directly to the session's anchor.
    pub async fn placeholder(&self) -> std::result::Result<i64, DeliveryError> {
        self.sink
            .send_message(self.chat_id, "…", Some(self.reply_to()))
            .await
            .map_err(|e| DeliveryError::new("placeholder", e))
    }

    async fn attach(
        &self,
        status: Option<i64>,
        response: &dyn AnswerSource,
    ) -> std::result::Result<i64, DeliveryError> {
        let bytes = answer_text(response).into_bytes();
        self.sink
            .send_file(
                self.chat_id,
                &bytes,
                "answer.txt",
                Some(self.reply_to()),
                None,
            )
            .await
            .map_err(|e| DeliveryError::new("attachment", e))?;
        let preview = SimpleAnswer::text(format!(
            "The complete answer is attached as answer.txt.\n\nPreview:\n{}",
            answer_plain(response)
                .chars()
                .take(1500)
                .collect::<String>()
        ));
        // `render` ↔ `attach` recurse (long answers attach a file, then
        // render a preview). The recursive call is boxed so the future
        // has a finite size (E0733).
        Box::pin(self.render(status, &preview)).await
    }

    /// Replace the placeholder with a native result or a complete answer attachment.
    pub async fn render(
        &self,
        status: Option<i64>,
        response: &dyn AnswerSource,
    ) -> std::result::Result<i64, DeliveryError> {
        let rich_allowed = self.sink.can_send_rich().await.unwrap_or(false);
        let plain = answer_plain(response);
        let exceeds_budget = if rich_allowed {
            answer_text(response).len() > 12000
        } else {
            plain.encode_utf16().count() > 4000
        };
        if exceeds_budget {
            return self.attach(status, response).await;
        }
        if !rich_allowed {
            if status.is_none() {
                return self
                    .sink
                    .send_message(self.chat_id, &plain, Some(self.trigger_message_id))
                    .await
                    .map_err(|e| DeliveryError::new("fallback", e));
            }
            let status_id = status.unwrap();
            return self
                .sink
                .edit_message(self.chat_id, status_id, &plain)
                .await
                .map(|_| status_id)
                .map_err(|e| DeliveryError::new("plain_edit", e));
        }
        let payload = match native_answer(response) {
            Ok(p) => p,
            // Native nesting/block/text budgets cannot justify losing the answer.
            Err(_) => return self.attach(status, response).await,
        };
        if status.is_none() {
            return self.reply(response).await;
        }
        let status_id = status.unwrap();
        self.sink
            .edit_rich(self.chat_id, status_id, &payload)
            .await
            .map(|_| status_id)
            .map_err(|e| DeliveryError::new("rich_edit", e))
    }

    /// Send a fresh native result rooted in the same session or trigger.
    pub async fn reply(
        &self,
        response: &dyn AnswerSource,
    ) -> std::result::Result<i64, DeliveryError> {
        let rich_allowed = self.sink.can_send_rich().await.unwrap_or(false);
        if rich_allowed {
            // `native_answer` failing here is a delivery failure, not a
            // reason to downgrade silently (the `render` path already
            // attaches when the native answer cannot be built).
            let payload = native_answer(response).map_err(|e| DeliveryError::new("fallback", e))?;
            return self
                .sink
                .send_rich(self.chat_id, &payload, Some(self.reply_to()))
                .await
                .map_err(|e| DeliveryError::new("fallback", e));
        }
        self.sink
            .send_message(self.chat_id, &answer_plain(response), Some(self.reply_to()))
            .await
            .map_err(|e| DeliveryError::new("fallback", e))
    }
}
