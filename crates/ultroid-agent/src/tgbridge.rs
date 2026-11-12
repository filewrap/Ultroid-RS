//! Telegram bridge for the agent tool layer — port of
//! `pyUltroid/agent/_tgbridge.py`.
//!
//! The Python source talks to Telethon through the process `master`
//! router. This crate has no Telethon/grammers dependency, so routing
//! and sending are abstracted behind [`MasterRouter`] / [`TgClient`]:
//! the binary crate implements those against its real clients. All
//! decision logic — the read-only allowlist, route resolution, entity
//! parsing, parse-mode validation, the empty-text guard — lives here.

use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ultroid_core::error::{Error, Result};

use crate::telegram::{RichFormat, markup_to_plain, rich_output};

// Read-only raw MTProto functions the agent may invoke. Everything not
// listed here is refused; there is no wildcard. Write-type requests
// (send, edit, delete, join, ...) are deliberately absent.
pub const RAW_ALLOWED: &[&str] = &[
    // conversations / messages
    "messages.GetHistoryRequest",
    "messages.SearchRequest",
    "messages.GetMessagesRequest",
    "messages.GetPeerDialogs",
    "messages.GetFullChatRequest",
    "messages.GetWebPageRequest",
    // entities / profiles
    "channels.GetChannelsRequest",
    "channels.GetFullChannelRequest",
    "users.GetUsersRequest",
    "users.GetFullUserRequest",
    "contacts.ResolveUsernameRequest",
    // misc read-only metadata
    "help.GetConfig",
    "help.GetNearestDc",
    "messages.GetAllStickers",
];

const MESSAGE_MAX_TEXT: usize = 2000;
const READ_MAX_LIMIT: u32 = 100;

/// A normalized chat reference: a numeric id, or a username / invite token.
#[derive(Debug, Clone, PartialEq)]
pub enum EntityRef {
    Id(i64),
    Name(String),
}

impl EntityRef {
    pub fn as_str(&self) -> String {
        match self {
            EntityRef::Id(n) => n.to_string(),
            EntityRef::Name(s) => s.clone(),
        }
    }
}

fn tme_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:https?://)?t\.me/(?:joinchat/|\+)?([A-Za-z0-9_-]+)/?$")
            .expect("static regex")
    })
}

fn int_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^-?\d+$").expect("static regex"))
}

/// Normalize a chat reference into something Telegram can resolve.
///
/// Accepts numeric ids (int or digit string), `@usernames`, bare
/// usernames, and `t.me/<name>` links. Invite hashes come back as the
/// bare token (Telethon resolves them via `joinchat` semantics).
pub fn parse_entity_ref(r#ref: &str) -> Option<EntityRef> {
    let s = r#ref.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(caps) = tme_re().captures(s) {
        return Some(EntityRef::Name(caps[1].to_string()));
    }
    if int_re().is_match(s) {
        return s
            .parse::<i64>()
            .ok()
            .map(EntityRef::Id)
            .or_else(|| Some(EntityRef::Name(s.to_string())));
    }
    Some(EntityRef::Name(s.to_string()))
}

/// A Telegram call failure, classified the way `run_tg_call` does:
/// flood wait, RPC error, or anything else.
#[derive(Debug, Clone)]
pub enum TgError {
    FloodWait { seconds: Option<u64> },
    Rpc { name: String, message: String },
    Other(String),
}

impl TgError {
    pub fn to_dict(&self) -> Value {
        match self {
            TgError::FloodWait { seconds } => json!({
                "ok": false,
                "error": "FloodWait",
                "wait_seconds": seconds.map(Value::from).unwrap_or(Value::Null),
            }),
            TgError::Rpc { name, message } => json!({
                "ok": false,
                "error": format!("{name}: {message}"),
                "rpc": name,
            }),
            TgError::Other(message) => json!({
                "ok": false,
                "error": format!("Error: {message}"),
            }),
        }
    }
}

impl std::fmt::Display for TgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TgError::FloodWait { seconds } => write!(f, "FloodWait {seconds:?}s"),
            TgError::Rpc { name, message } => write!(f, "{name}: {message}"),
            TgError::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TgError {}

/// One sent message, as far as the agent can observe it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentMessage {
    pub message_id: Option<i64>,
    pub chat_id: Option<i64>,
}

/// One read message, serialized the way `_serialize_message` does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TgMessage {
    pub id: Option<i64>,
    pub sender_id: Option<i64>,
    /// ISO-8601 date string, when the client supplied one.
    pub date: Option<String>,
    pub text: String,
    #[serde(rename = "out")]
    pub is_out: bool,
    pub reply_to: Option<i64>,
    pub media: Option<String>,
}

/// One routed Telegram client. Implemented by the binary crate.
#[async_trait::async_trait]
pub trait TgClient: Send + Sync {
    async fn send_message(
        &self,
        entity: &EntityRef,
        text: &str,
        reply_to: Option<i64>,
        link_preview: bool,
    ) -> std::result::Result<SentMessage, TgError>;

