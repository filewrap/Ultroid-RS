//! The setup/status axum application and its routes.
//!
//! Port of `pyUltroid/webserver/app.py` (FastAPI) to axum 0.8.
//!
//! Route-for-route the same surface:
//! * `GET /status` — unauthenticated on purpose, exact legacy shape.
//! * `GET /` — the setup page (embedded; Python 404'd when the static
//!   assets were missing, the Rust binary always has its page).
//! * `GET /api/state`, `POST /api/setup/keys`
//! * `POST /api/login/phone`, `POST /api/login/otp`
//! * `POST /api/login/qr`, `GET /api/login/qr/{id}/wait` (SSE),
//!   `POST /api/login/qr/{id}/password`, `GET /api/login/qr/{id}/image`
//! * `GET /api/logs` (SSE), `POST /api/setup/complete`
//!
//! The token gate mirrors Python's pure-ASGI `TokenGateMiddleware`: it runs
//! before body parsing (unauthenticated callers always get 401, never 422)
//! and streams SSE responses through untouched. It is an axum middleware
//! plus [`axum::extract::DefaultBodyLimit`] for the 1 MiB cap — the
//! middleware is the outermost layer, so it runs first, and it also checks
//! a declared `content-length` itself, exactly like the Python gate.
//!
//! `standalone=True` (the binary's server mode): completion spawns the bot.
//! `standalone=False` (the bootstrap gate): completion fires `on_complete`
//! and the caller re-execs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Json, Path as AxumPath, State},
    http::{HeaderMap, Request, StatusCode, Uri, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use tracing;

use crate::bootstrap::SetupBootstrap;
use crate::envfile::EnvFile;
use crate::login::{ClientFactory, LoginError, QrWait};
use crate::logs::{LogBroker, LogStreamHub};
use crate::security::{RateLimiter, TokenAuthority, authority as global_authority};
use crate::setup_page::SETUP_PAGE;
use crate::state::{OnComplete, SetupState};

pub const BOT_TOKEN_PATTERN: &str = r"^\d+:[A-Za-z0-9_-]{30,}$";
pub const MAX_QR_CYCLES: usize = 8;
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
pub const API_HASH_MIN_LENGTH: usize = 8;
pub const LOG_KEEPALIVE_SECONDS: f64 = 15.0;
pub const OTP_ATTEMPTS: usize = 5;
pub const PHONE_ATTEMPTS: usize = 3;
pub const ATTEMPT_WINDOW: f64 = 60.0;

// -- payloads ------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct KeysPayload {
    pub api_id: i64,
    pub api_hash: String,
    pub bot_token: Option<String>,
    pub log_channel: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PhonePayload {
    pub phone: String,
}

#[derive(Debug, Deserialize)]
pub struct OtpPayload {
    pub login_id: String,
    pub code: String,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PasswordPayload {
    pub password: String,
}

// -- SSE -----------------------------------------------------------------

/// One `text/event-stream` frame, mirroring Python's `SseFrame` — an event
/// name plus a dict-or-string payload, rendered by axum's `Event`.
fn sse_frame(event: &str, data: &Value) -> Event {
    Event::default().event(event).data(data.to_string())
}

// -- error mapping -------------------------------------------------------

/// grammers login errors -> HTTP status + safe detail.
///
/// The error text goes through the redactor before logging, so a stray
/// provider message can never leak a phone number or password into the log
/// stream.
pub struct LoginErrorMapper;

impl LoginErrorMapper {
    pub fn response(&self, err: &LoginError) -> (StatusCode, Json<Value>) {
        let (status, detail) = self.describe(err);
        tracing::warn!(target: "pyUltroid.webserver", "login failed: {detail}");
        (status, Json(json!({ "detail": detail })))
    }

    pub fn describe(&self, err: &LoginError) -> (StatusCode, String) {
        match err {
            LoginError::UnknownSession(_) => {
                (StatusCode::NOT_FOUND, "unknown or expired login_id".into())
            }
            LoginError::PhoneNumberInvalid => {
                (StatusCode::BAD_REQUEST, "invalid phone number".into())
            }
            LoginError::PhoneCodeInvalid => (StatusCode::BAD_REQUEST, "invalid login code".into()),
            LoginError::PhoneCodeExpired => (
                StatusCode::BAD_REQUEST,
                "login code expired - request a new one".into(),
            ),
            LoginError::PasswordHashInvalid => {
                (StatusCode::BAD_REQUEST, "invalid 2FA password".into())
            }
            LoginError::ApiIdInvalid => (
                StatusCode::BAD_REQUEST,
                "API_ID/API_HASH combination is invalid".into(),
            ),
            LoginError::FloodWait(secs) => (
                StatusCode::TOO_MANY_REQUESTS,
                format!("rate limited by Telegram - retry in {secs}s"),
            ),
            LoginError::Other(detail) => {
                (StatusCode::BAD_GATEWAY, format!("login failed ({detail})"))
            }
        }
    }
}

/// The one 401 body every gated route returns.
fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "detail": "invalid or missing setup token" })),
    )
}

