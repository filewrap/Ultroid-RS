//! Structured callback-data model ("cbs") for the assistant crate.
//!
//! Port of `assistant/_cbs.py` (which re-exports
//! `pyUltroid/_core/_cbs.py`) plus the small shared helpers the
//! assistant facade (`assistant/__init__.py`) provides: `fmtmsg`
//! (port of `custom/fmt.py::fmtmsg`), DB key helpers, and time/RNG
//! helpers. This is the dependency-free leaf: every other module in
//! this crate may import from here, nothing here imports a sibling.
//!
//! Callback-data shapes (unchanged from Python):
//! 1. `help:<plugin>:<data>` — group-manager help menu.
//! 2. `assistant:<name>:<tail>` — everything else in assistant/.
//! 3. Standalones use the same `assistant:<name>:<tail>` shape.
//!
//! Contract note: `Ctx`/`Responder` carry no callback-query channel
//! (no answer-alert, no inline-query builder) and no button channel.
//! Callback handlers in this crate therefore take the raw callback
//! data as a `&str` argument (`handle_callback(ctx, data)`), and
//! keyboards are built as `Vec<Vec<InlineButton>>` data the binary
//! crate attaches. Where Python answers a callback query with an
//! alert, the handler replies with that text instead; a bare
//! acknowledgement is a no-op (nothing to answer through `Ctx`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use ultroid_core::error::{Error, Result};
use ultroid_core::{CommandDef, CommandHandler, Ctx, Registry};

/// Callback-data separator (Python `SEP`).
pub const SEP: &str = ":";

fn validate_part(part: &str) -> Result<String> {
    if part.is_empty() {
        return Err(Error::Other(
            "cb() segment must be non-empty; refusing to build a callback token with a missing segment"
                .to_string(),
        ));
    }
    if part.contains(SEP) {
        return Err(Error::Other(format!(
            "cb() segment {part:?} contains the separator ':'; callback data is colon-joined"
        )));
    }
    Ok(part.to_string())
}

/// Build callback data: `cb(&["assistant", "lang", "menu"])` ->
/// `"assistant:lang:menu"` (Python `cb(...)` returns bytes; Rust
/// keeps the UTF-8 string, `cb_bytes` gives the wire form).
pub fn cb(parts: &[&str]) -> Result<String> {
    if parts.is_empty() {
        return Err(Error::Other(
            "cb() called with no segments; refusing to build an empty token".to_string(),
        ));
    }
    let cleaned: Result<Vec<String>> = parts.iter().map(|p| validate_part(p)).collect();
    Ok(cleaned?.join(SEP))
}

/// `cb` as bytes, the exact Telegram callback-data payload.
pub fn cb_bytes(parts: &[&str]) -> Result<Vec<u8>> {
    Ok(cb(parts)?.into_bytes())
}

/// String form of `cb` for inline-query text / reuse as a token.
pub fn cb_str(parts: &[&str]) -> Result<String> {
    cb(parts)
}

/// `cb_pattern` semantics for dispatch: data must start with
/// `prefix + ":"`; returns the captured tail (everything after the
/// prefix separator), mirroring `re.compile(prefix + b":(.*)")`.
pub fn cb_tail(data: &str, prefix_parts: &[&str]) -> Result<Option<String>> {
    let prefix = cb(prefix_parts)?;
    let needle = format!("{prefix}{SEP}");
    if let Some(tail) = data.strip_prefix(&needle) {
        Ok(Some(tail.to_string()))
    } else {
        Ok(None)
    }
}

/// `cb_exact_pattern` semantics: data must equal the joined token.
pub fn cb_exact(parts: &[&str]) -> Result<String> {
    cb(parts)
}

fn clean_char(c: char) -> Option<char> {
    // Glyphs Python `_clean_nav_label` replaces with a space.
    if matches!(
        c as u32,
        0x00AB
            | 0x00BB
            | 0x2039
            | 0x203A
            | 0x25C0
            | 0x25B6
            | 0x2190
            | 0x2192
            | 0x2022
            | 0x00B7
            | 0xFE0E
            | 0xFE0F
            | 0x200D
            | 0x20E3
    ) {
        return Some(' ');
    }
    // Python drops characters whose Unicode category is So/Sk and
    // expands small-capital Latin letters; Rust has no unicodedata
    // table, so approximate: drop common symbol/dingbat ranges.
    let cp = c as u32;
    if (0x1F000..=0x1FAFF).contains(&cp) || (0x2600..=0x27BF).contains(&cp) {
        return Some(' ');
    }
    Some(c)
}

