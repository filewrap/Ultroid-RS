//! `listen()` — port of `pyUltroid/startup/listen.py` + `listen_resolve.py`
//! and `register_listen` in `pyUltroid/startup/dispatcher.py`.
//!
//! grammers has no conversation/`listen()` helper, so waiters are built on
//! the update stream: a waiter registers for (chat_id, optional sender_id)
//! and receives the next matching message text, with timeout.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};

#[derive(Debug, Clone)]
pub struct ListenedMessage {
    pub chat_id: i64,
    pub sender_id: i64,
    pub message_id: i64,
    pub text: String,
    pub reply_to_id: Option<i64>,
}

struct Waiter {
    sender_id: Option<i64>,
    tx: oneshot::Sender<ListenedMessage>,
}

/// Per-chat waiters. The binary's update loop calls `deliver` for every
/// incoming message; `wait_for` is used by handlers implementing `listen()`.
#[derive(Default, Clone)]
pub struct WaiterHub {
    inner: Arc<Mutex<HashMap<i64, Vec<Waiter>>>>,
}

impl WaiterHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait for the next message in `chat_id` (optionally from `sender_id`).
    /// Returns `None` on timeout. `once` waiters are removed after delivery.
    pub async fn wait_for(
        &self,
        chat_id: i64,
        sender_id: Option<i64>,
        timeout: std::time::Duration,
    ) -> Option<ListenedMessage> {
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.inner.lock().await;
            map.entry(chat_id).or_default().push(Waiter { sender_id, tx });
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(msg)) => Some(msg),
            _ => None,
        }
    }

    /// Deliver a message to matching waiters (removes them).
    pub async fn deliver(&self, msg: ListenedMessage) {
        let mut map = self.inner.lock().await;
        if let Some(waiters) = map.get_mut(&msg.chat_id) {
            let mut remaining = Vec::new();
            for w in waiters.drain(..) {
                if w.sender_id.map_or(true, |s| s == msg.sender_id) {
                    let _ = w.tx.send(msg.clone());
                } else {
                    remaining.push(w);
                }
            }
            *waiters = remaining;
        }
    }
}