fn too_many() -> (StatusCode, Json<Value>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "detail": "too many attempts - slow down" })),
    )
}

// -- token gate middleware ------------------------------------------------

/// Pure token gate for the whole `/api/*` surface.
///
/// Deliberately not layered on the `Json` extractors: it runs before body
/// parsing (unauthenticated callers always get 401, never 422) and it
/// streams SSE responses through untouched. Routes keep their own token
/// checks as defense in depth.
async fn token_gate(
    State(app): State<Arc<SetupApplication>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.uri().path().starts_with("/api/") {
        let query = request.uri().query().unwrap_or("");
        if !app.authority.permits(request.headers(), query, &app.token) {
            return unauthorized().into_response();
        }
        if let Some(declared) = request.headers().get("content-length") {
            if let Ok(text) = declared.to_str() {
                if let Ok(length) = text.parse::<usize>() {
                    if length > MAX_REQUEST_BODY_BYTES {
                        return (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            Json(json!({ "detail": "request body too large" })),
                        )
                            .into_response();
                    }
                }
            }
        }
    }
    next.run(request).await
}

// -- the application -----------------------------------------------------

/// The setup/status app and its routes.
pub struct SetupApplication {
    pub state: Arc<Mutex<SetupState>>,
    pub token: String,
    /// The `.env` path, fixed at construction (the state holds the same
    /// value; keeping it here avoids locking the state for a read-only path).
    pub env_path: PathBuf,
    pub hub: Arc<Mutex<LogStreamHub>>,
    pub broker: Arc<LogBroker>,
    pub otp_limiter: Arc<RateLimiter>,
    pub phone_limiter: Arc<RateLimiter>,
    pub authority: Arc<TokenAuthority>,
    pub log_keepalive_seconds: f64,
}

