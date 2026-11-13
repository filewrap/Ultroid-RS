//! `Responder` over a grammers `Client`.
//!
//! One responder is built per incoming update, bound to that update's
//! peer. The user client and the bot client share this implementation;
//! the only difference is `send_rich`: the bot client sends real rich
//! messages through [`ultroid_rich::RichSender`], the user client refuses
//! with [`Error::RichOnlyBot`] (the `Ctx::send_rich_or_plain` fallback
//! turns that refusal into a plain-text send, exactly like Python).

use grammers_client::{
    Client,
    session::types::{PeerAuth, PeerRef},
};
use ultroid_core::{ClientKind, Error};

pub(crate) fn telegram<E: std::fmt::Display>(e: E) -> Error {
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
    /// Intentional API for later responder wiring; currently uncalled.
    #[allow(dead_code)]
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

    /// The underlying client (for the extended ops in `responder_ops.rs`).
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    /// The peer this responder targets.
    pub(crate) fn peer(&self) -> PeerRef {
        self.peer
    }

    /// Which client kind this responder runs on.
    pub(crate) fn kind(&self) -> ClientKind {
        self.kind
    }

    /// The chat id this responder targets.
    pub(crate) fn chat_id_value(&self) -> i64 {
        self.chat_id
    }
}
