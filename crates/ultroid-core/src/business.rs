//! Business mode + guest mode — port of `pyUltroid/startup/business.py`,
//! `pyUltroid/startup/guestmode.py`, `pyUltroid/_core/business_profile.py`.
//!
//! Business mode wraps bot sends in `InvokeWithBusinessConnectionRequest`
//! (raw TL); guest mode gates which commands admitted guests may run.

use crate::db::Database;
use crate::error::Result;
use std::sync::Arc;

/// Guest-mode state (Python `is_guest` / `guest_db`).
#[derive(Clone)]
pub struct GuestMode {
    db: Arc<dyn Database>,
}

impl GuestMode {
    pub fn new(db: Arc<dyn Database>) -> Self {
        GuestMode { db }
    }

    pub async fn is_guest(&self, user_id: i64) -> Result<bool> {
        Ok(self
            .db
            .get(&format!("guest:{user_id}"))
            .await?
            .as_deref()
            == Some("1"))
    }

    pub async fn add_guest(&self, user_id: i64) -> Result<()> {
        self.db.set(&format!("guest:{user_id}"), "1").await
    }

    pub async fn remove_guest(&self, user_id: i64) -> Result<()> {
        self.db.del(&format!("guest:{user_id}")).await
    }

    pub async fn list_guests(&self) -> Result<Vec<i64>> {
        let keys = self.db.keys("guest:").await?;
        Ok(keys
            .iter()
            .filter_map(|k| k.strip_prefix("guest:")?.parse::<i64>().ok())
            .collect())
    }
}

/// Business connection id for a chat (set by the bot owner in BotFather
/// business settings). Raw-TL wrapping happens in the binary's responder.
#[derive(Clone)]
pub struct BusinessMode {
    db: Arc<dyn Database>,
}

impl BusinessMode {
    pub fn new(db: Arc<dyn Database>) -> Self {
        BusinessMode { db }
    }

    pub async fn connection_for(&self, chat_id: i64) -> Result<Option<String>> {
        self.db.get(&format!("business:{chat_id}")).await
    }

    pub async fn set_connection(&self, chat_id: i64, connection_id: &str) -> Result<()> {
        self.db.set(&format!("business:{chat_id}"), connection_id).await
    }
}
