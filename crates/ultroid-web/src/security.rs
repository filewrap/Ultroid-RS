//! Security primitives for the setup server.
//!
//! Port of `pyUltroid/webserver/security.py` — three unrelated concerns,
//! three unrelated types, sharing no base and no vocabulary on purpose:
//!
//! * [`SecretRedactor`] scrubs known secrets out of arbitrary text.
//! * [`RateLimiter`] is a sliding-window counter keyed by caller.
//! * [`TokenAuthority`] is the only minting/verifying authority; it owns the
//!   process-wide [`REDACTOR`] so a caller cannot wire the wrong one in.
//!
//! Differences from Python (same behaviour, Rust-shaped):
//! * Locks are `std::sync::Mutex` (the Python `threading.Lock` equivalent);
//!   the types are `Send + Sync` and cheaply cloneable through `Arc`.
//! * Constant-time comparison is a hand-rolled XOR fold — the workspace has
//!   no `subtle` dependency and the comparison is short and secret-sized.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;

pub const TOKEN_HEADER: &str = "X-Setup-Token";
pub const TOKEN_QUERY_PARAM: &str = "token";
pub const REDACTED: &str = "**********";

/// `secrets.token_urlsafe(24)`: 24 random bytes, base64url, no padding.
pub const TOKEN_BYTES: usize = 24;

/// Longest-first substring scrubber for a set of known secret values.
///
/// Values shorter than [`SecretRedactor::MIN_LENGTH`] are refused: scrubbing
/// a 3-char string would mangle unrelated text far more often than it would
/// help.
#[derive(Debug, Default)]
pub struct SecretRedactor {
    secrets: Mutex<HashSet<String>>,
}

impl SecretRedactor {
    pub const MIN_LENGTH: usize = 4;

    pub fn new() -> Self {
        Self::default()
    }

    /// Register `value` for scrubbing. Returns whether it is now tracked.
    pub fn add(&self, value: impl ToString) -> bool {
        let text = value.to_string();
        if text.len() < Self::MIN_LENGTH {
            return false;
        }
        let mut guard = self.secrets.lock().expect("redactor lock poisoned");
        guard.insert(text)
    }

    pub fn discard(&self, value: impl ToString) {
        let mut guard = self.secrets.lock().expect("redactor lock poisoned");
        guard.remove(&value.to_string());
    }

    pub fn clear(&self) {
        let mut guard = self.secrets.lock().expect("redactor lock poisoned");
        guard.clear();
    }

    pub fn len(&self) -> usize {
        self.secrets.lock().expect("redactor lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Replace every tracked secret in `text` with [`REDACTED`],
    /// longest secrets first so a short secret cannot split a longer one.
    pub fn redact(&self, text: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }
        let mut ordered: Vec<String> = {
            let guard = self.secrets.lock().expect("redactor lock poisoned");
            guard.iter().cloned().collect()
        };
        ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut out = text.to_string();
        for secret in ordered {
            if out.contains(&secret) {
                out = out.replace(&secret, REDACTED);
            }
        }
        out
    }
}

/// Sliding-window attempt counter.
///
/// Stateful per key and prunes idle keys so a long-lived server does not
/// accumulate a map entry per IP forever.
#[derive(Debug)]
pub struct RateLimiter {
    max_attempts: usize,
    window: Duration,
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_attempts: usize, window_seconds: f64) -> Self {
        Self {
            max_attempts,
            window: Duration::from_secs_f64(window_seconds),
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Record one attempt for `key`; `false` when the window is exhausted.
    /// A refused attempt is *not* recorded.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut guard = self.hits.lock().expect("limiter lock poisoned");
        Self::expire_locked(&mut guard, self.window, now);
        let hits = guard.entry(key.to_string()).or_default();
        if hits.len() >= self.max_attempts {
            return false;
        }
        hits.push(now);
        true
    }

    /// Attempts left for `key` inside the current window.
    pub fn remaining(&self, key: &str) -> usize {
        let now = Instant::now();
        let mut guard = self.hits.lock().expect("limiter lock poisoned");
        Self::expire_locked(&mut guard, self.window, now);
        let hits = guard.get(key).map(|h| h.len()).unwrap_or(0);
        self.max_attempts.saturating_sub(hits)
    }

    pub fn reset(&self) {
        let mut guard = self.hits.lock().expect("limiter lock poisoned");
        guard.clear();
    }

    fn expire_locked(hits: &mut HashMap<String, Vec<Instant>>, window: Duration, now: Instant) {
        hits.retain(|_, ts| {
            ts.retain(|t| now.duration_since(*t) < window);
            !ts.is_empty()
        });
    }
}

/// Mints, reads back and constant-time-verifies the setup token.
///
/// This is the single owner of the shared [`REDACTOR`]: anything that needs
/// to scrub must ask the authority it already has, so the setup app and the
/// Mini App can never end up pointing at two different redactors.
#[derive(Debug)]
pub struct TokenAuthority {
    redactor: Arc<SecretRedactor>,
}

impl TokenAuthority {
    pub fn new(redactor: Option<Arc<SecretRedactor>>) -> Self {
        Self {
            redactor: redactor.unwrap_or_else(|| Arc::new(SecretRedactor::new())),
        }
    }