    async fn send_file(
        &self,
        entity: &EntityRef,
        file: &[u8],
        filename: &str,
        caption: Option<&str>,
    ) -> std::result::Result<SentMessage, TgError>;

    async fn get_messages(
        &self,
        entity: &EntityRef,
        limit: u32,
    ) -> std::result::Result<Vec<TgMessage>, TgError>;

    /// Invoke an allowlisted raw request; returns the `repr` of the
    /// result (the raw result object itself is not JSON-serialisable).
    async fn invoke_raw(&self, name: &str, args: &Value) -> std::result::Result<String, TgError>;

    /// Whether this client can send native RichMessages.
    async fn can_send_rich(&self) -> std::result::Result<bool, TgError>;

    /// Send a native rich answer. The rich construction itself lives in
    /// the binary crate (it owns the RichMessage machinery).
    async fn send_rich_text(
        &self,
        entity: &EntityRef,
        text: &str,
        format: RichFormat,
        reply_to: Option<i64>,
        link_preview: bool,
    ) -> std::result::Result<SentMessage, TgError>;
}

/// Which client a route name selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Auto,
    User,
    Bot,
    Business,
}

impl Route {
    pub fn from_str(route: &str) -> std::result::Result<Route, String> {
        match route.to_lowercase().as_str() {
            "auto" | "" => Ok(Route::Auto),
            "user" => Ok(Route::User),
            "bot" => Ok(Route::Bot),
            "business" => Ok(Route::Business),
            other => Err(format!("Unknown route '{other}' (auto|user|bot|business)")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Route::Auto => "auto",
            Route::User => "user",
            Route::Bot => "bot",
            Route::Business => "business",
        }
    }
}

/// The master router. Implemented by the binary crate against its real
/// clients (`master.route()`, `master.user`, `master.bot`,
/// `master.business`, `master.available()` in Python).
pub trait MasterRouter: Send + Sync {
    /// The client automatic routing selects.
    fn auto_client(&self) -> Option<Arc<dyn TgClient>>;
    /// The client for an explicit route, if connected.
    fn client_for(&self, route: Route) -> Option<Arc<dyn TgClient>>;
    /// Names of the connected clients, for error messages.
    fn available(&self) -> Vec<String>;
}

/// Await a Telegram call and normalize its failure modes.
///
/// Returns `{"ok": true, "result": ...}` or a structured error the
/// agent can reason about (flood wait, RPC error, transport failure).
pub async fn run_tg_call<T: Serialize>(
    fut: impl std::future::Future<Output = std::result::Result<T, TgError>>,
) -> Value {
    match fut.await {
        Ok(result) => json!({"ok": true, "result": result}),
        Err(e) => e.to_dict(),
    }
}

/// Resolve an explicit route name to a real client.
pub fn pick_route_client(master: &Arc<dyn MasterRouter>, route: &str) -> Result<Arc<dyn TgClient>> {
    let parsed = Route::from_str(route).map_err(Error::Other)?;
    match parsed {
        Route::Auto => master.auto_client().ok_or_else(|| {
            Error::Other(format!(
                "Route 'auto' has no connected client; available: {}",
                master.available().join(", ")
            ))
        }),
        Route::User | Route::Bot | Route::Business => {
            let client = master.client_for(parsed);
            match client {
                Some(c) => Ok(c),
                None => Err(Error::Other(format!(
                    "Route '{}' has no connected client; available: {}",
                    parsed.as_str(),
                    if master.available().is_empty() {
                        "none".to_string()
                    } else {
                        format!("{:?}", master.available())
                    }
                ))),
            }
        }
    }
}

/// Options for [`send_via_master`], mirroring its Python kwargs.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    pub route: String,
    pub reply_to: Option<i64>,
    pub parse_mode: Option<String>,
    pub link_preview: bool,
    pub rich: bool,
}

impl SendOptions {
    pub fn new() -> Self {
        SendOptions {
            route: "auto".to_string(),
            reply_to: None,
            parse_mode: None,
            link_preview: true,
            rich: false,
        }
    }
}

fn sent_to_dict(route: &str, sent: &SentMessage) -> Value {
    json!({
        "ok": true,
        "route": route,
        "message_id": sent.message_id,
        "chat_id": sent.chat_id,
    })
}

fn normalize_parse_mode(
    parse_mode: Option<&str>,
) -> std::result::Result<Option<RichFormat>, Value> {
    match parse_mode {
        None => Ok(None),
        Some(mode) => {
            let mode = mode.trim().to_lowercase();
            match mode.as_str() {
                "md" | "markdown" | "markdownv3" => Ok(Some(RichFormat::Markdown)),
                "html" => Ok(Some(RichFormat::Html)),
                _ => Err(json!({
                    "ok": false,
                    "error": "parse_mode must be HTML, Markdown, or omitted for automatic detection.",
                })),
            }
        }
    }
}

