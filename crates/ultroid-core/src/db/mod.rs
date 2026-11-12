//! Database layer — port of `pyUltroid/startup/_database.py` and `pyUltroid/dB/`.
//!
//! Backend priority (same as Python): Redis > Mongo > Postgres > local JSON.
//! The trait surface mirrors Python's `_BaseDatabase` key/value API.

use crate::config::Config;
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::sync::Arc;

pub mod features;
pub mod local;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(feature = "mongo")]
pub mod mongo;
#[cfg(feature = "postgres")]
pub mod postgres;

#[async_trait]
pub trait Database: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn del(&self, key: &str) -> Result<()>;
    async fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key).await?.is_some())
    }
    async fn keys(&self, prefix: &str) -> Result<Vec<String>>;
}

/// Typed JSON helpers over any `dyn Database`. These are free functions —
/// generic methods would make the trait non-dyn-compatible (E0038).
pub async fn db_get_json<T: serde::de::DeserializeOwned + Send>(
    db: &dyn Database,
    key: &str,
) -> Result<Option<T>> {
    match db.get(key).await? {
        Some(v) => Ok(Some(
            serde_json::from_str(&v).map_err(|e| Error::Database(e.to_string()))?,
        )),
        None => Ok(None),
    }
}

pub async fn db_set_json<T: serde::Serialize + Send + Sync>(
    db: &dyn Database,
    key: &str,
    value: &T,
) -> Result<()> {
    let v = serde_json::to_string(value).map_err(|e| Error::Database(e.to_string()))?;
    db.set(key, &v).await
}

/// Open the configured backend. Mirrors the Python priority order.
/// If a URI is set but its feature is not compiled in, this errors naming
/// the feature — it never silently falls back to the local file DB.
pub async fn open_database(cfg: &Config) -> Result<Arc<dyn Database>> {
    if let Some(uri) = &cfg.redis_uri {
        #[cfg(feature = "redis")]
        {
            return Ok(Arc::new(redis::RedisDb::connect(uri, cfg.redis_password.as_deref()).await?));
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = uri;
            return Err(Error::Database(
                "REDIS_URI is set but the `redis` feature is not compiled in; rebuild with --features redis".into(),
            ));
        }
    }
    if let Some(uri) = &cfg.mongo_uri {
        #[cfg(feature = "mongo")]
        {
            return Ok(Arc::new(mongo::MongoDb::connect(uri).await?));
        }
        #[cfg(not(feature = "mongo"))]
        {
            let _ = uri;
            return Err(Error::Database(
                "MONGO_URI is set but the `mongo` feature is not compiled in; rebuild with --features mongo".into(),
            ));
        }
    }
    if let Some(url) = &cfg.database_url {
        #[cfg(feature = "postgres")]
        {
            return Ok(Arc::new(postgres::PostgresDb::connect(url).await?));
        }
        #[cfg(not(feature = "postgres"))]
        {
            let _ = url;
            return Err(Error::Database(
                "DATABASE_URL is set but the `postgres` feature is not compiled in; rebuild with --features postgres".into(),
            ));
        }
    }
    let path = std::path::Path::new(&cfg.data_dir).join("ultroid.localdb.json");
    Ok(Arc::new(local::LocalJsonDb::open(path)?))
}
