//! Structural, type-stable keys for agent conversation scopes — port of
//! `pyUltroid/agent/_chatkey.py`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

/// Separator between the ``dm`` prefix and its two numeric components.
pub const DM_SESSION_DELIMITER: &str = ":";

/// Category identifiers carried on every [`ChatKey`].
pub const CATEGORY_GROUP: &str = "group";
pub const CATEGORY_USER: &str = "user";
pub const CATEGORY_SESSION: &str = "dm-session";

// ── failure types ─────────────────────────────────────────────────────────

/// A string key matched its category's prefix but not its full shape.
///
/// Raised at the boundary so a malformed value is rejected instead of
/// being silently accepted and misrouted. Distinct from a type error:
/// this is a *content* problem, not a *kind* one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKeyFormat(pub String);

impl fmt::Display for InvalidKeyFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidKeyFormat {}

impl From<InvalidKeyFormat> for ultroid_core::error::Error {
    fn from(e: InvalidKeyFormat) -> Self {
        ultroid_core::error::Error::Other(e.0)
    }
}

// ── concrete key types ────────────────────────────────────────────────────

/// A plain integer Telegram chat/sender id — the common case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NumericChatKey {
    original: i64,
    canonical: String,
}

impl NumericChatKey {
    pub fn new(original: i64) -> Self {
        NumericChatKey {
            original,
            canonical: original.to_string(),
        }
    }

    /// The numeric id, for callers that genuinely need `int` semantics.
    pub fn user_id(&self) -> i64 {
        self.original
    }

    pub fn original(&self) -> i64 {
        self.original
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }
}

/// A ``dm:<user_id>:<anchor>`` session key — one per reply thread.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DmSessionKey {
    user_id: i64,
    anchor: i64,
    canonical: String,
}

impl DmSessionKey {
    pub fn new(user_id: i64, anchor: i64) -> Self {
        let canonical = format!("dm{DM_SESSION_DELIMITER}{user_id}{DM_SESSION_DELIMITER}{anchor}");
        DmSessionKey {
            user_id,
            anchor,
            canonical,
        }
    }

    pub fn user_id(&self) -> i64 {
        self.user_id
    }

    pub fn anchor(&self) -> i64 {
        self.anchor
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }
}

/// A string key nobody else claimed — carried verbatim, never coerced.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OpaqueStringKey {
    original: String,
}

impl OpaqueStringKey {
    pub fn new(original: impl Into<String>) -> Self {
        OpaqueStringKey {
            original: original.into(),
        }
    }

    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn canonical(&self) -> &str {
        &self.original
    }
}

/// Immutable, category-aware conversation-scope identifier.
///
/// Every ``chat_id``-consuming site takes a [`ChatKey`] rather than a
/// bare `i64` or `String`. The key discovers its own category at
/// construction and never changes it, so downstream code branches on
/// `category` without re-parsing the value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatKey {
    Numeric(NumericChatKey),
    DmSession(DmSessionKey),
    Opaque(OpaqueStringKey),
}

impl ChatKey {
    /// The unconverted value the key was built from, as text.
    pub fn original(&self) -> String {
        match self {
            ChatKey::Numeric(k) => k.original.to_string(),
            ChatKey::DmSession(k) => k.canonical.clone(),
            ChatKey::Opaque(k) => k.original.clone(),
        }
    }

    /// Canonical string form — the safe dict/storage key.
    pub fn value(&self) -> String {
        match self {
            ChatKey::Numeric(k) => k.canonical.clone(),
            ChatKey::DmSession(k) => k.canonical.clone(),
            ChatKey::Opaque(k) => k.original.clone(),
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            ChatKey::Numeric(_) => CATEGORY_GROUP,
            ChatKey::DmSession(_) => CATEGORY_SESSION,
            ChatKey::Opaque(_) => CATEGORY_USER,
        }
    }

    pub fn is_session(&self) -> bool {
        self.category() == CATEGORY_SESSION
    }

    pub fn is_group(&self) -> bool {
        self.category() == CATEGORY_GROUP
    }

    pub fn is_user(&self) -> bool {
        self.category() == CATEGORY_USER
    }

    /// The form to use as an in-memory `dict` key.
    pub fn to_dict_key(&self) -> String {
        self.value()
    }

