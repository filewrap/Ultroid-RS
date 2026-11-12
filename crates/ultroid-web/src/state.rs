//! Shared mutable state for the setup server.
//!
//! Port of `pyUltroid/webserver/state.py`: [`SetupState`] is the one object
//! every setup route mutates — credentials, the login manager, and the
//! one-shot completion latch.
//!
//! Two deliberate changes from the previous shape, carried over:
//!
//! * `grammers` is **not** imported here. It used to come in transitively
//!   via the login module even though this module only stores a manager
//!   reference; the manager is built through an injectable factory.
//! * [`SetupState::retarget`] disposes the manager it replaces. Re-posting
//!   the API keys mid-login used to leave the previous manager's connected
//!   grammers client behind forever.
//!
//! Rust shape: the state lives behind a `tokio::sync::Mutex` in the app
//! (routes run concurrently, Python's asyncio routes did not). The manager
//! itself is `Arc`-shared so a long QR wait can hold it without holding
//! the state lock. `on_complete` is an `Arc<dyn Fn() + Send + Sync>`; the
//! 0.5 s delayed fire is a spawned tokio task instead of a daemon thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::envfile::EnvFile;
use crate::login::{ClientFactory, LoginManager};

/// The completion callback fires 0.5 s after the route returns, so the HTTP
/// response reaches the browser before any re-exec or spawn.
pub const COMPLETION_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

pub type OnComplete = Arc<dyn Fn() + Send + Sync>;

/// Mutable state shared by every setup route.
pub struct SetupState {
    pub token: String,
    pub env_path: PathBuf,
    pub standalone: bool,
    pub on_complete: Option<OnComplete>,
    pub client_factory: Option<Arc<dyn ClientFactory>>,
    pub session_dir: PathBuf,
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
    pub bot_token: Option<String>,
    pub log_channel: Option<i64>,
    /// The sqlite session file path of the logged-in account.
    /// Server-side only, never sent over HTTP.
    pub session_path: Option<PathBuf>,
    pub login_manager: Option<Arc<LoginManager>>,
    pub completed: bool,
}

impl SetupState {
    pub fn create(
        token: &str,
        env_path: Option<impl AsRef<Path>>,
        standalone: bool,
        on_complete: Option<OnComplete>,
        client_factory: Option<Arc<dyn ClientFactory>>,
        session_dir: Option<impl AsRef<Path>>,
    ) -> Self {
        let env_path = env_path
            .map(|p| p.as_ref().to_path_buf())
            .unwrap_or_else(EnvFile::default_path);
        let session_dir = session_dir
            .map(|p| p.as_ref().to_path_buf())
            .unwrap_or_else(|| {
                env_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("sessions")
            });
        Self {
            token: token.to_string(),
            env_path,
            standalone,
            on_complete,
            client_factory,
            session_dir,
            api_id: None,
            api_hash: None,
            bot_token: None,
            log_channel: None,
            session_path: None,
            login_manager: None,
            completed: false,
        }
    }

    pub fn has_api_keys(&self) -> bool {
        self.api_id.is_some() && self.api_hash.is_some()
    }

    pub fn has_session(&self) -> bool {
        self.session_path.is_some()
    }

    /// Fill in API keys from the `.env` file if they are not set yet.
    pub fn adopt_env_keys(&mut self) -> bool {
        if self.has_api_keys() {
            return false;
        }
        let env = self.env().read();
        let (Some(raw_id), Some(raw_hash)) = (env.get("API_ID"), env.get("API_HASH")) else {
            return false;
        };
        let Ok(api_id) = raw_id.parse::<i32>() else {
            return false;
        };
        self.api_id = Some(api_id);
        self.api_hash = Some(raw_hash.clone());
        true
    }

    pub fn env(&self) -> EnvFile {
        EnvFile::new(Some(&self.env_path))
    }

    /// Keep the session in memory only; persisting is the route's call.
    pub fn store_session(&mut self, session_path: PathBuf) {
        self.session_path = Some(session_path);
    }

    /// The login manager, rebuilt when the API keys change.
    pub fn manager(&mut self) -> Arc<LoginManager> {
        if let Some(current) = &self.login_manager {
            if current.api_id == self.api_id.unwrap_or(0)
                && current.api_hash == self.api_hash.clone().unwrap_or_default()
            {
                return current.clone();
            }
        }
        let manager = self.retarget(self.api_id, self.api_hash.clone());
        self.login_manager = Some(manager.clone());
        manager
    }

    /// Build a new manager for `api_id` / `api_hash`.
    ///
    /// The previous manager's sessions are closed first: swapping the
    /// credentials out from under a live login otherwise strands its
    /// connected client.
    pub fn retarget(&mut self, api_id: Option<i32>, api_hash: Option<String>) -> Arc<LoginManager> {
        let previous = self.login_manager.take();
        if let Some(previous) = previous {
            tokio::spawn(async move {
                previous.dispose().await;
            });
        }
        Arc::new(LoginManager::with_defaults(
            api_id.unwrap_or(0),
            &api_hash.unwrap_or_default(),
            self.client_factory.clone(),
            self.session_dir.clone(),
        ))
    }

    /// The manager, or `None` when no usable API keys exist yet.
    pub fn manager_or_none(&mut self) -> Option<Arc<LoginManager>> {
        self.adopt_env_keys();
        if !self.has_api_keys() {
            return None;
        }
        Some(self.manager())
    }

    /// Fire the completion callback exactly once.
    ///
    /// Returns whether *this* call was the one that fired it. The callback
    /// runs on a short timer so the HTTP response reaches the browser
    /// before any re-exec or spawn.
    pub fn trigger_completion(&mut self) -> bool {
        if self.completed {
            return false;
        }
        self.completed = true;
        let Some(callback) = self.on_complete.clone() else {
            return false;
        };
        tokio::spawn(async move {
            tokio::time::sleep(COMPLETION_DELAY).await;
            callback();
        });
        true
    }
}

/// The historical `make_state` module surface.
pub fn make_state(
    token: &str,
    env_path: Option<impl AsRef<Path>>,
    standalone: bool,
    on_complete: Option<OnComplete>,
    client_factory: Option<Arc<dyn ClientFactory>>,
) -> SetupState {
    SetupState::create(
        token,
        env_path,
        standalone,
        on_complete,
        client_factory,
        None::<&Path>,
    )
}
