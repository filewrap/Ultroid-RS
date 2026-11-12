//! VC persistence. Port of `vc/_database.py` (`VcDatabase` over udB).
//!
//! Keys: `vc:assistant:{chat_id}` (assistant account id for the chat),
//! `vc:channel:{chat_id}` (channel-play binding).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultroid_core::db::{Database, db_get_json, db_set_json};

/// Per-chat assistant assignment + channel binding.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VcChatSettings {
    pub assistant_id: Option<i64>,
    pub channel_chat_id: Option<i64>,
}

pub struct VcDatabase {
    db: Arc<dyn Database>,
    cache: Mutex<HashMap<i64, VcChatSettings>>,
}

impl VcDatabase {
    pub fn new(db: Arc<dyn Database>) -> Self {
        VcDatabase {
            db,
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn load(&self, chat_id: i64) -> VcChatSettings {
        if let Some(s) = self.cache.lock().await.get(&chat_id) {
            return s.clone();
        }
        let settings = db_get_json::<VcChatSettings>(&*self.db, &format!("vc:assistant:{chat_id}"))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.cache.lock().await.insert(chat_id, settings.clone());
        settings
    }

    async fn store(&self, chat_id: i64, settings: &VcChatSettings) {
        let _ = db_set_json(&*self.db, &format!("vc:assistant:{chat_id}"), settings).await;
        self.cache.lock().await.insert(chat_id, settings.clone());
    }

    pub async fn get_assistant(&self, chat_id: i64) -> Option<i64> {
        self.load(chat_id).await.assistant_id
    }

    pub async fn set_assistant(&self, chat_id: i64, assistant_id: Option<i64>) {
        let mut s = self.load(chat_id).await;
        s.assistant_id = assistant_id;
        self.store(chat_id, &s).await;
    }

    pub async fn get_channel(&self, chat_id: i64) -> Option<i64> {
        self.load(chat_id).await.channel_chat_id
    }

    pub async fn set_channel(&self, chat_id: i64, channel_id: Option<i64>) {
        let mut s = self.load(chat_id).await;
        s.channel_chat_id = channel_id;
        self.store(chat_id, &s).await;
    }
}