impl SetupApplication {
    pub fn new(
        token: Option<String>,
        env_path: Option<impl AsRef<Path>>,
        standalone: bool,
        on_complete: Option<OnComplete>,
        client_factory: Option<Arc<dyn ClientFactory>>,
        session_dir: Option<impl AsRef<Path>>,
        log_keepalive_seconds: f64,
    ) -> Arc<Self> {
        let authority = global_authority();
        let resolved = token
            .filter(|t| !t.is_empty())
            .or_else(|| std::env::var("SETUP_TOKEN").ok().filter(|t| !t.is_empty()))
            .unwrap_or_else(|| authority.mint(true));
        authority.redactor().add(&resolved);
        let on_complete = match (standalone, on_complete) {
            (true, None) => Some(Arc::new(|| {
                SetupBootstrap::default().spawn_bot();
            }) as OnComplete),
            (_, provided) => provided,
        };
        let env_path = env_path
            .map(|p| p.as_ref().to_path_buf())
            .unwrap_or_else(EnvFile::default_path);
        let state = SetupState::create(
            &resolved,
            Some(&env_path),
            standalone,
            on_complete,
            client_factory,
            session_dir,
        );
        let hub = LogStreamHub::new(None);
        let broker = hub.broker.clone();
        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            token: resolved,
            env_path,
            hub: Arc::new(Mutex::new(hub)),
            broker,
            otp_limiter: Arc::new(RateLimiter::new(OTP_ATTEMPTS, ATTEMPT_WINDOW)),
            phone_limiter: Arc::new(RateLimiter::new(PHONE_ATTEMPTS, ATTEMPT_WINDOW)),
            authority,
            log_keepalive_seconds,
        })
    }

    pub fn env(&self) -> EnvFile {
        EnvFile::new(Some(&self.env_path))
    }

    fn authorized(&self, headers: &HeaderMap, query: &str) -> bool {
        self.authority.permits(headers, query, &self.token)
    }

    fn client_ip(headers: &HeaderMap) -> String {
        if let Some(forwarded) = headers.get("x-forwarded-for") {
            if let Ok(text) = forwarded.to_str() {
                if let Some(first) = text.split(',').next() {
                    let first = first.trim();
                    if !first.is_empty() {
                        return first.to_string();
                    }
                }
            }
        }
        if let Some(real) = headers.get("x-real-ip") {
            if let Ok(text) = real.to_str() {
                if !text.is_empty() {
                    return text.to_string();
                }
            }
        }
        "unknown".to_string()
    }

    /// Keep the session in memory and persist it; never log its value.
    pub async fn store_session(&self, session_path: PathBuf) {
        let mut state = self.state.lock().await;
        state.store_session(session_path.clone());
        self.authority
            .redactor()
            .add(session_path.to_string_lossy());
        drop(state);
        let mut updates = std::collections::HashMap::new();
        updates.insert(
            "SESSION".to_string(),
            session_path.to_string_lossy().into_owned(),
        );
        if let Err(err) = self.env().write(&updates, true) {
            tracing::warn!(target: "pyUltroid.webserver", "SESSION persist failed: {err}");
            return;
        }
        tracing::info!(target: "pyUltroid.webserver", "SESSION saved to .env (value never leaves the server).");
    }

    /// Persist the API keys plus the optional bot token / log channel.
    ///
    /// The empty-string and zero sentinels matter: they tell the bootstrap
    /// gate "the user was asked and said no", which is different from
    /// "nobody has answered yet".
    pub async fn save_credentials(
        &self,
        api_id: i64,
        api_hash: &str,
        bot_token: Option<&str>,
        log_channel: Option<i64>,
    ) {
        let mut state = self.state.lock().await;
        let env = state.env();
        let mut updates: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        updates.insert("API_ID".to_string(), api_id.to_string());
        updates.insert("API_HASH".to_string(), api_hash.to_string());
        if let Some(token) = bot_token {
            updates.insert("BOT_TOKEN".to_string(), token.to_string());
        } else if env
            .read()
            .get("BOT_TOKEN")
            .map(|v| v.is_empty())
            .unwrap_or(true)
            && std::env::var("BOT_TOKEN")
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            updates.insert("BOT_TOKEN".to_string(), String::new());
        }
        let mut log_channel = log_channel;
        if log_channel.is_none()
            && env
                .read()
                .get("LOG_CHANNEL")
                .map(|v| v.is_empty())
                .unwrap_or(true)
            && std::env::var("LOG_CHANNEL")
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            log_channel = Some(0);
        }
        if let Some(channel) = log_channel {
            updates.insert("LOG_CHANNEL".to_string(), channel.to_string());
        }
        if let Err(err) = env.write(&updates, true) {
            tracing::warn!(target: "pyUltroid.webserver", "credential persist failed: {err}");
            return;
        }
        state.api_id = Some(api_id as i32);
        state.api_hash = Some(api_hash.to_string());
        state.bot_token = bot_token.map(str::to_string);
        state.log_channel = log_channel;
        self.authority.redactor().add(api_hash);
        if let Some(token) = bot_token {
            self.authority.redactor().add(token);
        }
        tracing::info!(target: "pyUltroid.webserver", "API credentials saved (api_id={api_id}).");
    }

    /// Construct the axum router and register every route.
    /// The hub stays attached for the lifetime of the app: that attachment
    /// is what feeds the `/api/logs` stream. `close()` reverses it.
    pub async fn build_router(self: &Arc<Self>) -> Router {
        {
            let mut hub = self.hub.lock().await;
            let redactor = self.authority.redactor().clone();
            hub.attach(redactor);
        }
        let state = self.clone();
        Router::new()
            .route("/status", get(status))
            .route("/", get(root))
            .route("/api/state", get(api_state))
            .route("/api/setup/keys", post(setup_keys))
            .route("/api/login/phone", post(login_phone))
            .route("/api/login/otp", post(login_otp))
            .route("/api/login/qr", post(login_qr))
            .route("/api/login/qr/{login_id}/wait", get(login_qr_wait))
            .route("/api/login/qr/{login_id}/password", post(login_qr_password))
            .route("/api/login/qr/{login_id}/image", get(login_qr_image))
            .route("/api/logs", get(stream_logs))
            .route("/api/setup/complete", post(setup_complete))
            .with_state(state.clone())
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
            .layer(middleware::from_fn_with_state(state, token_gate))
    }

    /// Detach the log hub and drop any live login clients.
    pub async fn close(&self) -> bool {
        let detached = {
            let mut hub = self.hub.lock().await;
            hub.detach()
        };
        let manager = {
            let state = self.state.lock().await;
            state.login_manager.clone()
        };
        if let Some(manager) = manager {
            manager.dispose().await;
        }
        detached
    }
}