    /// The numeric id when this is a [`NumericChatKey`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ChatKey::Numeric(k) => Some(k.user_id()),
            _ => None,
        }
    }
}

impl fmt::Display for ChatKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value())
    }
}

// ── formatters ────────────────────────────────────────────────────────────

fn looks_like_int(text: &str) -> bool {
    // True when *text* parses as an integer id.
    //
    // Guarded so a stray sign or whitespace never yields a false
    // positive; a chat id is a bare run of digits in the Telegram schema.
    if text.is_empty() {
        return false;
    }
    text.trim_start_matches(['+', '-'])
        .chars()
        .all(|c| c.is_ascii_digit())
        && !text.trim_start_matches(['+', '-']).is_empty()
}

fn chat_id_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^dm:(\d+):(\d+)$").expect("static regex"))
}

/// Base strategy owning one key category's round trip.
///
/// Subclasses bind both the category string and the [`ChatKey`] variant
/// they render. That binding is what lets [`ChatKeyRegistry`] dispatch
/// [`ChatKeyRegistry::canonical`] by *variant* rather than by a string
/// lookup a typo could silently misroute.
pub trait ChatKeyFormatter: Send + Sync {
    fn category(&self) -> &'static str;
    /// Return the stable string form used for dict keys and storage.
    fn canonical(&self, key: &ChatKey) -> String;
    /// Build a key from *text*, or return `None` if it is not ours.
    ///
    /// Returning `None` (rather than raising) for a foreign string lets
    /// the registry probe formatters in order without try/except at each
    /// hop. A string that matches this formatter's prefix but not its
    /// full shape is [`InvalidKeyFormat`], because accepting it would
    /// misroute updates.
    fn parse(&self, text: &str) -> Result<Option<ChatKey>, InvalidKeyFormat>;
}

/// Formatter for plain integer Telegram chat ids.
#[derive(Debug, Clone, Copy, Default)]
pub struct _NumericFormatter;

impl ChatKeyFormatter for _NumericFormatter {
    fn category(&self) -> &'static str {
        CATEGORY_GROUP
    }

    fn canonical(&self, key: &ChatKey) -> String {
        match key {
            ChatKey::Numeric(k) => k.user_id().to_string(),
            other => other.value(),
        }
    }

    fn parse(&self, text: &str) -> Result<Option<ChatKey>, InvalidKeyFormat> {
        if looks_like_int(text) {
            return match text.parse::<i64>() {
                Ok(n) => Ok(Some(ChatKey::Numeric(NumericChatKey::new(n)))),
                Err(_) => Ok(None),
            };
        }
        Ok(None)
    }
}

/// Formatter for ``dm:<user_id>:<anchor>`` session keys.
#[derive(Debug, Clone, Copy, Default)]
pub struct _SessionFormatter;

impl ChatKeyFormatter for _SessionFormatter {
    fn category(&self) -> &'static str {
        CATEGORY_SESSION
    }

    fn canonical(&self, key: &ChatKey) -> String {
        match key {
            ChatKey::DmSession(k) => format!(
                "dm{DM_SESSION_DELIMITER}{}{DM_SESSION_DELIMITER}{}",
                k.user_id(),
                k.anchor()
            ),
            other => other.value(),
        }
    }

    fn parse(&self, text: &str) -> Result<Option<ChatKey>, InvalidKeyFormat> {
        if !text.starts_with("dm:") {
            return Ok(None);
        }
        let Some(caps) = chat_id_pattern().captures(text) else {
            return Err(InvalidKeyFormat(format!(
                "DM session key {text:?} has non-numeric user_id or anchor"
            )));
        };
        let user_id: i64 = caps[1].parse().map_err(|_| {
            InvalidKeyFormat(format!(
                "DM session key {text:?} has non-numeric user_id or anchor"
            ))
        })?;
        let anchor: i64 = caps[2].parse().map_err(|_| {
            InvalidKeyFormat(format!(
                "DM session key {text:?} has non-numeric user_id or anchor"
            ))
        })?;
        Ok(Some(ChatKey::DmSession(DmSessionKey::new(user_id, anchor))))
    }
}

/// Fallback for structurally valid strings nobody else claimed.
#[derive(Debug, Clone, Copy, Default)]
pub struct _OpaqueFormatter;

