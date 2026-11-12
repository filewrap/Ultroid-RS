//! MongoDB backend — port of Python's `MongoDB` (pymongo).
//! Key/value surface over one collection (`ultroid.kv`, `{key, value}` docs).
//! Feature-gated: `ultroid-core/mongo`.

use super::Database;
use crate::error::{Error, Result};
use async_trait::async_trait;
use mongodb::bson::{doc, Document};
use mongodb::{Client, Collection};

pub struct MongoDb {
    coll: Collection<Document>,
}

impl MongoDb {
    pub async fn connect(uri: &str) -> Result<Self> {
        let client = Client::with_uri_str(uri)
            .await
            .map_err(|e| Error::Database(format!("mongo connect: {e}")))?;
        let db = client.database("ultroid");
        let coll = db.collection::<Document>("kv");
        // Verify connectivity explicitly (client creation is lazy).
        coll.count_documents(doc! {})
            .await
            .map_err(|e| Error::Database(format!("mongo ping: {e}")))?;
        Ok(MongoDb { coll })
    }
}

#[async_trait]
impl Database for MongoDb {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let d = self
            .coll
            .find_one(doc! { "key": key })
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(d.and_then(|d| d.get_str("value").ok().map(|s| s.to_string())))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.coll
            .update_one(
                doc! { "key": key },
                doc! { "$set": { "key": key, "value": value } },
            )
            .upsert(true)
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<()> {
        self.coll
            .delete_one(doc! { "key": key })
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        use futures::TryStreamExt;
        let mut cursor = self
            .coll
            .find(doc! { "key": { "$regex": format!("^{}", regex::escape(prefix)) } })
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut out = Vec::new();
        while let Some(d) = cursor.try_next().await.map_err(|e| Error::Database(e.to_string()))? {
            if let Ok(k) = d.get_str("key") {
                out.push(k.to_string());
            }
        }
        Ok(out)
    }
}
