//! Postgres backend — port of Python's `SqlDB` (psycopg2).
//! Key/value surface over table `ultroid_kv(key TEXT PRIMARY KEY, value TEXT)`.
//! Feature-gated: `ultroid-core/postgres`.

use super::Database;
use crate::error::{Error, Result};
use async_trait::async_trait;
use tokio_postgres::{Client, NoTls};

pub struct PostgresDb {
    client: tokio::sync::Mutex<Client>,
}

impl PostgresDb {
    pub async fn connect(url: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .map_err(|e| Error::Database(format!("postgres connect: {e}")))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS ultroid_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            )
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(PostgresDb {
            client: tokio::sync::Mutex::new(client),
        })
    }
}

#[async_trait]
impl Database for PostgresDb {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let c = self.client.lock().await;
        let row = c
            .query_opt("SELECT value FROM ultroid_kv WHERE key = $1", &[&key])
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        let c = self.client.lock().await;
        c.execute(
            "INSERT INTO ultroid_kv(key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[&key, &value],
        )
        .await
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<()> {
        let c = self.client.lock().await;
        c.execute("DELETE FROM ultroid_kv WHERE key = $1", &[&key])
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let c = self.client.lock().await;
        let rows = c
            .query("SELECT key FROM ultroid_kv WHERE key LIKE $1", &[&format!("{prefix}%")])
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
    }
}