    pub fn redactor(&self) -> &Arc<SecretRedactor> {
        &self.redactor
    }

    /// Return a fresh URL-safe token, registered for redaction unless
    /// `track` is false (the Mini App mints untracked tokens: they live in
    /// the database, and the setup redactor must not grow unboundedly).
    pub fn mint(&self, track: bool) -> String {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        if track {
            self.redactor.add(&token);
        }
        token
    }

    /// Header first, then query string (the UI links carry `?token=`).
    pub fn pull(&self, headers: &http::HeaderMap, query: &str) -> Option<String> {
        if let Some(value) = headers.get(TOKEN_HEADER) {
            if let Ok(text) = value.to_str() {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == TOKEN_QUERY_PARAM)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    }

    /// Constant-time comparison that also rejects empty inputs.
    pub fn verify(&self, provided: Option<&str>, expected: &str) -> bool {
        let provided = match provided {
            Some(p) if !p.is_empty() && !expected.is_empty() => p,
            _ => return false,
        };
        constant_time_eq(provided.as_bytes(), expected.as_bytes())
    }

    /// Convenience: pull then verify in one call.
    pub fn permits(&self, headers: &http::HeaderMap, query: &str, expected: &str) -> bool {
        self.verify(self.pull(headers, query).as_deref(), expected)
    }
}

impl Default for TokenAuthority {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Byte-wise equality in time independent of the first differing byte.
/// Lengths are folded into the accumulator so unequal lengths also fail
/// without an early return.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// The process-wide authority, mirroring Python's module-level `AUTHORITY`.
pub static AUTHORITY: OnceLock<Arc<TokenAuthority>> = OnceLock::new();

pub fn authority() -> Arc<TokenAuthority> {
    AUTHORITY
        .get_or_init(|| Arc::new(TokenAuthority::new(None)))
        .clone()
}

/// The process-wide redactor, mirroring Python's module-level `REDACTOR`.
pub fn redactor() -> Arc<SecretRedactor> {
    authority().redactor().clone()
}

/// Mint a fresh setup token on the process-wide authority.
pub fn generate_setup_token() -> String {
    authority().mint(true)
}

/// Pull a token from headers/query on the process-wide authority.
pub fn extract_token(headers: &http::HeaderMap, query: &str) -> Option<String> {
    authority().pull(headers, query)
}

/// Verify a token on the process-wide authority.
pub fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    authority().verify(provided, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redactor_longest_first_and_min_length() {
        let r = SecretRedactor::new();
        assert!(!r.add("abc"));
        assert!(r.add("secret-token-value"));
        assert!(r.add("secret"));
        let redacted = r.redact("leak secret-token-value and secret here");
        assert_eq!(redacted, "leak ********** and ********** here");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn redactor_empty_passthrough() {
        let r = SecretRedactor::new();
        assert_eq!(r.redact(""), "");
    }

    #[test]
    fn limiter_window_and_remaining() {
        let l = RateLimiter::new(2, 60.0);
        assert!(l.allow("1.2.3.4"));
        assert_eq!(l.remaining("1.2.3.4"), 1);
        assert!(l.allow("1.2.3.4"));
        assert!(!l.allow("1.2.3.4"));
        assert_eq!(l.remaining("1.2.3.4"), 0);
        assert!(l.allow("5.6.7.8"));
        l.reset();
        assert!(l.allow("1.2.3.4"));
    }

    #[test]
    fn token_verify_rejects_empty_and_mismatch() {
        let a = TokenAuthority::new(None);
        let token = a.mint(true);
        assert!(a.verify(Some(&token), &token));
        assert!(!a.verify(Some("wrong"), &token));
        assert!(!a.verify(None, &token));
        assert!(!a.verify(Some(""), &token));
        assert!(!a.verify(Some(&token), ""));
    }

    #[test]
    fn token_mint_is_url_safe() {
        let a = TokenAuthority::new(None);
        let token = a.mint(true);
        assert_eq!(token.len(), 32);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn pull_prefers_header_over_query() {
        let a = TokenAuthority::new(None);
        let mut headers = http::HeaderMap::new();
        headers.insert(TOKEN_HEADER, "from-header".parse().unwrap());
        assert_eq!(
            a.pull(&headers, "token=from-query"),
            Some("from-header".into())
        );
        let empty = http::HeaderMap::new();
        assert_eq!(
            a.pull(&empty, "token=from-query"),
            Some("from-query".into())
        );
        assert_eq!(a.pull(&empty, "other=1"), None);
    }
}
