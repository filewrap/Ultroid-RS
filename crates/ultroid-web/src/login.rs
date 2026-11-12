//! Phone-OTP and QR login flows.
//!
//! Port of `pyUltroid/webserver/login.py`, with the Telethon client replaced
//! by a grammers client built through a [`ClientFactory`].
//!
//! The only standalone machinery here is the *session factory*: building a
//! grammers client is the one step that needs to be replaceable (tests
//! inject a fake factory) and lazy (importing `grammers` must not be forced
//! on anyone who merely uses the setup types). That is why
//! [`SessionFactory`] exists as its own type with its own `build`, instead
//! of a module-level default.
//!
//! [`LoginManager`] then owns *nothing but* the in-flight sessions: it
//! creates clients through the factory, tracks them by id, expires them,
//! and guarantees a client is disconnected exactly once.
//!
//! QR login has no high-level helper in grammers 0.12, so it is implemented
//! with the raw TL calls Telethon's `qr_login` uses, verified against the
//! generated layer-233 code:
//! `auth.exportLoginToken` / `auth.importLoginToken` returning
//! `auth.LoginToken::{Token, MigrateTo, Success}`. A `MigrateTo` result is
//! retried once on the named DC via `invoke_in_dc`, mirroring the
//! `FILE_MIGRATE` handling in grammers' own `request_login_code`.
//!
//! The "session string" a successful login produces is **not** a Telethon
//! `StringSession` — those are not portable to grammers (locked rule). It
//! is the path of the grammers sqlite session file the login client wrote
//! (`sessions/login-<uuid>.db`); the file *is* the session, and the setup
//! route persists that path as `SESSION` in `.env`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE};
use chrono::{DateTime, TimeDelta, Utc};
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::{Client, SignInError};
use grammers_mtsender::InvocationError;
use grammers_session::storages::SqliteSession;
use grammers_tl_types as tl;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use ultroid_core::{Error, Result};

/// `SESSION_TTL_SECONDS` — in-flight login attempts expire after 10 min.
pub const SESSION_TTL: Duration = Duration::from_secs(600);
pub const KIND_PHONE: &str = "phone";
pub const KIND_QR: &str = "qr";

/// What a login step produced. `session_string` is the sqlite session file
/// path; it never leaves the server except into `.env`.
#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub ok: bool,
    pub session_string: Option<String>,
    pub needs_password: bool,
    pub error: Option<String>,
}

impl LoginOutcome {
    pub fn success(session_string: String) -> Self {
        Self {
            ok: true,
            session_string: Some(session_string),
            needs_password: false,
            error: None,
        }
    }

    pub fn needs_password() -> Self {
        Self {
            ok: false,
            session_string: None,
            needs_password: true,
            error: None,
        }
    }
}

/// What `start_phone` produced. grammers 0.12 does not expose
/// `auth.SentCode.type`, so there is no delivery-channel name to report —
/// the route answers `code_type: null` and the UI shows the generic
/// "code sent" message instead of inventing one.
#[derive(Debug, Clone)]
pub struct PhoneStarted {
    pub login_id: String,
}

/// What `start_qr` produced.
#[derive(Debug, Clone)]
pub struct QrStarted {
    pub login_id: String,
    pub url: String,
    pub expires: DateTime<Utc>,
}

/// One `wait_qr` window resolved to.
#[derive(Debug, Clone)]
pub enum QrWait {
    Success { session_string: String },
    Expired { url: String, expires: DateTime<Utc> },
    NeedsPassword,
}

/// No live session with that id (unknown, expired, or wrong kind).
#[derive(Debug, Clone)]
pub struct LoginSessionUnknown(pub String);

impl std::fmt::Display for LoginSessionUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown or expired login_id: {}", self.0)
    }
}

impl std::error::Error for LoginSessionUnknown {}

/// A login failure that maps to an HTTP status (see [`LoginErrorMapper`]
/// in `app.rs`). `Other` carries a safe, secret-free detail.
#[derive(Debug, Clone)]
pub enum LoginError {
    UnknownSession(LoginSessionUnknown),
    PhoneNumberInvalid,
    PhoneCodeInvalid,
    PhoneCodeExpired,
    PasswordHashInvalid,
    ApiIdInvalid,
    FloodWait(u32),
    Other(String),
}

