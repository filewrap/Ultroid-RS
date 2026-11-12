//! Agent engine lifecycle, started/stopped WITH the bot — port of
//! `pyUltroid/agent/_lifecycle.py`.
//!
//! The Python module registers the process agent with the universal
//! plugin lifecycle owned by `asst` / `ultroid_bot`. Those runtime
//! objects have no counterpart inside this crate, so registration is
//! split in two: [`AgentLifecycle`] (the engine itself — start/stop,
//! provider logging, in-flight shutdown) and the module-level
//! [`register_lifecycle`] flag, which the binary crate flips once it
//! has attached the lifecycle to its own plugin registry. Calling
//! `register_lifecycle()` without that attachment returns `false`,
//! exactly like the Python version returning `False` when the
//! lifecycle object is missing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, info, warn};

use ultroid_core::db::Database;

use crate::config::AgentConfig;
use crate::mini::MiniAI;
use crate::providers::ProviderRegistry;

/// Opaque registration context, stored and passed back on stop. The
/// Python source stores the universal-plugin context object without
/// ever reading it; the binary crate supplies whatever it uses.
#[derive(Debug, Clone, Default)]
pub struct LifecycleContext {
    pub id: String,
}

pub struct AgentLifecycle {
    pub id: &'static str,
    pub started: bool,
    context: Option<LifecycleContext>,
    mini: Option<Arc<MiniAI>>,
    db: Option<Arc<dyn Database>>,
}

impl AgentLifecycle {
    pub const ID: &'static str = "ultroid.agent";

    /// *db* feeds the provider registry's `AgentConfig`; without it
    /// the start log cannot list configured providers.
    pub fn new(mini: Option<Arc<MiniAI>>, db: Option<Arc<dyn Database>>) -> Self {
        AgentLifecycle {
            id: Self::ID,
            started: false,
            context: None,
            mini,
            db,
        }
    }

    // ── universal plugin contract ──

    pub fn lifecycle_register(&mut self, context: LifecycleContext) {
        self.context = Some(context);
    }

    pub async fn lifecycle_start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let configured: Vec<String> = match &self.db {
            Some(db) => {
                let registry = ProviderRegistry::new(AgentConfig::new(Arc::clone(db)));
                registry
                    .all()
                    .iter()
                    .filter(|p| p.is_configured())
                    .map(|p| p.name().to_string())
                    .collect()
            }
            None => Vec::new(),
        };
        info!(
            "[AGENT:LIFECYCLE] engine started  -  providers: {}",
            if configured.is_empty() {
                "none".to_string()
            } else {
                configured.join(", ")
            }
        );

        if let Some(db) = &self.db {
            let registry = ProviderRegistry::new(AgentConfig::new(Arc::clone(db)));
            if let Ok(ollama) = registry.provider("ollama") {
                let running = ollama.verify().await;
                info!(
                    "[AGENT:LIFECYCLE] local Ollama provider: {}",
                    if running {
                        "reachable"
                    } else {
                        "not running (cloud escalation only)"
                    }
                );
            }
        }
    }

    pub async fn lifecycle_stop(&mut self) {
        if !self.started {
            return;
        }
        self.started = false;

        if let Some(mini) = &self.mini {
            // The lifecycle owns the same MiniAI the agent runs on, so
            // its in-flight map is right here. Stamping a fresh MiniAI
            // would wake nobody — the bug a module singleton was
            // silently papering over in Python.
            let stamped = mini.shutdown_inflight().await;
            if stamped > 0 {
                debug!("[AGENT:LIFECYCLE] stamped {stamped} in-flight request(s) with shutdown");
            }
        }
        info!("[AGENT:LIFECYCLE] engine stopped");
        let _ = &self.context;
    }
}

// ── registration ──────────────────────────────────────────────────────────

/// Delay before shutdown-stamped cache-key results are swept. (The
/// sweep itself lives in `MiniAI::shutdown_inflight`.)
pub const SHUTDOWN_RESULT_SWEEP_DELAY_SECS: u64 = 5;

static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Attach the engine lifecycle to the installed universal plugin
/// lifecycle. Idempotent — a hot-reload never double-registers.
///
/// In this crate there is no bot runtime to attach to, so this only
/// reports the flag the binary crate sets with
/// [`mark_lifecycle_registered`]. It returns `true` when attached (or
/// already attached), `false` when the attachment has not happened.
pub fn register_lifecycle() -> bool {
    if REGISTERED.load(Ordering::SeqCst) {
        return true;
    }
    debug!(
        "[AGENT:LIFECYCLE] registration skipped: no universal plugin lifecycle in this crate; the binary crate must call mark_lifecycle_registered() after attaching"
    );
    false
}

/// The binary crate calls this after it has registered the process
/// agent's lifecycle with its universal plugin registry.
pub fn mark_lifecycle_registered() {
    REGISTERED.store(true, Ordering::SeqCst);
    info!("[AGENT:LIFECYCLE] registered with the universal plugin lifecycle");
}

pub fn lifecycle_registered() -> bool {
    REGISTERED.load(Ordering::SeqCst)
}

/// Warn helper kept for parity with the Python module's log call in
/// `lifecycle_stop`'s exception path.
#[allow(dead_code)]
fn log_stop_error(error: &str) {
    warn!("[AGENT:LIFECYCLE]: Exception: {error}");
}