/// Send plain text by default, or native RichMessage when explicitly asked.
pub async fn send_via_master(
    master: &Arc<dyn MasterRouter>,
    text: &str,
    entity_ref: &str,
    opts: &SendOptions,
) -> Value {
    let client = match pick_route_client(master, &opts.route) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    let Some(entity) = parse_entity_ref(entity_ref) else {
        return json!({"ok": false, "error": "empty entity reference"});
    };

    if opts.rich {
        match client.can_send_rich().await {
            Ok(true) => {}
            Ok(false) => {
                return json!({
                    "ok": false,
                    "error": concat!(
                        "Native RichMessages require a bot account or a confirmed ",
                        "Telegram Premium user account. Send with rich=false for ",
                        "ordinary plain text. No RichMessage request was sent."
                    ),
                });
            }
            Err(e) => return e.to_dict(),
        }
        let format = match normalize_parse_mode(opts.parse_mode.as_deref()) {
            Ok(f) => f.unwrap_or_else(|| rich_output(text).format),
            Err(err) => return err,
        };
        return match client
            .send_rich_text(&entity, text, format, opts.reply_to, opts.link_preview)
            .await
        {
            Ok(sent) => sent_to_dict(&opts.route, &sent),
            Err(e) => e.to_dict(),
        };
    }

    let format = match normalize_parse_mode(opts.parse_mode.as_deref()) {
        Ok(f) => f,
        Err(err) => return err,
    };
    let markup = match format {
        Some(f) => crate::telegram::RichMarkupData {
            text: text.to_string(),
            format: f,
        },
        None => rich_output(text),
    };
    let plain_text = markup_to_plain(&markup);
    if plain_text.trim().is_empty() {
        return json!({"ok": false, "error": "Plain-text message cannot be empty."});
    }
    match client
        .send_message(&entity, &plain_text, opts.reply_to, opts.link_preview)
        .await
    {
        Ok(sent) => sent_to_dict(&opts.route, &sent),
        Err(e) => e.to_dict(),
    }
}

/// Send an in-memory attachment through the master router as plain text.
pub async fn send_file_via_master(
    master: &Arc<dyn MasterRouter>,
    file: &[u8],
    filename: &str,
    entity_ref: &str,
    route: &str,
    caption: Option<&str>,
) -> Value {
    let client = match pick_route_client(master, route) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    let Some(entity) = parse_entity_ref(entity_ref) else {
        return json!({"ok": false, "error": "empty entity reference"});
    };
    match client.send_file(&entity, file, filename, caption).await {
        Ok(sent) => json!({
            "ok": true,
            "route": route,
            "message_id": sent.message_id,
        }),
        Err(e) => e.to_dict(),
    }
}

/// Read the most recent messages of a chat via the routed client.
pub async fn read_recent(
    master: &Arc<dyn MasterRouter>,
    entity_ref: &str,
    limit: u32,
    route: &str,
) -> Value {
    let client = match pick_route_client(master, route) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": e.to_string()}),
    };
    let limit = limit.clamp(1, READ_MAX_LIMIT);
    let Some(entity) = parse_entity_ref(entity_ref) else {
        return json!({"ok": false, "error": "empty entity reference"});
    };
    match client.get_messages(&entity, limit).await {
        Ok(mut msgs) => {
            for m in &mut msgs {
                if m.text.chars().count() > MESSAGE_MAX_TEXT {
                    m.text = m.text.chars().take(MESSAGE_MAX_TEXT).collect();
                }
            }
            let count = msgs.len();
            json!({
                "ok": true,
                "count": count,
                "messages": msgs,
            })
        }
        Err(e) => e.to_dict(),
    }
}

/// Resolve an allowlisted `namespace.Class` raw request name.
///
/// Returns the name itself when it is on the read-only allowlist, or
/// `None` when it is not on the allowlist. The Telethon request class
/// it names is resolved by the binary crate's `TgClient::invoke_raw`;
/// there is no Telethon inside this crate to resolve it against.
pub fn resolve_raw_function(name: &str) -> Option<&'static str> {
    if !RAW_ALLOWED.contains(&name) {
        return None;
    }
    RAW_ALLOWED.iter().copied().find(|allowed| *allowed == name)
}

/// Build and invoke an allowlisted raw request, flood-aware.
pub async fn invoke_raw(client: &Arc<dyn TgClient>, name: &str, args: Option<&Value>) -> Value {
    if resolve_raw_function(name).is_none() {
        return json!({
            "ok": false,
            "error": format!(
                "Raw function '{name}' is not on the read-only allowlist. Allowed: {}",
                {
                    let mut allowed: Vec<&str> = RAW_ALLOWED.to_vec();
                    allowed.sort_unstable();
                    allowed.join(", ")
                }
            ),
            "denied": true,
        });
    }
    let args = args.cloned().unwrap_or(Value::Null);
    match client.invoke_raw(name, &args).await {
        Ok(result_repr) => {
            // The raw result object itself is not JSON-serialisable; the
            // repr is what the model reads. Structured serializers can be
            // added per-request-type later if needed.
            let repr: String = result_repr.chars().take(4000).collect();
            json!({"ok": true, "result_repr": repr})
        }
        Err(e) => e.to_dict(),
    }
}
