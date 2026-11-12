//! The boot gate: decide whether to run the bot or serve web setup.
//!
//! Port of `pyUltroid/webserver/bootstrap.py` — two types, split by what
//! they know about:
//!
//! * [`ConfigGate`] knows about credentials and the environment. It asks
//!   one question — is the bot configured? — and answers it out of the
//!   `.env` file plus the real environment.
//! * [`SetupBootstrap`] knows about *processes*: the port, the PID marker,
//!   the blocking wait, the re-exec. Every one of its methods that touches
//!   the filesystem or the process table is a method on the object, so the
//!   whole flow can be driven from a test by handing it a different marker
//!   path.
//!
//! Rust differences, all documented where they occur:
//! * `python -m pyUltroid` becomes "re-run this binary" — the Rust binary
//!   is both the setup server and the bot entry point, so `spawn_bot`
//!   spawns `std::env::current_exe()` with no arguments and `reexec`
//!   execs it via `std::os::unix::process::CommandExt::exec`.
//! * The PID liveness check uses `/proc/<pid>` (Linux) instead of
//!   `os.kill(pid, 0)`; on other platforms it degrades to "not active".
//! * `ensure()` is `async` and returns `Result<()>` instead of raising
//!   `SystemExit(1)` on the wait timeout — the binary decides the exit
//!   code.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app::create_app;
use crate::envfile::EnvFile;
use crate::runner::{PortProbe, RunningServer};
use crate::security::TokenAuthority;

pub const REQUIRED_KEYS: &[&str] = &["API_ID", "API_HASH", "SESSION"];
pub const DEFAULT_PORT: u16 = 7860;
pub const DEFAULT_BIND: &str = "0.0.0.0";
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(7200);
pub const WAIT_POLL: Duration = Duration::from_secs(2);
pub const MARKER_NAME: &str = ".setup-waiting";
pub const BANNER_WIDTH: usize = 64;

/// Answers "may the bot boot?" from the env file plus the environment.
///
/// The argv short-circuit is part of the contract: `ultroid-rs
/// <api_id> <api_hash> <session>` is how the container passes credentials
/// directly, and in that case there is no reason to interrupt the user
/// with a setup server.
#[derive(Debug, Clone)]
pub struct ConfigGate {
    pub env_path: PathBuf,
    /// `std::env::args()`, program name included — `argv_supplied` mirrors
    /// Python's `len(sys.argv) > 3`.
    pub argv: Vec<String>,
}

impl ConfigGate {
    pub fn new(env_path: Option<impl AsRef<Path>>, argv: Option<Vec<String>>) -> Self {
        Self {
            env_path: env_path
                .map(|p| p.as_ref().to_path_buf())
                .unwrap_or_else(EnvFile::default_path),
            argv: argv.unwrap_or_else(|| std::env::args().collect()),
        }
    }

    pub fn env(&self) -> EnvFile {
        EnvFile::new(Some(&self.env_path))
    }

    pub fn argv_supplied(&self) -> bool {
        self.argv.len() > 3
    }

    /// True when API_ID/API_HASH/SESSION are all usable.
    pub fn complete(&self) -> bool {
        if self.argv_supplied() {
            return true;
        }
        self.env().missing().is_empty()
    }

    /// Seed the process environment from the file; real values always win.
    pub fn export(&self) -> usize {
        self.env().export()
    }
}

/// Owns the setup-server process lifecycle.
#[derive(Debug, Clone)]
pub struct SetupBootstrap {
    pub env_path: PathBuf,
}

impl SetupBootstrap {
    pub fn new(env_path: Option<impl AsRef<Path>>) -> Self {
        Self {
            env_path: env_path
                .map(|p| p.as_ref().to_path_buf())
                .unwrap_or_else(EnvFile::default_path),
        }
    }

    pub fn gate(&self) -> ConfigGate {
        ConfigGate::new(Some(&self.env_path), None)
    }

    pub fn env(&self) -> EnvFile {
        EnvFile::new(Some(&self.env_path))
    }

    /// The PID marker path: next to the `.env` file (Python used the repo
    /// root; the Rust equivalent is the data directory holding `.env`).
    pub fn marker_file(&self) -> PathBuf {
        self.env_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(MARKER_NAME)
    }