/// Port of `assistant/__init__.py::_clean_nav_label`: strip navigation
/// glyphs so a button label stays plain, glyph-free text.
pub fn clean_nav_label(label: &str) -> String {
    let cleaned: String = label.chars().filter_map(clean_char).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Build a blue, glyph-free "Back" navigation row targeting `name`
/// (a destination's own callback data — the back-navigation mechanism
/// from `assistant/__init__.py::get_back_button`).
pub fn back_button(name: &str, label: &str) -> ultroid_core::tg::InlineButton {
    use ultroid_core::tg::{ButtonStyle, InlineButton};
    InlineButton::data(clean_nav_label(label), name.as_bytes().to_vec())
        .styled(ButtonStyle::Primary)
}

// ── fmtmsg ────────────────────────────────────────────────────────────────
// Port of `custom/fmt.py::FmtKit.fmtmsg`:
// `<b>{head}</b>` + optional padded `<code>key</code> : value` lines
// + optional `<blockquote expandable>{msgs}</blockquote>` + optional
// `<b><blockquote>{foot}</blockquote></b>`, joined with blank lines.
// Head/keys/msgs/foot are HTML-escaped; data values are treated as
// pre-formatted HTML (the call sites pass already-escaped text or
// `rawhtml` links, exactly as in Python).

pub fn fmtmsg(head: &str, data: &[(&str, String)], msgs: &str, foot: &str) -> String {
    fmtmsg_with(head, data, msgs, foot, false)
}

pub fn fmtmsg_with(
    head: &str,
    data: &[(&str, String)],
    msgs: &str,
    foot: &str,
    raw_head: bool,
) -> String {
    let esc = ultroid_core::fns::escape_html;
    let mut parts: Vec<String> = Vec::new();
    let head_text = if raw_head {
        head.to_string()
    } else {
        esc(head)
    };
    parts.push(format!("<b>{head_text}</b>"));
    if !data.is_empty() {
        let pad = data
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = Vec::new();
        for (k, v) in data {
            let key = esc(k);
            let padded = format!("{key:<pad$}", pad = pad);
            lines.push(format!("  <code>{padded}</code> : {v}"));
        }
        parts.push(lines.join("\n"));
    }
    if !msgs.is_empty() {
        parts.push(format!("<blockquote expandable>{}</blockquote>", esc(msgs)));
    }
    if !foot.is_empty() {
        parts.push(format!("<b><blockquote>{}</blockquote></b>", esc(foot)));
    }
    parts.join("\n\n")
}

/// `fmtmsg` with a plain message body only (the common `msgs=` call).
pub fn fmtmsg_text(head: &str, msgs: &str) -> String {
    fmtmsg(head, &[], msgs, "")
}

/// `fmtmsg` with a footer only.
pub fn fmtmsg_foot(head: &str, msgs: &str, foot: &str) -> String {
    fmtmsg(head, &[], msgs, foot)
}

// ── DB helpers ────────────────────────────────────────────────────────────

/// Read a raw udB key (Python `udB.get_key`).
pub async fn db_get(ctx: &Ctx, key: &str) -> Result<Option<String>> {
    ctx.db.get(key).await
}

/// Write a raw udB key (Python `udB.set_key`).
pub async fn db_set(ctx: &Ctx, key: &str, value: &str) -> Result<()> {
    ctx.db.set(key, value).await
}

/// Delete a udB key (Python `udB.del_key`).
pub async fn db_del(ctx: &Ctx, key: &str) -> Result<()> {
    ctx.db.del(key).await
}

/// Python truthiness for udB values stored as `"True"`/`"False"` or
/// native bools serialised the same way.
pub fn is_enabled(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("true") | Some("on") | Some("1") | Some("yes")
    )
}

/// Human display of a stored value: `"Not set"` when absent/empty.
pub fn val_or_not_set(value: Option<&str>) -> String {
    match value {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => "Not set".to_string(),
    }
}