// -- route handlers ------------------------------------------------------

async fn status() -> Json<Value> {
    // Unauthenticated on purpose: keep the exact legacy shape.
    Json(json!({ "message": "running" }))
}

async fn root() -> Html<&'static str> {
    Html(SETUP_PAGE)
}

async fn api_state(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let state = app.state.lock().await;
    let env = state.env().read();
    Json(json!({
        "setup_required": !(state.has_session() || env.get("SESSION").map(|v| !v.is_empty()).unwrap_or(false)),
        "has_api_keys": state.has_api_keys() || (env.get("API_ID").is_some() && env.get("API_HASH").is_some()),
        "has_bot_token": state.bot_token.is_some() || env.get("BOT_TOKEN").map(|v| !v.is_empty()).unwrap_or(false),
        "has_session": state.has_session() || env.get("SESSION").map(|v| !v.is_empty()).unwrap_or(false),
        "completed": state.completed,
        "standalone": state.standalone,
    }))
    .into_response()
}

fn bot_token_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(BOT_TOKEN_PATTERN).expect("static bot token pattern"))
}

fn reject_keys(payload: &KeysPayload, api_hash: &str) -> Option<(StatusCode, Json<Value>)> {
    if payload.api_id <= 0 {
        return Some((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": "api_id must be a positive integer" })),
        ));
    }
    if api_hash.len() < API_HASH_MIN_LENGTH || !api_hash.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Some((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": "api_hash looks invalid" })),
        ));
    }
    let bot_token = payload.bot_token.as_deref().unwrap_or("").trim();
    if !bot_token.is_empty() && !bot_token_re().is_match(bot_token) {
        return Some((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": "bot_token format is invalid" })),
        ));
    }
    None
}

async fn setup_keys(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<KeysPayload>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    // The payload arrives through the Json extractor after the gate; the
    // handler signature order above keeps the gate's 401 ahead of any 422.
    let api_hash = payload.api_hash.trim().to_string();
    if let Some(invalid) = reject_keys(&payload, &api_hash) {
        return invalid.into_response();
    }
    let bot_token = payload
        .bot_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    app.save_credentials(payload.api_id, &api_hash, bot_token, payload.log_channel)
        .await;
    Json(json!({ "ok": true })).into_response()
}

async fn login_phone(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<PhonePayload>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    if !app
        .phone_limiter
        .allow(&SetupApplication::client_ip(&headers))
    {
        return too_many().into_response();
    }
    let manager = {
        let mut state = app.state.lock().await;
        state.manager_or_none()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "detail": "save API_ID/API_HASH first" })),
        )
            .into_response();
    };
    let phone = payload.phone.trim().to_string();
    // Phone numbers never appear in any log output.
    app.authority.redactor().add(&phone);
    match manager.start_phone(&phone).await {
        Ok(result) => {
            let tail = phone
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            tracing::info!(target: "pyUltroid.webserver", "login code sent to phone ending …{tail}");
            Json(json!({ "login_id": result.login_id, "code_type": Value::Null })).into_response()
        }
        Err(err) => LoginErrorMapper.response(&err).into_response(),
    }
}

