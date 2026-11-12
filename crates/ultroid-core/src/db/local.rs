//! Local JSON database — port of Python's built-in `_JSONDatabase`
//! (`{name}.localdb.json`). Always available; the default backend.

use super::Database;
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub struct LocalJsonDb {
    path: PathBuf,
    map: RwLock<HashMap<String, String>>,
}

impl LocalJsonDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let map = match std::fs::read_to_string(&path) {
            Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s)
                .map_err(|e| Error::Database(format!("corrupt local db {}: {e}", path.display())))?,
            _ => HashMap::new(),
        };
        Ok(LocalJsonDb {
            path,
            map: RwLock::new(map),
        })
    }

    fn persist(&self) -> Result<()> {
        let map = self.map.read().map_err(|_| Error::Database("db lock poisoned".into()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&*map)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[async_trait]
impl Database for LocalJsonDb {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let map = self.map.read().map_err(|_| Error::Database("db lock poisoned".into()))?;
        Ok(map.get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        {
            let mut map = self.map.write().map_err(|_| Error::Database("db lock poisoned".into()))?;
            map.insert(key.to_string(), value.to_string());
        }
        self.persist()
    }

    async fn del(&self, key: &str) -> Result<()> {
        {
            let mut map = self.map.write().map_err(|_| Error::Database("db lock poisoned".into()))?;
            map.remove(key);
        }
        self.persist()
    }

    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        let map = self.map.read().map_err(|_| Error::Database("db lock poisoned".into()))?;
        Ok(map.keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
}

/// In-memory database (tests / ephemeral use).
pub struct MemoryDb {
    map: RwLock<HashMap<String, String>>,
}

impl Default for MemoryDb {
    fn default() -> Self {
        MemoryDb { map: RwLock::new(HashMap::new()) }
    }
}

impl MemoryDb {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Database for MemoryDb {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.map.read().unwrap().get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.map.write().unwrap().insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn del(&self, key: &str) -> Result<()> {
        self.map.write().unwrap().remove(key);
        Ok(())
    }
    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .map
            .read()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}
