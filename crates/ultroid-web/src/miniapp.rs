//! Mini App settings dashboard.
//!
//! Port of `pyUltroid/webserver/miniapp.py` — four types, each answering
//! to a different collaborator:
//!
//! * [`MemoryStore`] is the fallback key/value store used when no live
//!   database exists. Explicit about being ephemeral: nothing here survives
//!   a restart.
//! * [`MiniAppStore`] owns *storage semantics*: the persisted token, the
//!   validated UI preferences, and the toggle rows read out of a
//!   [`ToggleRegistry`].
//! * [`MiniAppServer`] is the supervisor that starts at most one server —
//!   the lock and the cached server are instance state, not module globals.
//! * [`MiniAppApplication`] builds the axum app and its three routes.
//!
//! Rust differences:
//! * Python's `udB` is an untyped `get_key`/`set_key` bag; here it is the
//!   [`MiniAppDb`] trait (async, JSON values). The live implementation
//!   wraps `ultroid_core`'s `Arc<dyn Database>`; the fallback is
//!   [`MemoryStore`].
//! * Python's manifest registry (`core.manifest._entries` + `flip_toggle`)
//!   is the [`ToggleRegistry`] trait; the binary crate adapts the real
//!   plugin registry to it. With no registry, the dashboard simply shows
//!   no plugin toggles — the store never reaches for a global.
//! * The SPA page is rewritten (same behaviour: theme/cards/buttons pills,
//!   plugin toggles, token scrubbed from the URL), not copied.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use axum::{
    Router,
    extract::{Json, State},
    http::{HeaderMap, StatusCode, Uri},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use ultroid_core::{Database, Error};

use crate::runner::{PortProbe, RunningServer};
use crate::security::{TokenAuthority, authority as global_authority};

pub const MINIAPP_TOKEN_KEY: &str = "MINIAPP_TOKEN";
pub const MINIAPP_URL_KEY: &str = "MINIAPP_URL";
pub const TOKEN_HEADER: &str = "X-Miniapp-Token";
pub const TOKEN_QUERY_PARAM: &str = "token";

pub const THEMES: &[&str] = &["default", "dark"];
pub const BUTTON_STYLES: &[&str] = &["classic", "styled"];
pub const DEFAULT_THEME: &str = "default";
pub const DEFAULT_BUTTON_STYLE: &str = "styled";
pub const DEFAULT_CARDS: bool = true;

pub const DEFAULT_MINIAPP_PORT: u16 = 7861;
pub const DEFAULT_BIND: &str = "0.0.0.0";

// -- storage ---------------------------------------------------------------

/// The key/value surface the dashboard needs. Values are JSON so a `bool`
/// preference and a `str` token share one store.
#[async_trait]
pub trait MiniAppDb: Send + Sync {
    async fn get_key(&self, key: &str) -> Option<Value>;
    async fn set_key(&self, key: &str, value: Value);
    async fn del_key(&self, key: &str);
}

/// In-memory `get_key` / `set_key` / `del_key` stand-in for the live db.
///
/// Only used when the bot is not running in-process. Explicit about being
/// ephemeral: nothing here survives a restart.
#[derive(Debug, Default)]
pub struct MemoryStore {
    data: std::sync::Mutex<HashMap<String, Value>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) {
        self.data
            .lock()
            .expect("memory store lock poisoned")
            .clear();
    }

    pub fn len(&self) -> usize {
        self.data.lock().expect("memory store lock poisoned").len()
    }
}

#[async_trait]
impl MiniAppDb for MemoryStore {
    async fn get_key(&self, key: &str) -> Option<Value> {
        self.data
            .lock()
            .expect("memory store lock poisoned")
            .get(key)
            .cloned()
    }

    async fn set_key(&self, key: &str, value: Value) {
        self.data
            .lock()
            .expect("memory store lock poisoned")
            .insert(key.to_string(), value);
    }

    async fn del_key(&self, key: &str) {
        self.data
            .lock()
            .expect("memory store lock poisoned")
            .remove(key);
    }
}

/// A live database adapted to the dashboard's key/value surface.
pub struct LiveDb {
    db: Arc<dyn Database>,
}

