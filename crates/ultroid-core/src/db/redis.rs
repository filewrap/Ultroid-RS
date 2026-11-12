//! Redis backend — port of Python's `RedisDB` (redis-py).
//! Real RESP client via the `redis` crate (tokio multiplexed connection).
//! Feature-gated: `ultroid-core/redis`.

use super::Database;
use crate::error::{Error, Result};
use async_trait::async_trait;
use redis::AsyncCommands;

pub struct RedisDb {
    client: redis::Client,
}

impl RedisDb {
    pub async fn connect(uri: &str, password: Option<&str>) -> Result<Self> {
        let mut url = uri.to_string();
        if let Some(pw) = password {
            // redis://host:port -> redis://:pw@host:port (only when no auth present)
            if !url.contains('@') {
                if let Some(rest) = url.strip_prefix("redis://") {
                    url = format!("redis://:{pw}@{rest}");
                }
            }
        }
        let client = redis::Client::open(url).map_err(|e| Error::Database(e.to_string()))?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Database(format!("redis connect: {e}")))?;
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Database(format!("redis ping: {e}")))?;
        Ok(RedisDb { client })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Database(e.to_string()))
    }
}

#[async_trait]
impl Database for RedisDb {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let mut c = self.conn().await?;
        let v: Option<String> = c.get(key).await.map_err(|e| Error::Database(e.to_string()))?;
        Ok(v)
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        let mut c = self.conn().await?;
        let _: () = c.set(key, value).await.map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<()> {
        let mut c = self.conn().await?;
        let _: () = c.del(key).await.map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let mut c = self.conn().await?;
        let v: Vec<String> = c
            .keys(format!("{prefix}*"))
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(v)
    }
}
