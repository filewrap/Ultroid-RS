//! Telegram Stars + Stories core — port of `pyUltroid/_core/stars.py`
//! and `pyUltroid/_core/stories.py` (state helpers; the actual API calls
//! are raw TL invokes in the binary).

use crate::db::Database;
use crate::error::Result;
use std::sync::Arc;

#[derive(Clone)]
pub struct StarsStore {
    db: Arc<dyn Database>,
}

impl StarsStore {
    pub fn new(db: Arc<dyn Database>) -> Self {
        StarsStore { db }
    }

    pub async fn balance(&self) -> Result<i64> {
        Ok(self
            .db
            .get("stars:balance")
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    pub async fn set_balance(&self, amount: i64) -> Result<()> {
        self.db.set("stars:balance", &amount.to_string()).await
    }
}

#[derive(Clone)]
pub struct StoriesStore {
    db: Arc<dyn Database>,
}

impl StoriesStore {
    pub fn new(db: Arc<dyn Database>) -> Self {
        StoriesStore { db }
    }

    pub async fn last_story_id(&self, chat_id: i64) -> Result<Option<i64>> {
        Ok(self
            .db
            .get(&format!("story:last:{chat_id}"))
            .await?
            .and_then(|v| v.parse().ok()))
    }

    pub async fn set_last_story_id(&self, chat_id: i64, story_id: i64) -> Result<()> {
        self.db
            .set(&format!("story:last:{chat_id}"), &story_id.to_string())
            .await
    }
}