impl LoginError {
    pub fn from_rpc(err: &grammers_mtsender::RpcError) -> Self {
        match err.name.as_str() {
            "PHONE_NUMBER_INVALID" => Self::PhoneNumberInvalid,
            "PHONE_CODE_INVALID" => Self::PhoneCodeInvalid,
            "PHONE_CODE_EXPIRED" => Self::PhoneCodeExpired,
            "PASSWORD_HASH_INVALID" => Self::PasswordHashInvalid,
            "API_ID_INVALID" => Self::ApiIdInvalid,
            "FLOOD_WAIT" => Self::FloodWait(err.value.unwrap_or(0)),
            name => Self::Other(name.to_string()),
        }
    }

    pub fn from_invocation(err: &InvocationError) -> Self {
        match err {
            InvocationError::Rpc(rpc) => Self::from_rpc(rpc),
            other => Self::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSession(e) => write!(f, "{e}"),
            Self::PhoneNumberInvalid => write!(f, "invalid phone number"),
            Self::PhoneCodeInvalid => write!(f, "invalid login code"),
            Self::PhoneCodeExpired => write!(f, "login code expired - request a new one"),
            Self::PasswordHashInvalid => write!(f, "invalid 2FA password"),
            Self::ApiIdInvalid => write!(f, "API_ID/API_HASH combination is invalid"),
            Self::FloodWait(secs) => write!(f, "rate limited by Telegram - retry in {secs}s"),
            Self::Other(detail) => write!(f, "login failed ({detail})"),
        }
    }
}

impl std::error::Error for LoginError {}

impl From<LoginSessionUnknown> for LoginError {
    fn from(e: LoginSessionUnknown) -> Self {
        Self::UnknownSession(e)
    }
}

/// One connected grammers client owned by a login attempt.
pub struct LoginClient {
    pub client: Client,
    runner: Option<JoinHandle<()>>,
    pub session_path: PathBuf,
}

impl LoginClient {
    /// Stop the sender-pool runner. Dropping the client alone leaves the
    /// runner task alive; aborting it is what actually disconnects.
    pub fn disconnect(mut self) {
        if let Some(runner) = self.runner.take() {
            runner.abort();
        }
    }
}

/// Builds the grammers client a login attempt runs on.
///
/// Kept as a trait so the manager has no knowledge of `SqliteSession` or
/// `SenderPool` and the whole manager can be exercised against a fake
/// client with no Telegram involved.
#[async_trait]
pub trait ClientFactory: Send + Sync {
    async fn build(&self, api_id: i32, api_hash: &str) -> Result<LoginClient>;
}

/// The production factory: a fresh sqlite session file per attempt, a
/// `SenderPool` on that session, and a spawned runner.
pub struct GrammersClientFactory {
    pub session_dir: PathBuf,
}

