//! Addon extender — port of `pyUltroid/extender/` (`_manifest.py`,
//! `_manager.py`, `_installer.py`).
//!
//! Addons are downloaded as archives from a URL and recorded in the DB.
//! Archive extraction is intentionally not done here: it needs the `zip`
//! crate, which is not a workspace dependency — `install` downloads and
//! stores the archive under `data_dir/addons/` and reports the path.

use crate::db::Database;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
}

pub struct Extender {
    db: Arc<dyn Database>,
    http: reqwest::Client,
    data_dir: std::path::PathBuf,
}

impl Extender {
    pub fn new(db: Arc<dyn Database>, data_dir: impl Into<std::path::PathBuf>) -> Self {
        Extender {
            db,
            http: reqwest::Client::new(),
            data_dir: data_dir.into(),
        }
    }

    pub async fn install_from_url(&self, url: &str, name: &str) -> Result<std::path::PathBuf> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!("addon download failed: {}", resp.status())));
        }
        let bytes = resp.bytes().await?;
        let dir = self.data_dir.join("addons");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.zip"));
        std::fs::write(&path, &bytes)?;
        self.db
            .set(&format!("addon:{name}"), &format!("{{\"url\":\"{url}\",\"path\":\"{}\"}}", path.display()))
            .await?;
        Ok(path)
    }

    pub async fn list(&self) -> Result<Vec<String>> {
        Ok(self.db.keys("addon:").await?)
    }

    pub async fn uninstall(&self, name: &str) -> Result<()> {
        self.db.del(&format!("addon:{name}")).await
    }
}