impl LiveDb {
    pub fn new(db: Arc<dyn Database>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MiniAppDb for LiveDb {
    async fn get_key(&self, key: &str) -> Option<Value> {
        match self.db.get(key).await {
            Ok(Some(text)) => serde_json::from_str(&text).ok(),
            _ => None,
        }
    }

    async fn set_key(&self, key: &str, value: Value) {
        let _ = self.db.set(key, &value.to_string()).await;
    }

    async fn del_key(&self, key: &str) {
        let _ = self.db.del(key).await;
    }
}

// -- toggle registry ---------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ToggleEntry {
    pub key: String,
    pub label: String,
    pub plugin: String,
    pub icon: String,
    pub description: String,
}

/// The manifest's toggle surface, as the dashboard needs it. The binary
/// crate adapts the real plugin registry (`ultroid-core`'s
/// `ManifestRegistry` plus the plugin toggle entries) to this trait.
#[async_trait]
pub trait ToggleRegistry: Send + Sync {
    fn toggle_entries(&self) -> Vec<ToggleEntry>;
    fn toggle_state(&self, key: &str) -> bool;
    async fn flip_toggle(&self, key: &str) -> Result<bool, Error>;
}

// -- the store ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct UiState {
    pub theme: String,
    pub cards: bool,
    pub buttons: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToggleRow {
    pub key: String,
    pub label: String,
    pub plugin: String,
    pub icon: String,
    pub description: String,
    pub enabled: bool,
}

/// Storage semantics for the dashboard: token, UI prefs, toggle rows.
///
/// A Mini App token is re-read from the database on *every* request, which
/// is what makes rotation take effect immediately instead of at the next
/// restart.
pub struct MiniAppStore {
    pub db: Arc<dyn MiniAppDb>,
    pub registry: Option<Arc<dyn ToggleRegistry>>,
    pub live_db: bool,
    warned: AtomicBool,
}

impl MiniAppStore {
    pub fn new(db: Option<Arc<dyn MiniAppDb>>, registry: Option<Arc<dyn ToggleRegistry>>) -> Self {
        match db {
            Some(db) => Self {
                db,
                registry,
                live_db: true,
                warned: AtomicBool::new(false),
            },
            None => Self {
                db: Arc::new(MemoryStore::new()),
                registry,
                live_db: false,
                warned: AtomicBool::new(false),
            },
        }
    }

    pub fn warn_if_ephemeral(&self) {
        if self.live_db || self.warned.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(target: "pyUltroid.webserver.miniapp", "miniapp: live database unavailable - using an in-memory fallback store (expected outside the bot process).");
    }

    // -- token

    /// The persisted token, minting and storing one on first use.
    pub async fn ensure_token(&self) -> String {
        if let Some(Value::String(token)) = self.db.get_key(MINIAPP_TOKEN_KEY).await {
            if !token.is_empty() {
                return token;
            }
        }
        let token = global_authority().mint(false);
        self.db
            .set_key(MINIAPP_TOKEN_KEY, Value::String(token.clone()))
            .await;
        token
    }

    /// Replace the token. Every previously issued link stops working.
    pub async fn rotate_token(&self) -> String {
        let token = global_authority().mint(false);
        self.db
            .set_key(MINIAPP_TOKEN_KEY, Value::String(token.clone()))
            .await;
        token
    }

    /// Force a caller-supplied token to be the persisted one.
    pub async fn adopt_token(&self, token: &str) -> String {
        self.db
            .set_key(MINIAPP_TOKEN_KEY, Value::String(token.to_string()))
            .await;
        token.to_string()
    }

    // -- preferences

    /// Validated theme / cards / buttons, falling back to the defaults.
    pub async fn ui_state(&self) -> UiState {
        let theme = match self.db.get_key("UI_THEME").await {
            Some(Value::String(t)) if THEMES.contains(&t.as_str()) => t,
            _ => DEFAULT_THEME.to_string(),
        };
        let cards = match self.db.get_key("UI_CARDS").await {
            Some(Value::Bool(b)) => b,
            _ => DEFAULT_CARDS,
        };
        let buttons = match self.db.get_key("UI_BUTTONS").await {
            Some(Value::String(b)) if BUTTON_STYLES.contains(&b.as_str()) => b,
            _ => DEFAULT_BUTTON_STYLE.to_string(),
        };
        UiState {
            theme,
            cards,
            buttons,
        }
    }