impl GrammersClientFactory {
    pub fn new(session_dir: impl AsRef<Path>) -> Self {
        Self {
            session_dir: session_dir.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl ClientFactory for GrammersClientFactory {
    async fn build(&self, api_id: i32, _api_hash: &str) -> Result<LoginClient> {
        std::fs::create_dir_all(&self.session_dir).map_err(Error::Io)?;
        let path = self
            .session_dir
            .join(format!("login-{}.db", Uuid::new_v4().simple()));
        let session = Arc::new(
            SqliteSession::open(&path)
                .await
                .map_err(|e| Error::Telegram(format!("sqlite session open failed: {e}")))?,
        );
        let pool = grammers_mtsender::SenderPool::new(session, api_id);
        let grammers_mtsender::SenderPool { runner, handle, .. } = pool;
        let client = Client::new(handle);
        let runner_handle = tokio::spawn(async move { runner.run().await });
        Ok(LoginClient {
            client,
            runner: Some(runner_handle),
            session_path: path,
        })
    }
}

/// Builds the grammers client a login attempt runs on.
pub struct SessionFactory {
    inner: Arc<dyn ClientFactory>,
    api_id: i32,
    api_hash: String,
}

impl SessionFactory {
    pub fn new(
        api_id: i32,
        api_hash: &str,
        custom: Option<Arc<dyn ClientFactory>>,
        session_dir: PathBuf,
    ) -> Self {
        let inner: Arc<dyn ClientFactory> =
            custom.unwrap_or_else(|| Arc::new(GrammersClientFactory::new(session_dir)));
        Self {
            inner,
            api_id,
            api_hash: api_hash.to_string(),
        }
    }

    /// A factory bound to `api_id` / `api_hash`.
    ///
    /// A custom factory ignores the credentials entirely — that is the
    /// contract tests rely on — so rebinding is just a new wrapper around
    /// the same inner factory.
    pub fn for_credentials(&self, api_id: i32, api_hash: &str) -> SessionFactory {
        Self {
            inner: self.inner.clone(),
            api_id,
            api_hash: api_hash.to_string(),
        }
    }

    pub async fn build(&self) -> Result<LoginClient> {
        self.inner.build(self.api_id, &self.api_hash).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    Phone,
    Qr,
}

/// One in-flight login attempt and the client it owns.
pub struct LoginSession {
    pub id: String,
    pub kind: LoginKind,
    pub client: LoginClient,
    pub phone: Option<String>,
    pub login_token: Option<LoginToken>,
    pub password_token: Option<PasswordToken>,
    pub qr_token: Option<Vec<u8>>,
    pub qr_url: Option<String>,
    pub qr_expires: Option<DateTime<Utc>>,
    pub needs_2fa: bool,
    pub created: Instant,
}

impl LoginSession {
    pub fn age(&self) -> Duration {
        self.created.elapsed()
    }
}

/// The registry of in-flight login attempts for one credential pair.
pub struct LoginManager {
    pub api_id: i32,
    pub api_hash: String,
    factory: SessionFactory,
    ttl: Duration,
    sessions: Mutex<HashMap<String, LoginSession>>,
}

impl LoginManager {
    pub fn new(
        api_id: i32,
        api_hash: &str,
        client_factory: Option<Arc<dyn ClientFactory>>,
        session_dir: PathBuf,
        ttl: Duration,
    ) -> Self {
        Self {
            api_id,
            api_hash: api_hash.to_string(),
            factory: SessionFactory::new(api_id, api_hash, client_factory, session_dir),
            ttl,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults(
        api_id: i32,
        api_hash: &str,
        client_factory: Option<Arc<dyn ClientFactory>>,
        session_dir: PathBuf,
    ) -> Self {
        Self::new(api_id, api_hash, client_factory, session_dir, SESSION_TTL)
    }

    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub async fn contains(&self, login_id: &str) -> bool {
        self.sessions.lock().await.contains_key(login_id)
    }

    /// True when `login_id` is live. Never raises.
    pub async fn peek(&self, login_id: &str) -> bool {
        self.get(login_id, None).await.is_ok()
    }

    // -- client lifecycle

    async fn sweep(&self) {
        let expired: Vec<LoginSession> = {
            let mut guard = self.sessions.lock().await;
            let now = Instant::now();
            let ids: Vec<String> = guard
                .iter()
                .filter(|(_, s)| now.duration_since(s.created) > self.ttl)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| guard.remove(&id)).collect()
        };
        for session in expired {
            session.client.disconnect();
        }
    }

    /// Disconnect every live client. Call on shutdown / key rotation.
    pub async fn dispose(&self) -> usize {
        let sessions: Vec<LoginSession> = {
            let mut guard = self.sessions.lock().await;
            guard.drain().map(|(_, s)| s).collect()
        };
        let count = sessions.len();
        for session in sessions {
            session.client.disconnect();
        }
        count
    }

    // -- registry

    async fn get(
        &self,
        login_id: &str,
        kind: Option<LoginKind>,
    ) -> std::result::Result<LoginSession, LoginSessionUnknown> {
        let mut guard = self.sessions.lock().await;
        let Some(session) = guard.remove(login_id) else {
            return Err(LoginSessionUnknown(login_id.to_string()));
        };
        if session.age() > self.ttl {
            drop(guard);
            session.client.disconnect();
            return Err(LoginSessionUnknown(login_id.to_string()));
        }
        if let Some(kind) = kind {
            if session.kind != kind {
                guard.insert(session.id.clone(), session);
                return Err(LoginSessionUnknown(login_id.to_string()));
            }
        }
        Ok(session)
    }

    async fn put(&self, session: LoginSession) {
        self.sessions
            .lock()
            .await
            .insert(session.id.clone(), session);
    }

    fn finish(session: LoginSession) -> String {
        let path = session.client.session_path.clone();
        session.client.disconnect();
        path.to_string_lossy().into_owned()
    }

    // -- phone OTP flow

    /// Send the login code. Returns the `login_id` for the OTP step.
    pub async fn start_phone(&self, phone: &str) -> std::result::Result<PhoneStarted, LoginError> {
        self.sweep().await;
        let login_client = self
            .factory
            .build()
            .await
            .map_err(|e| LoginError::Other(e.to_string()))?;
        let token = match login_client
            .client
            .request_login_code(phone, &self.api_hash)
            .await
        {
            Ok(token) => token,
            Err(err) => {
                login_client.disconnect();
                return Err(LoginError::from_invocation(&err));
            }
        };
        let login_id = Uuid::new_v4().simple().to_string();
        self.put(LoginSession {
            id: login_id.clone(),
            kind: LoginKind::Phone,
            client: login_client,
            phone: Some(phone.to_string()),
            login_token: Some(token),
            password_token: None,
            qr_token: None,
            qr_url: None,
            qr_expires: None,
            needs_2fa: false,
            created: Instant::now(),
        })
        .await;
        Ok(PhoneStarted { login_id })
    }

    pub async fn submit_code(
        &self,
        login_id: &str,
        code: &str,
        password: Option<&str>,
    ) -> std::result::Result<LoginOutcome, LoginError> {
        let mut session = self.get(login_id, Some(LoginKind::Phone)).await?;
        if session.needs_2fa {
            if password.is_none() {
                let outcome = LoginOutcome::needs_password();
                self.put(session).await;
                return Ok(outcome);
            }
            let Some(token) = session.password_token.take() else {
                let err = LoginError::Other("2FA challenge lost".into());
                self.put(session).await;
                return Err(err);
            };
            return self
                .finish_with_password(session, token, password.unwrap())
                .await;
        }
        let Some(token) = session.login_token.as_ref() else {
            let err = LoginError::Other("login token lost".into());
            self.put(session).await;
            return Err(err);
        };
        match session.client.client.sign_in(token, code).await {
            Ok(_) => Ok(LoginOutcome::success(Self::finish(session))),
            Err(SignInError::PasswordRequired(token)) => {
                session.needs_2fa = true;
                if password.is_none() {
                    session.password_token = Some(token);
                    let outcome = LoginOutcome::needs_password();
                    self.put(session).await;
                    return Ok(outcome);
                }
                self.finish_with_password(session, token, password.unwrap())
                    .await
            }
            Err(SignInError::InvalidCode) => {
                let err = LoginError::PhoneCodeInvalid;
                self.put(session).await;
                Err(err)
            }
            Err(SignInError::InvalidPassword(token)) => {
                session.password_token = Some(token);
                let err = LoginError::PasswordHashInvalid;
                self.put(session).await;
                Err(err)
            }
            Err(SignInError::SignUpRequired) => {
                let err = LoginError::Other("sign up with official client required".into());
                self.put(session).await;
                Err(err)
            }
            Err(SignInError::Other(InvocationError::Rpc(ref rpc)))
                if rpc.name == "PHONE_CODE_EXPIRED" || rpc.name == "AUTH_RESTART" =>
            {
                let err = LoginError::from_rpc(rpc);
                session.client.disconnect();
                Err(err)
            }
            Err(SignInError::Other(err)) => {
                let mapped = LoginError::from_invocation(&err);
                self.put(session).await;
                Err(mapped)
            }
        }
    }

    /// 2FA step, valid for both phone and QR logins.
    pub async fn submit_password(
        &self,
        login_id: &str,
        password: &str,
    ) -> std::result::Result<LoginOutcome, LoginError> {
        let mut session = self.get(login_id, None).await?;
        let Some(token) = session.password_token.take() else {
            // No 2FA token was ever issued for this session: the password
            // step is meaningless; keep the session for the caller to retry
            // the step that does issue one.
            let err = LoginError::Other("no 2FA challenge in progress".into());
            self.put(session).await;
            return Err(err);
        };
        self.finish_with_password(session, token, password).await
    }

    async fn finish_with_password(
        &self,
        mut session: LoginSession,
        token: PasswordToken,
        password: &str,
    ) -> std::result::Result<LoginOutcome, LoginError> {
        match session
            .client
            .client
            .check_password(token, password.as_bytes().to_vec())
            .await
        {
            Ok(_) => Ok(LoginOutcome::success(Self::finish(session))),
            Err(SignInError::InvalidPassword(token)) => {
                session.password_token = Some(token);
                let err = LoginError::PasswordHashInvalid;
                self.put(session).await;
                Err(err)
            }
            Err(SignInError::Other(err)) => {
                let mapped = LoginError::from_invocation(&err);
                session.client.disconnect();
                Err(mapped)
            }
            Err(other) => {
                let err = LoginError::Other(other.to_string());
                session.client.disconnect();
                Err(err)
            }
        }
    }

    // -- QR flow

    /// Begin QR login. Returns the `login_id`, the `tg://login?token=…`
    /// URL and its expiry.
    pub async fn start_qr(&self) -> std::result::Result<QrStarted, LoginError> {
        self.sweep().await;
        let login_client = self
            .factory
            .build()
            .await
            .map_err(|e| LoginError::Other(e.to_string()))?;
        let (token, expires) = match self.export_login_token(&login_client.client).await {
            Ok(pair) => pair,
            Err(err) => {
                login_client.disconnect();
                return Err(err);
            }
        };
        let url = qr_login_url(&token);
        let login_id = Uuid::new_v4().simple().to_string();
        self.put(LoginSession {
            id: login_id.clone(),
            kind: LoginKind::Qr,
            client: login_client,
            phone: None,
            login_token: None,
            password_token: None,
            qr_token: Some(token),
            qr_url: Some(url.clone()),
            qr_expires: Some(expires),
            needs_2fa: false,
            created: Instant::now(),
        })
        .await;
        Ok(QrStarted {
            login_id,
            url,
            expires,
        })
    }

    pub async fn qr_url(&self, login_id: &str) -> std::result::Result<String, LoginSessionUnknown> {
        let session = self.get(login_id, Some(LoginKind::Qr)).await?;
        let url = session.qr_url.clone().unwrap_or_default();
        self.put(session).await;
        Ok(url)
    }

    /// Await one QR expiry window.
    ///
    /// Returns `Success` (the session path is consumed by the route layer,
    /// never sent over HTTP), `Expired` (auto-recreated, with the new URL
    /// and expiry), or `NeedsPassword`.
    pub async fn wait_qr(&self, login_id: &str) -> std::result::Result<QrWait, LoginError> {
        let session = self.get(login_id, Some(LoginKind::Qr)).await?;
        let token = session.qr_token.clone().unwrap_or_default();
        let client = session.client.client.clone();
        let wait_secs = session
            .qr_expires
            .map(|exp| (exp - Utc::now()).num_seconds())
            .unwrap_or(30)
            .clamp(1, 600) as u64;
        // Put the session back for the long-poll: `peek()` must stay true
        // while waiting, and the password route must still find the session.
        // No lock is held across the await — the cloned client carries the
        // invoke, and the session is taken out again to resolve it.
        self.put(session).await;
        let attempt = tokio::time::timeout(
            Duration::from_secs(wait_secs),
            client.invoke(&tl::functions::auth::ImportLoginToken { token }),
        )
        .await;
        // Take the session out again to resolve the outcome.
        let mut session = self.get(login_id, Some(LoginKind::Qr)).await?;
        match attempt {
            Ok(Ok(tl::enums::auth::LoginToken::Success(_))) => Ok(QrWait::Success {
                session_string: Self::finish(session),
            }),
            Ok(Ok(tl::enums::auth::LoginToken::Token(_))) => {
                // Not yet scanned and not a success: treat as expiry.
                let (url, expires) = self.recreate_qr(&mut session).await;
                self.put(session).await;
                Ok(QrWait::Expired { url, expires })
            }
            Ok(Ok(tl::enums::auth::LoginToken::MigrateTo(m))) => {
                match session
                    .client
                    .client
                    .invoke_in_dc(
                        m.dc_id,
                        &tl::functions::auth::ImportLoginToken { token: m.token },
                    )
                    .await
                {
                    Ok(tl::enums::auth::LoginToken::Success(_)) => Ok(QrWait::Success {
                        session_string: Self::finish(session),
                    }),
                    _ => {
                        let (url, expires) = self.recreate_qr(&mut session).await;
                        self.put(session).await;
                        Ok(QrWait::Expired { url, expires })
                    }
                }
            }
            Ok(Err(InvocationError::Rpc(ref rpc))) if rpc.is("SESSION_PASSWORD_NEEDED") => {
                session.needs_2fa = true;
                self.put(session).await;
                Ok(QrWait::NeedsPassword)
            }
            Ok(Err(err)) => {
                let mapped = LoginError::from_invocation(&err);
                session.client.disconnect();
                Err(mapped)
            }
            Err(_elapsed) => {
                // The long-poll outlived the token: recreate, report expired.
                let (url, expires) = self.recreate_qr(&mut session).await;
                self.put(session).await;
                Ok(QrWait::Expired { url, expires })
            }
        }
    }

    /// Export a fresh login token, retrying once on the DC a `MigrateTo`
    /// result names. Returns the raw token bytes and its expiry.
    async fn export_login_token(
        &self,
        client: &Client,
    ) -> std::result::Result<(Vec<u8>, DateTime<Utc>), LoginError> {
        let request = tl::functions::auth::ExportLoginToken {
            api_id: self.api_id,
            api_hash: self.api_hash.clone(),
            except_ids: Vec::new(),
        };
        match client.invoke(&request).await {
            Ok(tl::enums::auth::LoginToken::Token(t)) => {
                Ok((t.token, Utc::now() + TimeDelta::seconds(t.expires as i64)))
            }
            Ok(tl::enums::auth::LoginToken::MigrateTo(m)) => {
                match client.invoke_in_dc(m.dc_id, &request).await {
                    Ok(tl::enums::auth::LoginToken::Token(t)) => {
                        Ok((t.token, Utc::now() + TimeDelta::seconds(t.expires as i64)))
                    }
                    Ok(_) => Err(LoginError::Other("unexpected login token response".into())),
                    Err(err) => Err(LoginError::from_invocation(&err)),
                }
            }
            Ok(_) => Err(LoginError::Other("unexpected login token response".into())),
            Err(err) => Err(LoginError::from_invocation(&err)),
        }
    }

    /// Export a new token for an expired QR session and update it in place.
    /// Returns the new URL and expiry. Never fails the caller on export
    /// trouble: the old URL/expiry are kept so the route can still report
    /// `expired` and the UI can start over.
    async fn recreate_qr(&self, session: &mut LoginSession) -> (String, DateTime<Utc>) {
        match self.export_login_token(&session.client.client).await {
            Ok((token, expires)) => {
                let url = qr_login_url(&token);
                session.qr_token = Some(token);
                session.qr_url = Some(url.clone());
                session.qr_expires = Some(expires);
                (url, expires)
            }
            Err(_) => (
                session.qr_url.clone().unwrap_or_default(),
                session.qr_expires.unwrap_or_else(Utc::now),
            ),
        }
    }
}

/// The `tg://login?token=…` URL for a raw login token, byte-identical to
/// Telethon's `qr_login` (padded URL-safe base64).
pub fn qr_login_url(token: &[u8]) -> String {
    format!("tg://login?token={}", URL_SAFE.encode(token))
}

/// The historical module-level default client factory (grammers-backed).
pub fn default_client_factory(session_dir: PathBuf) -> Arc<dyn ClientFactory> {
    Arc::new(GrammersClientFactory::new(session_dir))
}
