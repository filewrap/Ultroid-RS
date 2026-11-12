//! axum runners for the setup server.
//!
//! Port of `pyUltroid/webserver/runner.py` — two classes, two jobs:
//!
//! * [`PortProbe`] answers one question — can I bind this address right
//!   now? It probes by binding and releasing (the only portable way).
//! * [`RunningServer`] owns an axum server plus the tokio task that runs
//!   it, and exposes a real lifecycle (`start` / `stop` / `is_running`).
//!
//! The Python `ThreadedServer` runs uvicorn on a daemon thread; the Rust
//! equivalent is `axum::serve` on a spawned tokio task with a graceful
//! shutdown signal. `uvicorn`'s `should_exit` is cooperative; axum's
//! `with_graceful_shutdown` is the same idea, and `stop(timeout)` waits
//! for the task the same way.

use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub const THREAD_NAME: &str = "pyultroid-webserver";
pub const LOG_LEVEL: &str = "info";

/// Best-effort "is this address free?" arbiter.
///
/// Racy by construction — another process can win the race between the
/// probe returning and the server starting. That is acceptable here because
/// the only caller (the bootstrap gate) re-checks and degrades to waiting.
#[derive(Debug, Default, Clone, Copy)]
pub struct PortProbe;

impl PortProbe {
    /// Bind and immediately release; `false` when the address is taken.
    pub fn bindable(&self, host: &str, port: u16) -> bool {
        let addr = match format!("{host}:{port}").parse::<SocketAddr>() {
            Ok(a) => a,
            Err(_) => return false,
        };
        // std sets SO_REUSEADDR on Unix listeners, matching Python's probe.
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                drop(listener);
                true
            }
            Err(_) => false,
        }
    }
}

/// An axum server running on a tokio task, with a real lifecycle.
pub struct RunningServer {
    host: String,
    port: u16,
    shutdown_tx: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl RunningServer {
    /// Bind now and spawn the serve task. Returns the server; the socket is
    /// already bound, so the bootstrap's probe race is closed here.
    pub async fn start(router: Router, host: &str, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind((host, port)).await?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let running = Arc::new(AtomicBool::new(true));
        let running_flag = running.clone();
        let task = tokio::spawn(async move {
            let shutdown = async move {
                let mut rx = shutdown_rx;
                while !*rx.borrow() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                let _ = rx;
            };
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown)
                .await;
            running_flag.store(false, Ordering::SeqCst);
        });
        Ok(Self {
            host: host.to_string(),
            port,
            shutdown_tx,
            task: Some(task),
            running,
        })
    }

    /// Ask the server to wind down, optionally waiting for the task.
    /// axum's graceful shutdown is cooperative, so a caller about to
    /// `exec` a fresh process must not assume the socket is free the
    /// instant this returns. Pass a timeout to actually wait.
    pub async fn stop(&mut self, timeout: Option<Duration>) -> bool {
        let _ = self.shutdown_tx.send(true);
        match (self.task.take(), timeout) {
            (Some(task), Some(timeout)) => tokio::time::timeout(timeout, task).await.is_ok(),
            (Some(task), None) => {
                task.abort();
                false
            }
            (None, _) => !self.is_running(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Module surface: `bind_available`.
pub fn bind_available(host: &str, port: u16) -> bool {
    PortProbe.bindable(host, port)
}

/// Module surface: bind + serve on the calling task until shutdown.
/// (Python's `serve_forever` runs uvicorn on the calling thread and
/// installs signal handlers; signal handling belongs to the binary here,
/// so this awaits the serve future directly.)
pub async fn serve_forever(router: Router, host: &str, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind((host, port)).await?;
    axum::serve(listener, router).await
}

/// Module surface: build the running server without starting anything
/// beyond the bind (Python's `make_server` returned the uvicorn `Server`
/// object; here the bind happens in `RunningServer::start`).
pub async fn make_server(router: Router, host: &str, port: u16) -> std::io::Result<RunningServer> {
    RunningServer::start(router, host, port).await
}