    /// The manifest's toggle entries, sorted by key; one bad row is
    /// skipped, never fatal — the trait's `toggle_entries` is already
    /// validated, so this is just the row mapping.
    pub fn toggle_rows(&self) -> Vec<ToggleRow> {
        let Some(registry) = &self.registry else {
            return Vec::new();
        };
        let mut rows: Vec<ToggleRow> = registry
            .toggle_entries()
            .into_iter()
            .map(|entry| ToggleRow {
                enabled: registry.toggle_state(&entry.key),
                key: entry.key,
                label: entry.label,
                plugin: entry.plugin,
                icon: entry.icon,
                description: entry.description,
            })
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        rows
    }

    /// Everything the dashboard renders, in one object.
    pub async fn snapshot(&self) -> Value {
        json!({
            "ui": self.ui_state().await,
            "themes": THEMES,
            "button_styles": BUTTON_STYLES,
            "pmf_toggles": self.toggle_rows(),
        })
    }

    // -- writes

    pub async fn cycle_theme(&self) -> String {
        self.cycle("UI_THEME", THEMES, DEFAULT_THEME).await
    }

    pub async fn cycle_buttons(&self) -> String {
        self.cycle("UI_BUTTONS", BUTTON_STYLES, DEFAULT_BUTTON_STYLE)
            .await
    }

    pub async fn flip_cards(&self) -> bool {
        let current = match self.db.get_key("UI_CARDS").await {
            Some(Value::Bool(b)) => b,
            _ => DEFAULT_CARDS,
        };
        let next = !current;
        self.db.set_key("UI_CARDS", Value::Bool(next)).await;
        next
    }

    /// Delegates to the manifest registry; errors on a missing registry or
    /// a bad key.
    pub async fn flip_manifest_toggle(&self, key: &str) -> Result<(), Error> {
        let Some(registry) = &self.registry else {
            return Err(Error::Other("no toggle registry configured".into()));
        };
        registry.flip_toggle(key).await?;
        Ok(())
    }

    async fn cycle(&self, key: &str, options: &[&str], default: &str) -> String {
        let current = match self.db.get_key(key).await {
            Some(Value::String(v)) if options.contains(&v.as_str()) => v,
            _ => default.to_string(),
        };
        let idx = options.iter().position(|o| o == &current).unwrap_or(0);
        let next = options[(idx + 1) % options.len()].to_string();
        self.db.set_key(key, Value::String(next.clone())).await;
        next
    }
}

// -- the server supervisor ---------------------------------------------------

/// Supervises at most one in-process Mini App server.
///
/// The lock, the cached server and the port decision all live on the
/// instance, so tests get an isolated supervisor simply by constructing
/// their own instead of resetting module globals.
pub struct MiniAppServer {
    server: AsyncMutex<Option<RunningServer>>,
}

impl MiniAppServer {
    pub fn new() -> Self {
        Self {
            server: AsyncMutex::new(None),
        }
    }

    pub async fn running(&self) -> bool {
        self.server
            .lock()
            .await
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// Idempotently start `router`. Best-effort, never raises: returns
    /// `true` when a server is running afterwards, `false` when the port
    /// is already taken by something else.
    pub async fn ensure(&self, router: Router, host: &str, port: u16) -> bool {
        let mut guard = self.server.lock().await;
        if let Some(server) = guard.as_ref() {
            if server.is_running() {
                return true;
            }
        }
        if !PortProbe.bindable(host, port) {
            tracing::warn!(target: "pyUltroid.webserver.miniapp", "miniapp: {host}:{port} already in use - not starting a second Mini App server.");
            return false;
        }
        match RunningServer::start(router, host, port).await {
            Ok(server) => {
                *guard = Some(server);
                true
            }
            Err(err) => {
                tracing::warn!(target: "pyUltroid.webserver.miniapp", "miniapp: failed to start: {err}");
                false
            }
        }
    }

    pub async fn stop(&self, timeout: Option<std::time::Duration>) -> bool {
        let mut guard = self.server.lock().await;
        let Some(mut server) = guard.take() else {
            return false;
        };
        server.stop(timeout).await
    }
}

impl Default for MiniAppServer {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide supervisor (Python's module-level `SUPERVISOR`).
pub static SUPERVISOR: std::sync::OnceLock<MiniAppServer> = std::sync::OnceLock::new();

pub fn supervisor() -> &'static MiniAppServer {
    SUPERVISOR.get_or_init(MiniAppServer::new)
}

// -- the application ---------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TogglePayload {
    pub kind: String,
    pub key: Option<String>,
}

/// The handle the axum app exposes as state (Python's `app.state.miniapp`).
pub struct MiniAppState {
    pub store: Arc<MiniAppStore>,
    pub authority: Arc<TokenAuthority>,
}

impl MiniAppState {
    pub fn db(&self) -> &Arc<dyn MiniAppDb> {
        &self.store.db
    }