impl ChatKeyFormatter for _OpaqueFormatter {
    fn category(&self) -> &'static str {
        CATEGORY_USER
    }

    fn canonical(&self, key: &ChatKey) -> String {
        key.value()
    }

    fn parse(&self, text: &str) -> Result<Option<ChatKey>, InvalidKeyFormat> {
        // A fallback claims everything that is left. Returning `None` for
        // an empty string prevents an empty-chat-key from shadowing a real
        // private-chat session.
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(ChatKey::Opaque(OpaqueStringKey::new(text))))
    }
}

/// A catalog mirrors the registry but keyed by category, for callers
/// that want to ask "what shapes are supported" without constructing a
/// key.
pub fn chat_key_catalog() -> ChatKeyRegistry {
    default_chatkey_registry()
}

// ── registry ──────────────────────────────────────────────────────────────

/// Ordered collection of [`ChatKeyFormatter`] strategies.
///
/// `probe` returns the first formatter whose parser claims a string. The
/// dispatch for [`ChatKeyRegistry::canonical`] is by *variant*: each
/// formatter is bound to its [`ChatKey`] variant at construction time.
pub struct ChatKeyRegistry {
    ordered: Vec<Arc<dyn ChatKeyFormatter>>,
    by_category: HashMap<&'static str, Arc<dyn ChatKeyFormatter>>,
}

impl ChatKeyRegistry {
    pub fn new(formatters: Vec<Arc<dyn ChatKeyFormatter>>) -> Self {
        let by_category = formatters
            .iter()
            .map(|f| (f.category(), Arc::clone(f)))
            .collect();
        ChatKeyRegistry {
            ordered: formatters,
            by_category,
        }
    }

    pub fn ordered(&self) -> &[Arc<dyn ChatKeyFormatter>] {
        &self.ordered
    }

    /// Return the first formatter that claims *text*.
    ///
    /// Raises [`InvalidKeyFormat`] when a formatter recognised *text*'s
    /// prefix but its full shape did not match — the key is malformed,
    /// not unknown.
    pub fn probe(&self, text: &str) -> Result<Option<Arc<dyn ChatKeyFormatter>>, InvalidKeyFormat> {
        for fmt in &self.ordered {
            // Surface immediately so ``dm:garbage:1`` is not silently
            // swallowed by the catch-all formatter and misrouted.
            if let Some(_claim) = fmt.parse(text)? {
                return Ok(Some(Arc::clone(fmt)));
            }
        }
        Ok(None)
    }

    /// Look up a formatter by its declared category string.
    pub fn for_category(
        &self,
        category: &str,
    ) -> Result<Arc<dyn ChatKeyFormatter>, InvalidKeyFormat> {
        self.by_category.get(category).cloned().ok_or_else(|| {
            InvalidKeyFormat(format!("no formatter registered for category {category:?}"))
        })
    }

    /// Return the formatter bound to *key*'s concrete variant.
    pub fn formatter_for(
        &self,
        key: &ChatKey,
    ) -> Result<Arc<dyn ChatKeyFormatter>, InvalidKeyFormat> {
        self.for_category(key.category())
    }

    /// Render *key* via the formatter bound to its concrete variant.
    pub fn canonical(&self, key: &ChatKey) -> Result<String, InvalidKeyFormat> {
        Ok(self.formatter_for(key)?.canonical(key))
    }
}

/// Default chain: numeric ids, then DM sessions, then catch-all string.
pub fn default_chatkey_registry() -> ChatKeyRegistry {
    ChatKeyRegistry::new(vec![
        Arc::new(_NumericFormatter),
        Arc::new(_SessionFormatter),
        Arc::new(_OpaqueFormatter),
    ])
}

// ── normalizer ────────────────────────────────────────────────────────────

/// The raw shapes a `chat_id` can arrive in.
#[derive(Debug, Clone)]
pub enum ChatKeyValue {
    /// `None`: a caller bug, not a new key shape.
    Null,
    /// A `bool` is rejected explicitly: `bool` is a subclass of `int`
    /// in Python and `int(True) == 1` is a real Telegram id; a boolean
    /// arriving here is a flag in the wrong argument position.
    Bool(bool),
    Int(i64),
    Str(String),
    /// An existing key is returned untouched (idempotent).
    Key(ChatKey),
}

