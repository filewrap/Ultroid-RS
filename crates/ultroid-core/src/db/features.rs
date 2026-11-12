//! Per-feature DB namespaces — port of `pyUltroid/dB/*`.
//! Each Python feature module (`afk_db`, `warn_db`, `notes_db`, …) is a
//! namespaced key prefix over the shared `Database` surface.

use crate::db::{db_get_json, db_set_json, Database};
use crate::error::Result;
use std::sync::Arc;

/// Thin namespaced view over a shared database.
#[derive(Clone)]
pub struct FeatureDb {
    db: Arc<dyn Database>,
    prefix: &'static str,
}

impl FeatureDb {
    pub fn new(db: Arc<dyn Database>, prefix: &'static str) -> Self {
        FeatureDb { db, prefix }
    }

    fn key(&self, key: &str) -> String {
        format!("{}:{}", self.prefix, key)
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        self.db.get(&self.key(key)).await
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.set(&self.key(key), value).await
    }

    pub async fn del(&self, key: &str) -> Result<()> {
        self.db.del(&self.key(key)).await
    }

    pub async fn keys(&self) -> Result<Vec<String>> {
        self.db.keys(&format!("{}:", self.prefix)).await
    }

    pub async fn get_json<T: serde::de::DeserializeOwned + Send>(&self, key: &str) -> Result<Option<T>> {
        db_get_json(&*self.db, &self.key(key)).await
    }

    pub async fn set_json<T: serde::Serialize + Send + Sync>(&self, key: &str, value: &T) -> Result<()> {
        db_set_json(&*self.db, &self.key(key), value).await
    }
}

/// One `FeatureDb` per Python `pyUltroid/dB/*.py` module.
pub struct Features {
    pub afk: FeatureDb,
    pub antiflood: FeatureDb,
    pub asstcmd: FeatureDb,
    pub blacklist: FeatureDb,
    pub blacklist_chat: FeatureDb,
    pub botchat: FeatureDb,
    pub echo: FeatureDb,
    pub filestore: FeatureDb,
    pub filter: FeatureDb,
    pub forcesub: FeatureDb,
    pub gban_mute: FeatureDb,
    pub greetings: FeatureDb,
    pub guest: FeatureDb,
    pub mute: FeatureDb,
    pub notes: FeatureDb,
    pub nsfw: FeatureDb,
    pub snips: FeatureDb,
    pub vc_sudos: FeatureDb,
    pub warn: FeatureDb,
}

impl Features {
    pub fn new(db: Arc<dyn Database>) -> Self {
        let f = |p: &'static str| FeatureDb::new(db.clone(), p);
        Features {
            afk: f("afk"),
            antiflood: f("antiflood"),
            asstcmd: f("asstcmd"),
            blacklist: f("blacklist"),
            blacklist_chat: f("blacklist_chat"),
            botchat: f("botchat"),
            echo: f("echo"),
            filestore: f("filestore"),
            filter: f("filter"),
            forcesub: f("forcesub"),
            gban_mute: f("gban_mute"),
            greetings: f("greetings"),
            guest: f("guest"),
            mute: f("mute"),
            notes: f("notes"),
            nsfw: f("nsfw"),
            snips: f("snips"),
            vc_sudos: f("vc_sudos"),
            warn: f("warn"),
        }
    }
}