    pub fn registry(&self) -> &Option<Arc<dyn ToggleRegistry>> {
        &self.store.registry
    }

    pub fn live_udb(&self) -> bool {
        self.store.live_db
    }
}

/// The Mini App axum app: one page, one state route, one toggle route.
pub struct MiniAppApplication {
    pub store: Arc<MiniAppStore>,
    pub authority: Arc<TokenAuthority>,
    pub page: &'static str,
}

impl MiniAppApplication {
    pub async fn new(
        token: Option<String>,
        db: Option<Arc<dyn MiniAppDb>>,
        registry: Option<Arc<dyn ToggleRegistry>>,
        authority: Option<Arc<TokenAuthority>>,
        page: Option<&'static str>,
    ) -> Arc<Self> {
        let authority = authority.unwrap_or_else(global_authority);
        let store = Arc::new(MiniAppStore::new(db, registry));
        store.warn_if_ephemeral();
        let app = Arc::new(Self {
            store,
            authority,
            page: page.unwrap_or(MINIAPP_PAGE),
        });
        // Adopt a caller-supplied token before any request can arrive
        // (otherwise the first request would mint a different one);
        // otherwise ensure one exists now.
        if let Some(token) = token {
            app.store.adopt_token(&token).await;
        } else {
            app.store.ensure_token().await;
        }
        app
    }

    pub fn state(&self) -> MiniAppState {
        MiniAppState {
            store: self.store.clone(),
            authority: self.authority.clone(),
        }
    }

    /// Re-resolve the expected token each time, so rotation is instant.
    pub async fn authorized(&self, headers: &HeaderMap, query: &str) -> bool {
        let expected = self.store.ensure_token().await;
        self.authority.permits(headers, query, &expected)
    }

    pub fn build_router(self: &Arc<Self>) -> Router {
        async fn security_headers(
            request: axum::http::Request<axum::body::Body>,
            next: Next,
        ) -> Response {
            let mut response = next.run(request).await;
            response.headers_mut().insert(
                "cache-control",
                axum::http::HeaderValue::from_static("no-store"),
            );
            response.headers_mut().insert(
                "referrer-policy",
                axum::http::HeaderValue::from_static("no-referrer"),
            );
            response
        }

        let app = self.clone();
        Router::new()
            .route("/settings", get(settings_page))
            .route("/settings/api/state", get(get_state))
            .route("/settings/api/toggle", post(post_toggle))
            .with_state(app)
            .layer(middleware::from_fn(security_headers))
    }
}

fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "detail": "invalid or missing miniapp token" })),
    )
}

async fn settings_page(
    State(app): State<Arc<MiniAppApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")).await {
        return unauthorized().into_response();
    }
    Html(app.page).into_response()
}

async fn get_state(
    State(app): State<Arc<MiniAppApplication>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")).await {
        return unauthorized().into_response();
    }
    Json(app.store.snapshot().await).into_response()
}

async fn post_toggle(
    State(app): State<Arc<MiniAppApplication>>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<TogglePayload>,
) -> Response {
    if !app.authorized(&headers, uri.query().unwrap_or("")).await {
        return unauthorized().into_response();
    }
    match apply_toggle(&app.store, &payload).await {
        Ok(()) => Json(app.store.snapshot().await).into_response(),
        Err((status, detail)) => (status, Json(json!({ "detail": detail }))).into_response(),
    }
}

async fn apply_toggle(
    store: &MiniAppStore,
    payload: &TogglePayload,
) -> std::result::Result<(), (StatusCode, String)> {
    match payload.kind.as_str() {
        "ui_theme" => {
            store.cycle_theme().await;
        }
        "ui_cards" => {
            store.flip_cards().await;
        }
        "ui_buttons" => {
            store.cycle_buttons().await;
        }
        "pmf" => {
            let Some(key) = &payload.key else {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "key is required for kind=pmf".into(),
                ));
            };
            if let Err(err) = store.flip_manifest_toggle(key).await {
                tracing::error!(target: "pyUltroid.webserver.miniapp", "miniapp: flip_toggle failed for {key:?}: {err}");
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "toggle failed".into()));
            }
        }
        other => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("unknown kind {other:?}"),
            ));
        }
    }
    Ok(())
}