/// Turns a raw `chat_id` into a [`ChatKey`].
///
/// Holds a [`ChatKeyRegistry`] so the strategy chain is swappable —
/// tests can construct a normalizer over a restricted registry to assert
/// that rejection paths fire.
pub struct ChatKeyNormalizer {
    registry: ChatKeyRegistry,
}

impl Default for ChatKeyNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatKeyNormalizer {
    pub fn new() -> Self {
        ChatKeyNormalizer {
            registry: default_chatkey_registry(),
        }
    }

    pub fn with_registry(registry: ChatKeyRegistry) -> Self {
        ChatKeyNormalizer { registry }
    }

    pub fn registry(&self) -> &ChatKeyRegistry {
        &self.registry
    }

    /// Build a [`ChatKey`] from anything a plugin or DM flow passes.
    ///
    /// Dispatch order:
    ///
    /// 1. `Null` → [`InvalidKeyFormat`] (a None id is a caller bug).
    /// 2. An existing [`ChatKey`] is returned untouched (idempotent).
    /// 3. `Bool` is rejected loudly.
    /// 4. `Int` → [`NumericChatKey`].
    /// 5. `Str` → the registry probes formatters in order.
    pub fn normalize(&self, chat_id: ChatKeyValue) -> Result<ChatKey, InvalidKeyFormat> {
        match chat_id {
            ChatKeyValue::Null => Err(InvalidKeyFormat("chat_id is None".to_string())),
            ChatKeyValue::Key(key) => Ok(key),
            ChatKeyValue::Bool(b) => Err(InvalidKeyFormat(format!(
                "chat_id {b:?} is a bool, not an id"
            ))),
            ChatKeyValue::Int(n) => Ok(ChatKey::Numeric(NumericChatKey::new(n))),
            ChatKeyValue::Str(text) => self.from_string(&text),
        }
    }

    /// Resolve a string via the registry, failing closed on ambiguity.
    fn from_string(&self, text: &str) -> Result<ChatKey, InvalidKeyFormat> {
        let fmt = self.registry.probe(text)?;
        match fmt {
            None => Ok(ChatKey::Opaque(OpaqueStringKey::new(text))),
            Some(f) => match f.parse(text)? {
                Some(key) => Ok(key),
                None => Ok(ChatKey::Opaque(OpaqueStringKey::new(text))),
            },
        }
    }
}

fn default_chatkey_normalizer() -> &'static ChatKeyNormalizer {
    static NORMALIZER: OnceLock<ChatKeyNormalizer> = OnceLock::new();
    NORMALIZER.get_or_init(ChatKeyNormalizer::new)
}

/// Construct a [`ChatKey`], sharing the default normalizer.
pub fn build_chat_key(chat_id: ChatKeyValue) -> Result<ChatKey, InvalidKeyFormat> {
    default_chatkey_normalizer().normalize(chat_id)
}

/// Return the canonical string form of *chat_id*.
///
/// Convenience for call sites that already store strings and want the
/// normalized form without holding a [`ChatKey`] object.
pub fn normalize_chat_id(chat_id: ChatKeyValue) -> Result<String, InvalidKeyFormat> {
    Ok(build_chat_key(chat_id)?.value())
}

// ── enum façade over the string categories ─────────────────────────────────

/// Stable enum over the string categories.
///
/// Exposed for callers that prefer enums to strings (e.g. typed config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKeyCategory {
    Group,
    User,
    Session,
}

impl ChatKeyCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatKeyCategory::Group => CATEGORY_GROUP,
            ChatKeyCategory::User => CATEGORY_USER,
            ChatKeyCategory::Session => CATEGORY_SESSION,
        }
    }

    pub fn of(value: &str) -> Result<Self, InvalidKeyFormat> {
        match value {
            CATEGORY_GROUP => Ok(ChatKeyCategory::Group),
            CATEGORY_USER => Ok(ChatKeyCategory::User),
            CATEGORY_SESSION => Ok(ChatKeyCategory::Session),
            other => Err(InvalidKeyFormat(format!(
                "unknown chat-key category {other:?}"
            ))),
        }
    }
}

impl fmt::Display for ChatKeyCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
