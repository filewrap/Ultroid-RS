//! `Responder` over a grammers `Client`.
//!
//! One responder is built per incoming update, bound to that update's
//! peer. The user client and the bot client share this implementation;
//! the only difference is `send_rich`: the bot client sends real rich
//! messages through [`ultroid_rich::RichSender`], the user client refuses
//! with [`Error::RichOnlyBot`] (the `Ctx::send_rich_or_plain` fallback
//! turns that refusal into a plain-text send, exactly like Python).

use async_trait::async_trait;
use grammers_client::{
    Client,
    message::InputMessage,
    session::types::{PeerAuth, PeerRef},
};
use std::path::Path;
use ultroid_core::{ClientKind, Error, Responder, Result, rich::RichDoc};

fn telegram<E: std::fmt::Display>(e: E) -> Error {
    Error::Telegram(e.to_string())
}

/// A responder bound to one chat peer on one client.
pub struct GrammersResponder {
    client: Client,
    peer: PeerRef,
    chat_id: i64,
    kind: ClientKind,
}

impl GrammersResponder {
    pub fn new(client: Client, peer: PeerRef, chat_id: i64, kind: ClientKind) -> Self {
        Self {
            client,
            peer,
            chat_id,
            kind,
        }
    }

    /// Build a responder for a peer id when no cached `PeerRef` exists.
    /// Uses ambient authority: the peer must already be known to the
    /// session cache (dialogs are warmed after login for this reason).
    pub fn from_peer_id(
        client: Client,
        peer_id: grammers_client::session::types::PeerId,
        chat_id: i64,
        kind: ClientKind,
    ) -> Self {
        Self {
            client,
            peer: PeerRef {
                id: peer_id,
                auth: PeerAuth::default(),
            },
            chat_id,
            kind,
        }
    }
}

#[async_trait]
impl Responder for GrammersResponder {
    async fn reply(&self, text: &str) -> Result<i64> {
        let msg = self
            .client
            .send_message(self.peer, InputMessage::new().text(text))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn reply_html(&self, html: &str) -> Result<i64> {
        let msg = self
            .client
            .send_message(self.peer, InputMessage::new().html(html))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn edit(&self, msg_id: i64, text: &str) -> Result<()> {
        self.client
            .edit_message(self.peer, msg_id as i32, InputMessage::new().text(text))
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn delete(&self, msg_id: i64) -> Result<()> {
        self.client
            .delete_messages(self.peer, &[msg_id as i32])
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn send_file(&self, path: &Path, caption: &str) -> Result<i64> {
        let uploaded = self.client.upload_file(path).await.map_err(telegram)?;
        let msg = self
            .client
            .send_message(self.peer, InputMessage::new().text(caption).file(uploaded))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_rich(&self, doc: &RichDoc) -> Result<i64> {
        match self.kind {
            ClientKind::AsstBot => {
                let sender = ultroid_rich::RichSender::new_bot(self.client.clone());
                let sent = sender.send_rich(self.peer, doc).await.map_err(|e| {
                    // A failed rich send must keep the real server reason
                    // (and never silently fall back to plain text here —
                    // the caller decides the fallback).
                    Error::Telegram(e.to_string())
                })?;
                Ok(sent.id() as i64)
            }
            _ => Err(Error::RichOnlyBot),
        }
    }

    fn chat_id(&self) -> i64 {
        self.chat_id
    }
}
