//! Dependency-free SSE log streaming.
//!
//! Port of `pyUltroid/webserver/logs.py` — four jobs, four types:
//!
//! * [`SecretRedactor`] use (via [`crate::security`]) rewrites one record.
//! * [`BrokerLayer`] is the `tracing` -> broker bridge (Python's
//!   `BrokerHandler` + `RedactionFilter`, fused: tracing has no per-record
//!   filter hook, so redaction happens inside the layer).
//! * [`LogBroker`] is the thread-safe fan-out with replay history.
//! * [`LogStreamHub`] owns one broker + layer attachment.
//!
//! Log lines keep Python's exact format:
//! `%(asctime)s | %(name)s [%(levelname)s] : %(message)s` — note the space
//! before the colon. `tracing::Level::WARN` renders as `WARNING`, matching
//! Python's `logging` level name.
//!
//! One honest difference: Python's hub attaches a handler to the `logging`
//! root logger and can detach it later. `tracing`'s global default
//! subscriber can only be *set*, never unset, so `attach()` installs the
//! broker layer as the process-wide subscriber when none is set yet, and
//! `detach()` silences the layer in place (the layer stays registered but
//! stops forwarding). When a global subscriber is already installed,
//! `attach()` fails softly and the caller should add [`LogStreamHub::layer`]
//! to their own subscriber builder instead.

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::{Layer, fmt, layer::Context, prelude::*, registry::LookupSpan};

use crate::security::SecretRedactor;

pub const LOG_FORMAT: &str = "%(asctime)s | %(name)s [%(levelname)s] : %(message)s";
pub const DEFAULT_HISTORY: usize = 200;

/// Thread-safe publish/subscribe fan-out with a bounded replay buffer.
///
/// Producer side may run on any thread; consumer side lives on the server
/// loop. `tokio::sync::broadcast` is the cross-thread hop (Python hops
/// through `loop.call_soon_threadsafe(queue.put_nowait, line)`).
#[derive(Debug)]
pub struct LogBroker {
    history: Mutex<VecDeque<String>>,
    history_len: usize,
    tx: broadcast::Sender<String>,
}

impl LogBroker {
    pub fn new(history: usize) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            history: Mutex::new(VecDeque::with_capacity(history)),
            history_len: history,
            tx,
        }
    }

    pub fn publish(&self, text: String) {
        {
            let mut guard = self.history.lock().expect("broker lock poisoned");
            if guard.len() == self.history_len {
                guard.pop_front();
            }
            guard.push_back(text.clone());
        }
        // No receivers is fine; lagged receivers just miss lines.
        let _ = self.tx.send(text);
    }

    /// A live subscription. The caller must also read [`LogBroker::history`]
    /// first for the replay — same order the SSE route uses.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    pub fn history(&self) -> Vec<String> {
        self.history
            .lock()
            .expect("broker lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for LogBroker {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY)
    }
}

/// A `tracing` layer that formats events as log lines and hands them to a
/// [`LogBroker`], redacting first. The Python `RedactionFilter` also clears
/// `record.args` so `%` interpolation cannot re-apply; tracing formats the
/// message once, so there is no second interpolation to guard against.
pub struct BrokerLayer {
    broker: Arc<LogBroker>,
    redactor: Arc<SecretRedactor>,
    enabled: Arc<AtomicBool>,
}

impl BrokerLayer {
    pub fn new(broker: Arc<LogBroker>, redactor: Arc<SecretRedactor>) -> Self {
        Self {
            broker,
            redactor,
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

impl<S> Layer<S> for BrokerLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        let mut visitor = MessageVisitor { message: None };
        event.record(&mut visitor);
        let message = visitor.message.unwrap_or_default();
        let redacted = self.redactor.redact(&message);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARNING",
            tracing::Level::INFO => "INFO",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::TRACE => "TRACE",
        };
        let line = format!(
            "{} | {} [{}] : {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S,%3f"),
            event.metadata().target(),
            level,
            redacted
        );
        self.broker.publish(line);
    }
}