    /// True when another `ultroid-rs` process is parked on setup.
    pub fn waiter_active(&self, marker: Option<&Path>) -> bool {
        let target = marker
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.marker_file());
        let Ok(text) = std::fs::read_to_string(&target) else {
            return false;
        };
        let Ok(pid) = text.trim().parse::<u32>() else {
            return false;
        };
        process_alive(pid)
    }

    /// The bind address, from `SETUP_BIND` / `PORT` with safe defaults.
    pub fn host_port(&self) -> (String, u16) {
        let host = std::env::var("SETUP_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
        let port = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        (host, port)
    }

    pub fn token(&self, mint: impl Fn() -> String) -> String {
        std::env::var("SETUP_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(mint)
    }

    /// Print the one-time-token URL the user has to open.
    pub fn banner(&self, host: &str, port: u16, token: &str) {
        let display = if host == DEFAULT_BIND {
            "127.0.0.1"
        } else {
            host
        };
        let rule = "=".repeat(BANNER_WIDTH);
        for line in [
            String::new(),
            rule.clone(),
            "  SarahUB setup: no Telegram config found - starting web setup.".to_string(),
            format!("  Open:  http://{display}:{port}/?token={token}"),
            "  The one-time token above gates every setup API call.".to_string(),
            rule,
            String::new(),
        ] {
            tracing::info!(target: "pyUltroid.webserver.bootstrap", "{line}");
        }
    }

    /// Start the bot as a child process. Returns whether we did.
    /// The child is this binary itself, re-run with no arguments.
    pub fn spawn_bot(&self) -> bool {
        if std::env::var_os("SETUP_NO_SPAWN").is_some() {
            tracing::info!(target: "pyUltroid.webserver.bootstrap", "setup: SETUP_NO_SPAWN set - not spawning the bot.");
            return false;
        }
        if self.waiter_active(None) {
            tracing::info!(target: "pyUltroid.webserver.bootstrap", "setup: a bootstrap waiter will boot the bot - not spawning.");
            return false;
        }
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        tracing::info!(target: "pyUltroid.webserver.bootstrap", "setup: starting the bot…");
        match std::process::Command::new(exe)
            .current_dir(self.env_path.parent().unwrap_or_else(|| Path::new(".")))
            .spawn()
        {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(target: "pyUltroid.webserver.bootstrap", "setup: failed to spawn the bot: {err}");
                false
            }
        }
    }

    /// Replace this process with a fresh copy of this binary.
    #[cfg(unix)]
    pub fn reexec(&self) -> ! {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().expect("current_exe");
        let err = std::process::Command::new(exe).exec();
        panic!("reexec failed: {err}");
    }

    /// Replace this process with a fresh copy of this binary.
    /// Non-Unix fallback: spawn a child and exit (there is no `exec`).
    #[cfg(not(unix))]
    pub fn reexec(&self) -> ! {
        self.spawn_bot();
        std::process::exit(0);
    }

    /// Return immediately when configured; otherwise serve setup.
    ///
    /// Returns only when the bot is configured (or web setup is
    /// unavailable), because a completed setup ends in [`SetupBootstrap::reexec`].
    pub async fn ensure(&self) -> Result<(), ultroid_core::Error> {
        let gate = self.gate();
        if gate.complete() {
            return Ok(());
        }
        let (host, port) = self.host_port();
        if !PortProbe.bindable(&host, port) {
            return self.wait_for_config(&host, port).await;
        }
        let authority = Arc::new(TokenAuthority::new(None));
        let token = self.token(|| authority.mint(true));
        let (done_tx, mut done_rx) = tokio::sync::watch::channel(false);
        let on_complete = {
            let done_tx = done_tx.clone();
            Arc::new(move || {
                let _ = done_tx.send(true);
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let router = create_app(
            Some(token.clone()),
            Some(&self.env_path),
            false,
            Some(on_complete),
            None,
        )
        .await;
        let mut server = RunningServer::start(router, &host, port).await?;
        self.banner(&host, port, &token);
        while !*done_rx.borrow() {
            if done_rx.changed().await.is_err() {
                break;
            }
        }
        server.stop(Some(Duration::from_secs(5))).await;
        gate.export();
        tracing::info!(target: "pyUltroid.webserver.bootstrap", "Setup complete - restarting into ultroid-rs…");
        self.reexec();
    }

    /// Another process owns the setup port: wait for it to finish setup.
    ///
    /// Publishes our PID in the marker while waiting, so a concurrently
    /// starting server knows not to spawn a second bot. Errors when the
    /// deadline passes (Python raised `SystemExit(1)`).
    pub async fn wait_for_config(&self, host: &str, port: u16) -> Result<(), ultroid_core::Error> {
        let timeout = std::env::var("SETUP_WAIT_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(Duration::from_secs_f64)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT);
        let marker = self.marker_file();
        let _ = std::fs::write(&marker, std::process::id().to_string());
        let result = async {
            tracing::info!(target: "pyUltroid.webserver.bootstrap", "setup: {host}:{port} is served by another process - waiting for web setup to complete there…");
            let gate = self.gate();
            let deadline = Instant::now() + timeout;
            loop {
                if gate.complete() {
                    tracing::info!(target: "pyUltroid.webserver.bootstrap", "setup: config detected - continuing normal bootstrap.");
                    gate.export();
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    tracing::warn!(target: "pyUltroid.webserver.bootstrap", "setup: timed out waiting for web setup to complete.");
                    return Err(ultroid_core::Error::Other(
                        "timed out waiting for web setup to complete".to_string(),
                    ));
                }
                tokio::time::sleep(WAIT_POLL).await;
            }
        }
        .await;
        let _ = std::fs::remove_file(&marker);
        result
    }
}

impl Default for SetupBootstrap {
    fn default() -> Self {
        Self::new(None::<&Path>)
    }
}

/// The shared singleton (Python's module-level `BOOTSTRAP`).
pub static BOOTSTRAP: std::sync::OnceLock<SetupBootstrap> = std::sync::OnceLock::new();

pub fn bootstrap() -> &'static SetupBootstrap {
    BOOTSTRAP.get_or_init(SetupBootstrap::default)
}

/// True when `pid` names a live process. Linux: `/proc/<pid>` exists.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

// -- module surface -------------------------------------------------------

pub fn export_env_file(env_path: Option<impl AsRef<Path>>) {
    SetupBootstrap::new(env_path).gate().export();
}

pub fn config_complete(env_path: Option<impl AsRef<Path>>) -> bool {
    ConfigGate::new(env_path, None).complete()
}

pub fn server_host_port() -> (String, u16) {
    bootstrap().host_port()
}

pub fn print_setup_url(host: &str, port: u16, token: &str) {
    bootstrap().banner(host, port, token);
}

pub async fn ensure_config_or_setup(
    env_path: Option<impl AsRef<Path>>,
) -> Result<(), ultroid_core::Error> {
    SetupBootstrap::new(env_path).ensure().await
}

pub async fn wait_for_config(
    host: &str,
    port: u16,
    env_path: Option<impl AsRef<Path>>,
) -> Result<(), ultroid_core::Error> {
    SetupBootstrap::new(env_path)
        .wait_for_config(host, port)
        .await
}

pub fn bootstrap_waiter_active(marker: Option<&Path>) -> bool {
    bootstrap().waiter_active(marker)
}

pub fn spawn_bot_after_setup() -> bool {
    bootstrap().spawn_bot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_argv_short_circuit() {
        let gate = ConfigGate::new(
            None::<&Path>,
            Some(vec![
                "ultroid-rs".into(),
                "1".into(),
                "hash".into(),
                "sess".into(),
            ]),
        );
        assert!(gate.argv_supplied());
        assert!(gate.complete());
        let gate = ConfigGate::new(None::<&Path>, Some(vec!["ultroid-rs".into()]));
        assert!(!gate.argv_supplied());
    }

    #[test]
    fn host_port_defaults_and_env() {
        let b = SetupBootstrap::default();
        let (host, port) = b.host_port();
        assert_eq!(
            host,
            std::env::var("SETUP_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string())
        );
        assert_eq!(
            port,
            std::env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PORT)
        );
    }

    #[test]
    fn waiter_active_rejects_garbage_marker() {
        let dir = std::env::temp_dir().join(format!("ultroid-web-bs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(".setup-waiting");
        std::fs::write(&marker, "not-a-pid").unwrap();
        let b = SetupBootstrap::default();
        assert!(!b.waiter_active(Some(&marker)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