async fn login_otp(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<OtpPayload>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    if !app
        .otp_limiter
        .allow(&SetupApplication::client_ip(&headers))
    {
        return too_many().into_response();
    }
    let manager = {
        let state = app.state.lock().await;
        state.login_manager.clone()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "detail": "no login in progress" })),
        )
            .into_response();
    };
    if let Some(password) = &payload.password {
        app.authority.redactor().add(password);
    }
    match manager
        .submit_code(
            &payload.login_id,
            payload.code.trim(),
            payload.password.as_deref(),
        )
        .await
    {
        Ok(outcome) => {
            if outcome.needs_password {
                return (
                    StatusCode::from_u16(428).unwrap(),
                    Json(
                        json!({ "detail": "two-factor password required", "needs_password": true }),
                    ),
                )
                    .into_response();
            }
            if let Some(session_path) = outcome.session_string {
                app.store_session(PathBuf::from(session_path)).await;
            }
            Json(json!({ "ok": true })).into_response()
        }
        Err(err) => LoginErrorMapper.response(&err).into_response(),
    }
}

async fn login_qr(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let manager = {
        let mut state = app.state.lock().await;
        state.manager_or_none()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "detail": "save API_ID/API_HASH first" })),
        )
            .into_response();
    };
    match manager.start_qr().await {
        Ok(result) => {
            tracing::info!(target: "pyUltroid.webserver", "QR login started (expires {}).", result.expires.to_rfc3339());
            Json(json!({
                "login_id": result.login_id,
                "url": result.url,
                "expires": result.expires.to_rfc3339(),
            }))
            .into_response()
        }
        Err(err) => LoginErrorMapper.response(&err).into_response(),
    }
}

async fn login_qr_wait(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    AxumPath(login_id): AxumPath<String>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let manager = {
        let state = app.state.lock().await;
        state.login_manager.clone()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "unknown or expired login_id" })),
        )
            .into_response();
    };
    if !manager.peek(&login_id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "unknown or expired login_id" })),
        )
            .into_response();
    }
    let (tx, mut rx) =
        mpsc::unbounded_channel::<std::result::Result<Event, std::convert::Infallible>>();
    let app_clone = app.clone();
    tokio::spawn(async move {
        let _ = tx.send(Ok(sse_frame("waiting", &json!({ "login_id": login_id }))));
        for _cycle in 0..MAX_QR_CYCLES {
            match manager.wait_qr(&login_id).await {
                Ok(QrWait::Expired { url, expires }) => {
                    if tx
                        .send(Ok(sse_frame(
                            "expired",
                            &json!({ "url": url, "expires": expires.to_rfc3339() }),
                        )))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(QrWait::NeedsPassword) => {
                    let _ = tx.send(Ok(sse_frame("needs_password", &json!({}))));
                    return;
                }
                Ok(QrWait::Success { session_string }) => {
                    app_clone.store_session(PathBuf::from(session_string)).await;
                    let _ = tx.send(Ok(sse_frame("success", &json!({}))));
                    return;
                }
                Err(LoginError::UnknownSession(_)) => {
                    let _ = tx.send(Ok(sse_frame(
                        "error",
                        &json!({ "detail": "login session expired" }),
                    )));
                    return;
                }
                Err(err) => {
                    tracing::warn!(target: "pyUltroid.webserver", "QR wait failed: {err}");
                    let _ = tx.send(Ok(sse_frame(
                        "error",
                        &json!({ "detail": format!("{err:?}") }),
                    )));
                    return;
                }
            }
        }
        let _ = tx.send(Ok(sse_frame(
            "error",
            &json!({ "detail": "too many QR expiries - start over" }),
        )));
    });
    let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Sse::new(stream).into_response()
}