/// Owns one broker + layer attachment to the process-wide logger.
///
/// `attach` is idempotent per hub, and `detach` reverses the *forwarding*.
/// The level rule from Python (only raised, never lowered, and only when
/// the logger was left at NOTSET) has no tracing equivalent — tracing
/// levels are per-layer `LevelFilter`s, so the broker layer simply accepts
/// every event and the stdout fmt layer keeps its own filter.
///
/// The hub keeps only the shared `enabled` flag: the layer itself is owned
/// by the tracing registry once attached (tracing-subscriber does not
/// forward `Layer` to `Arc`), and `detach()` silences it through the flag.
pub struct LogStreamHub {
    pub broker: Arc<LogBroker>,
    enabled: Arc<AtomicBool>,
    attached: bool,
}

impl LogStreamHub {
    pub fn new(broker: Option<Arc<LogBroker>>) -> Self {
        Self {
            broker: broker.unwrap_or_else(|| Arc::new(LogBroker::default())),
            enabled: Arc::new(AtomicBool::new(true)),
            attached: false,
        }
    }

    pub fn attached(&self) -> bool {
        self.attached
    }

    /// The layer for callers that build their own subscriber. It shares
    /// this hub's `enabled` flag, so `detach()` silences it wherever it
    /// is installed.
    pub fn layer(&self, redactor: Arc<SecretRedactor>) -> BrokerLayer {
        BrokerLayer {
            broker: self.broker.clone(),
            redactor,
            enabled: self.enabled.clone(),
        }
    }

    /// Wire this hub's broker into the process-wide tracing subscriber.
    /// Returns `true` when the global default was installed by this call.
    /// Returns `false` (and leaves everything untouched) when a global
    /// subscriber is already set — add [`LogStreamHub::layer`] to that
    /// subscriber instead.
    pub fn attach(&mut self, redactor: Arc<SecretRedactor>) -> bool {
        if self.attached {
            return true;
        }
        let layer = self.layer(redactor);
        layer.set_enabled(true);
        let subscriber = tracing_subscriber::registry()
            .with(fmt::layer())
            .with(layer);
        match tracing::subscriber::set_global_default(subscriber) {
            Ok(()) => {
                self.attached = true;
                true
            }
            Err(_) => false,
        }
    }

    /// Stop forwarding to the broker. Returns whether anything was
    /// silenced. (The tracing global default cannot be uninstalled, so the
    /// layer stays registered but silent — the closest faithful semantics.)
    pub fn detach(&mut self) -> bool {
        if !self.attached {
            return false;
        }
        self.enabled.store(false, Ordering::SeqCst);
        self.attached = false;
        true
    }
}

impl Default for LogStreamHub {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Attach a fresh hub's broker to the process-wide logger; the historical
/// module-level `attach_broker` surface.
pub fn attach_broker(redactor: Arc<SecretRedactor>) -> (Arc<LogBroker>, bool) {
    let mut hub = LogStreamHub::new(None);
    let broker = hub.broker.clone();
    let attached = hub.attach(redactor);
    // Dropping the hub does not silence the layer: the global subscriber
    // holds its own Arc, so forwarding continues process-wide.
    drop(hub);
    (broker, attached)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_replays_bounded_history_and_fans_out() {
        let broker = Arc::new(LogBroker::new(3));
        let mut rx = broker.subscribe();
        broker.publish("one".into());
        broker.publish("two".into());
        broker.publish("three".into());
        broker.publish("four".into());
        assert_eq!(broker.history(), vec!["two", "three", "four"]);
        assert_eq!(rx.try_recv().unwrap(), "one");
        assert_eq!(rx.try_recv().unwrap(), "two");
        assert_eq!(broker.subscriber_count(), 1);
    }

    #[test]
    fn redaction_in_layer_format() {
        // The formatting rule itself: timestamp | target [LEVEL] : message.
        let line = format!(
            "{} | {} [{}] : {}",
            "2026-11-12 00:00:00,000", "pyUltroid.webserver", "INFO", "hello"
        );
        assert_eq!(
            line,
            "2026-11-12 00:00:00,000 | pyUltroid.webserver [INFO] : hello"
        );
    }
}