// -- module surface ----------------------------------------------------------

/// Build the Mini App (the historical entry point).
pub async fn create_miniapp(
    token: Option<String>,
    db: Option<Arc<dyn MiniAppDb>>,
    registry: Option<Arc<dyn ToggleRegistry>>,
) -> Router {
    MiniAppApplication::new(token, db, registry, None, None)
        .await
        .build_router()
}

/// Idempotently start `app` on the process-wide supervisor.
///
/// Returns whether a server is running afterwards; `false` when the port
/// is already taken by something else (best-effort — never raises).
pub async fn ensure_running(router: Router, host: &str, port: u16) -> bool {
    supervisor().ensure(router, host, port).await
}

/// Return the persisted Mini App token, generating one on first use.
pub async fn ensure_token(db: Option<Arc<dyn MiniAppDb>>) -> String {
    MiniAppStore::new(db, None).ensure_token().await
}

pub async fn rotate_token(db: Option<Arc<dyn MiniAppDb>>) -> String {
    MiniAppStore::new(db, None).rotate_token().await
}

pub async fn snapshot(
    db: Option<Arc<dyn MiniAppDb>>,
    registry: Option<Arc<dyn ToggleRegistry>>,
) -> Value {
    MiniAppStore::new(db, registry).snapshot().await
}

// -- the SPA itself (single self-contained page) ---------------------------

/// The settings dashboard page: a port of the *behaviour* of Python's
/// `_PAGE_HTML` — theme/cards/buttons pills, plugin toggles, the token
/// scrubbed from the address bar on load, every API call carrying
/// `X-Miniapp-Token` — rewritten, not copied. It keeps the
/// `telegram-web-app.js` client script: the page runs inside Telegram,
/// and that script is how it reads the client's theme.
pub const MINIAPP_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Cache-Control" content="no-store">
<title>Ultroid Settings</title>
<script src="https://telegram.org/js/telegram-web-app.js"></script>
<style>
  :root { --bg: #f6f6fb; --text: #17181a; --hint: #8e8e93; --link: #5b5bf5; --button: #5b5bf5; --button-text: #ffffff; --secondary-bg: #ffffff; }
  * { box-sizing: border-box; }
  body { margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background: var(--bg); color: var(--text); padding: 14px; line-height: 1.4; }
  h1 { font-size: 18px; font-weight: 650; margin: 4px 0 14px; letter-spacing: -0.01em; }
  .card { background: var(--secondary-bg); border: 1px solid rgba(120,120,128,0.14); border-radius: 14px; box-shadow: 0 1px 2px rgba(20,20,40,0.04), 0 6px 16px -10px rgba(20,20,40,0.12); padding: 6px 14px; margin-bottom: 12px; }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 0; border-bottom: 1px solid rgba(120,120,128,0.16); }
  .row:last-child { border-bottom: none; }
  .row .label { font-size: 15px; font-weight: 550; }
  .row .sub { font-size: 12px; color: var(--hint); margin-top: 2px; }
  button.pill { border: 1px solid transparent; border-radius: 999px; padding: 7px 16px; font: inherit; font-size: 13px; font-weight: 600; background: var(--button); color: var(--button-text); cursor: pointer; transition: opacity 0.15s ease, transform 0.05s ease; }
  button.pill:active { transform: translateY(1px); opacity: 0.9; }
  button.pill.off { background: rgba(120,120,128,0.16); color: var(--text); border-color: rgba(120,120,128,0.22); }
  #status { font-size: 12px; color: var(--hint); margin-top: 10px; text-align: center; }
  #error { color: #cc2929; background: rgba(204,41,41,0.08); border: 1px solid rgba(204,41,41,0.2); border-radius: 10px; font-size: 13px; padding: 8px 12px; margin: 0 0 10px; display: none; }
</style>
</head>
<body>
  <h1>&#9881; SarahUB Settings</h1>
  <div id="error"></div>
  <div class="card">
    <div class="row">
      <div><div class="label">Theme</div><div class="sub">Overall UI color scheme</div></div>
      <button class="pill" id="btn-theme" data-kind="ui_theme">&hellip;</button>
    </div>
    <div class="row">
      <div><div class="label">Rich Cards</div><div class="sub">PIL-rendered cards vs plain rich messages</div></div>
      <button class="pill" id="btn-cards" data-kind="ui_cards">&hellip;</button>
    </div>
    <div class="row">
      <div><div class="label">Buttons</div><div class="sub">Classic vs styled inline buttons</div></div>
      <button class="pill" id="btn-buttons" data-kind="ui_buttons">&hellip;</button>
    </div>
  </div>
  <div class="card" id="pmf-card" style="display:none;">
    <div class="label" style="margin-bottom: 6px;">Plugin Toggles</div>
    <div id="pmf-list"></div>
  </div>
  <div id="status">loading&hellip;</div>