/// `"On"`/`"Off"` toggle display (Python `_toggle` in callbackstuffs).
pub fn toggle_display(value: Option<&str>) -> &'static str {
    if is_enabled(value) { "On" } else { "Off" }
}

/// Mask a secret for display (Python agent.py `_mask`): first 4 +
/// `****` + last 4, `****` when short, `"not set"` when empty.
pub fn mask_secret(value: Option<&str>) -> String {
    let Some(v) = value.filter(|v| !v.is_empty()) else {
        return "not set".to_string();
    };
    if v.chars().count() <= 8 {
        return "****".to_string();
    }
    let first: String = v.chars().take(4).collect();
    let last: String = v
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{first}****{last}")
}

/// Does the message text start a `/cancel` command (Python `_is_cancel`)?
pub fn is_cancel(text: &str) -> bool {
    let first = text.trim().split_whitespace().next().unwrap_or("");
    let cmd = first.split('@').next().unwrap_or("");
    cmd.eq_ignore_ascii_case("/cancel")
}

// ── Time / RNG ────────────────────────────────────────────────────────────

/// Current Unix time, seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Monotonic seconds (Python `time.monotonic` equivalent).
pub fn now_mono() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    start.elapsed().as_secs_f64()
}

/// Format Unix seconds as UTC `YYYY-MM-DD HH:MM` (Python
/// `time.strftime("%Y-%m-%d %H:%M", time.gmtime(...))`).
pub fn fmt_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hour, min) = ((rem / 3600), (rem % 3600) / 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{min:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil-from-days algorithm.
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

static RNG_COUNTER: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);

fn next_rand_u64() -> u64 {
    // xorshift64* seeded from time + a process counter (no `rand` crate
    // in this crate's dependencies).
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545F4914F6CDD1D)
        ^ RNG_COUNTER.fetch_add(0x9E3779B97F4A7C15, Ordering::Relaxed);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545F4914F6CDD1D)
}

/// `secrets.token_hex(6)` equivalent: 12 lowercase hex chars.
pub fn token_hex_6() -> String {
    let a = next_rand_u64();
    let b = next_rand_u64();
    format!("{a:016x}{b:016x}")[..12].to_string()
}

/// Small deterministic RNG (SplitMix64) for reproducible game setups
/// (matrix `enter()` uses `random.Random(seed + revision)` in Python;
/// values differ from CPython's Mersenne Twister, the distribution
/// guarantees — solvable Lights Out grid, shuffled memory deck — are
/// the same).
pub struct SmallRng(pub u64);

impl SmallRng {
    pub fn new(seed: u64) -> Self {
        SmallRng(seed.wrapping_add(0x9E3779B97F4A7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        self.next_u64() % n
    }

    /// Fisher–Yates shuffle, same call order as `random.shuffle`.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below((i + 1) as u64) as usize;
            v.swap(i, j);
        }
    }

    /// `random.sample(range(n), k)` without replacement.
    pub fn sample(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut pool: Vec<usize> = (0..n).collect();
        self.shuffle(&mut pool);
        pool.truncate(k);
        pool
    }
}

// ── Registry adapter ──────────────────────────────────────────────────────

/// `CommandHandler` adapter for plain async handler functions, so each
/// module registers `|ctx| Box::pin(handler(ctx))` instead of a
/// one-off struct per command.
pub struct Handler<F>(pub F);

#[async_trait]
impl<F> CommandHandler for Handler<F>
where
    F: for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> + Send + Sync,
{
    async fn handle(&self, ctx: &Ctx) -> Result<()> {
        (self.0)(ctx).await
    }
}

/// Register one command: `reg_add(registry, def, |ctx| Box::pin(f(ctx)))`.
pub fn reg_add<F>(registry: &mut Registry, def: CommandDef, f: F)
where
    F: for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    registry.add(def, Arc::new(Handler(f)));
}

/// A command definition with the common defaults (dual `.`/`/` handler
/// prefixes come from `Registry::add`).
pub fn def(name: &'static str, pattern: &'static str, help: &'static str) -> CommandDef {
    CommandDef {
        name,
        pattern,
        help,
        ..Default::default()
    }
}
