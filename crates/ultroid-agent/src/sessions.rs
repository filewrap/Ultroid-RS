//! DM agent-session state: anchors, ordering, and history keys — port of
//! `pyUltroid/agent/_sessions.py`.
//!
//! The store is deliberately free of any assistant-stack import so it can
//! be used — and tested — without a started bot.
//!
//! A session is identified by its *anchor*: the id of the session-start
//! card in the bot's DM. Every reply into that card's thread belongs to
//! the session.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ultroid_core::db::{Database, db_get_json, db_set_json};

/// Sessions kept per user, most recent first.
pub const MAX_SESSIONS: usize = 20;

/// uDB key prefix for a user's session list.
pub const SESSIONS_PREFIX: &str = "AGENT_DM_SESSIONS_";

/// A DM session's history key namespace. Never a bare chat id, so a
/// session's history can never collide with a normal chat's history.
const SESSION_KEY_FMT: &str = "dm:{user}:{anchor}";

/// One DM session: `anchor`, `title`, `created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub anchor: i64,
    pub title: String,
    pub created: i64,
}

/// A session could not be persisted.
#[derive(Debug, Clone)]
pub struct SessionStoreError(pub String);

impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionStoreError {}

/// The DM agent-session store (udB keys ``AGENT_DM_SESSIONS_<user>``).
///
/// Same shape as the dB layer's `FeatureDB` classes: one instance per
/// feature (`session_store`), with an injectable database. When `db` is
/// `None` (before startup has built it) reads return empty and writes
/// fail closed, exactly like the Python `_db_access` returning `None`.
pub struct SessionStore {
    db: Option<Arc<dyn Database>>,
}

impl SessionStore {
    pub const SESSION_KEY: &'static str = SESSIONS_PREFIX;

    pub fn new(db: Option<Arc<dyn Database>>) -> Self {
        SessionStore { db }
    }

    fn key(&self, user_id: i64) -> String {
        format!("{SESSIONS_PREFIX}{user_id}")
    }

    /// A user's sessions, newest first. Each is `anchor`, `title`,
    /// `created`.
    pub async fn session_list(&self, user_id: i64) -> Vec<Session> {
        let Some(db) = &self.db else {
            return Vec::new();
        };
        let stored: Option<Vec<Value>> = db_get_json(db.as_ref(), &self.key(user_id))
            .await
            .ok()
            .flatten();
        let Some(stored) = stored else {
            return Vec::new();
        };
        stored
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Session>(v).ok())
            .filter(|s| s.anchor != 0)
            .collect()
    }

    async fn save(&self, user_id: i64, sessions: &[Session]) -> bool {
        let Some(db) = &self.db else {
            return false;
        };
        let trimmed: Vec<Session> = sessions.iter().take(MAX_SESSIONS).cloned().collect();
        db_set_json(db.as_ref(), &self.key(user_id), &trimmed)
            .await
            .is_ok()
    }

    /// The session anchored at *anchor* for *user_id*, or `None`.
    pub async fn session_find(&self, user_id: i64, anchor: i64) -> Option<Session> {
        self.session_list(user_id)
            .await
            .into_iter()
            .find(|s| s.anchor == anchor)
    }

    /// Record a session anchored at *anchor*, replacing any older one there.
    ///
    /// Raises [`SessionStoreError`] when the write does not land: a
    /// session the store cannot find again is not a session, and
    /// pretending otherwise would strand the user's next reply.
    pub async fn session_create(
        &self,
        user_id: i64,
        anchor: i64,
        title: &str,
    ) -> Result<Session, SessionStoreError> {
        let title = if title.is_empty() {
            "New session".to_string()
        } else {
            title.chars().take(60).collect()
        };
        let session = Session {
            anchor,
            title,
            created: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        };
        let mut sessions: Vec<Session> = self
            .session_list(user_id)
            .await
            .into_iter()
            .filter(|s| s.anchor != anchor)
            .collect();
        sessions.insert(0, session.clone());
        if !self.save(user_id, &sessions).await {
            return Err(SessionStoreError(format!(
                "the session for {user_id} at anchor {anchor} could not be stored"
            )));
        }
        Ok(session)
    }

    /// The agent history key for one session.
    pub fn session_history_key(&self, user_id: i64, anchor: i64) -> String {
        SESSION_KEY_FMT
            .replacen("{user}", &user_id.to_string(), 1)
            .replacen("{anchor}", &anchor.to_string(), 1)
    }
}