async fn login_qr_password(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    AxumPath(login_id): AxumPath<String>,
    Json(payload): Json<PasswordPayload>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    if !app
        .otp_limiter
        .allow(&SetupApplication::client_ip(&headers))
    {
        return too_many().into_response();
    }
    let manager = {
        let state = app.state.lock().await;
        state.login_manager.clone()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "unknown or expired login_id" })),
        )
            .into_response();
    };
    if !manager.peek(&login_id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "unknown or expired login_id" })),
        )
            .into_response();
    }
    app.authority.redactor().add(&payload.password);
    match manager.submit_password(&login_id, &payload.password).await {
        Ok(outcome) => {
            if let Some(session_path) = outcome.session_string {
                app.store_session(PathBuf::from(session_path)).await;
            }
            Json(json!({ "ok": true })).into_response()
        }
        Err(err) => LoginErrorMapper.response(&err).into_response(),
    }
}

async fn login_qr_image(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
    AxumPath(login_id): AxumPath<String>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let manager = {
        let state = app.state.lock().await;
        state.login_manager.clone()
    };
    let Some(manager) = manager else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": "unknown or expired login_id" })),
        )
            .into_response();
    };
    let url = match manager.qr_url(&login_id).await {
        Ok(url) => url,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "detail": "unknown or expired login_id" })),
            )
                .into_response();
        }
    };
    // PNG of the tg:// login URL. The qrcode crate is a hard dependency
    // here (Python made it optional and 404'd without it); the UI falls
    // back to showing the URL as text either way.
    match render_qr_png(&url) {
        Ok(png) => {
            let mut response = Response::new(Body::from(png));
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                CONTENT_TYPE,
                axum::http::HeaderValue::from_static("image/png"),
            );
            response
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "detail": format!("QR render failed: {err}"), "url": url })),
        )
            .into_response(),
    }
}

fn render_qr_png(url: &str) -> std::result::Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    use std::io::Cursor;
    let code = qrcode::QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    let image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(300, 300)
        .build();
    let mut buf = Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            &image,
            image.width(),
            image.height(),
            image::ExtendedColorType::L8,
        )
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

async fn stream_logs(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let broker = app.broker.clone();
    let keepalive = app.log_keepalive_seconds;
    let (tx, mut rx) =
        mpsc::unbounded_channel::<std::result::Result<Event, std::convert::Infallible>>();
    tokio::spawn(async move {
        for line in broker.history() {
            if tx.send(Ok(sse_frame("log", &json!(line)))).is_err() {
                return;
            }
        }
        let _ = tx.send(Ok(Event::default().comment("ready")));
        let mut sub = broker.subscribe();
        loop {
            match sub.recv().await {
                Ok(line) => {
                    if tx.send(Ok(sse_frame("log", &json!(line)))).is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs_f64(keepalive))
                .text("keep-alive"),
        )
        .into_response()
}

async fn setup_complete(
    State(app): State<Arc<SetupApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")) {
        return unauthorized().into_response();
    }
    let has_session = {
        let state = app.state.lock().await;
        state.has_session()
            || state
                .env()
                .read()
                .get("SESSION")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
    };
    if !has_session {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "detail": "no SESSION yet - finish phone or QR login first" })),
        )
            .into_response();
    }
    let standalone = {
        let mut state = app.state.lock().await;
        state.trigger_completion();
        state.standalone
    };
    let mode = if standalone { "spawn" } else { "exec" };
    tracing::info!(target: "pyUltroid.webserver", "setup complete - bot restart handshake ({mode}).");
    Json(json!({ "ok": true, "restart": true, "mode": mode })).into_response()
}

// -- module surface -------------------------------------------------------

/// Build the setup/status app (the historical entry point).
pub async fn create_app(
    token: Option<String>,
    env_path: Option<impl AsRef<Path>>,
    standalone: bool,
    on_complete: Option<OnComplete>,
    client_factory: Option<Arc<dyn ClientFactory>>,
) -> Router {
    let app = SetupApplication::new(
        token,
        env_path,
        standalone,
        on_complete,
        client_factory,
        None::<&Path>,
        LOG_KEEPALIVE_SECONDS,
    );
    app.build_router().await
}
