//! Feature toggles — port of `pyUltroid/_core/toggles.py` (`ToggleRegistry`).

use crate::db::Database;
use crate::error::Result;
use std::sync::Arc;

#[derive(Clone)]
pub struct ToggleRegistry {
    db: Arc<dyn Database>,
}

impl ToggleRegistry {
    pub fn new(db: Arc<dyn Database>) -> Self {
        ToggleRegistry { db }
    }

    async fn key(name: &str) -> String {
        format!("toggle:{name}")
    }

    pub async fn is_on(&self, name: &str) -> Result<bool> {
        Ok(self.db.get(&Self::key(name).await).await?.as_deref() == Some("1"))
    }

    pub async fn set(&self, name: &str, on: bool) -> Result<()> {
        self.db.set(&Self::key(name).await, if on { "1" } else { "0" }).await
    }
}