<script>
(function () {
  "use strict";
  var tg = window.Telegram && window.Telegram.WebApp;
  if (tg) {
    try {
      tg.ready();
      tg.expand();
      var map = { bg_color: "--bg", text_color: "--text", hint_color: "--hint", link_color: "--link", button_color: "--button", button_text_color: "--button-text", secondary_bg_color: "--secondary-bg" };
      var applyTheme = function () {
        var tp = tg.themeParams || {};
        Object.keys(map).forEach(function (k) {
          if (tp[k]) document.documentElement.style.setProperty(map[k], tp[k]);
        });
      };
      applyTheme();
      tg.onEvent("themeChanged", applyTheme);
    } catch (e) { /* best-effort only */ }
  }
  var params = new URLSearchParams(window.location.search);
  var token = params.get("token") || "";
  // Read the token into memory (used for every subsequent /settings/api/*
  // call via the X-Miniapp-Token header), then immediately scrub it from
  // the visible address bar. history.replaceState does not re-request the
  // page, so `token` above stays intact for the rest of this session.
  if (token && window.history && window.history.replaceState) {
    try { window.history.replaceState(null, "", window.location.pathname); } catch (e) { /* best-effort only */ }
  }
  var statusEl = document.getElementById("status");
  var errorEl = document.getElementById("error");
  function showError(msg) { errorEl.textContent = msg; errorEl.style.display = "block"; }
  function api(path, opts) {
    opts = opts || {};
    opts.headers = Object.assign({ "X-Miniapp-Token": token }, opts.headers || {});
    return fetch(path, opts).then(function (res) {
      if (res.status === 401) throw new Error("Unauthorized - reopen from the bot.");
      if (!res.ok) throw new Error("Request failed (" + res.status + ")");
      return res.json();
    });
  }
  function pill(el, on, onText, offText) {
    el.textContent = on ? onText : offText;
    el.classList.toggle("off", !on);
  }
  function escapeHtml(s) {
    var d = document.createElement("div");
    d.textContent = s || "";
    return d.innerHTML;
  }
  function render(state) {
    pill(document.getElementById("btn-theme"), state.ui.theme === "dark", "dark", "default");
    pill(document.getElementById("btn-cards"), state.ui.cards, "on", "off");
    pill(document.getElementById("btn-buttons"), state.ui.buttons === "styled", "styled", "classic");
    var list = document.getElementById("pmf-list");
    var card = document.getElementById("pmf-card");
    list.innerHTML = "";
    if (state.pmf_toggles && state.pmf_toggles.length) {
      card.style.display = "block";
      state.pmf_toggles.forEach(function (t) {
        var row = document.createElement("div");
        row.className = "row";
        var left = document.createElement("div");
        left.innerHTML = '<div class="label">' + (t.icon ? t.icon + " " : "") + escapeHtml(t.label) + '</div><div class="sub">' + escapeHtml(t.plugin) + '</div>';
        var btn = document.createElement("button");
        btn.className = "pill" + (t.enabled ? "" : " off");
        btn.textContent = t.enabled ? "on" : "off";
        btn.onclick = function () { toggle("pmf", t.key); };
        row.appendChild(left);
        row.appendChild(btn);
        list.appendChild(row);
      });
    } else {
      card.style.display = "none";
    }
    statusEl.textContent = "synced";
  }
  function toggle(kind, key) {
    statusEl.textContent = "saving\u2026";
    var body = { kind: kind };
    if (key) body.key = key;
    api("/settings/api/toggle", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
      .then(render)
      .catch(function (err) { showError(err.message); statusEl.textContent = "error"; });
  }
  ["btn-theme", "btn-cards", "btn-buttons"].forEach(function (id) {
    document.getElementById(id).addEventListener("click", function () {
      toggle(this.getAttribute("data-kind"), null);
    });
  });
  api("/settings/api/state").then(render).catch(function (err) {
    showError(err.message);
    statusEl.textContent = "error";
  });
})();
</script>
</body>
</html>
"#;
