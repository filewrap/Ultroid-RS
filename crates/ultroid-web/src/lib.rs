//! The web setup / Mini App servers, as one module layer.
//!
//! Port of `pyUltroid/webserver/` (FastAPI + uvicorn + Telethon) to axum +
//! tokio + grammers. Every name below is bound eagerly; the submodules
//! that need grammers are compiled in (the workspace pins grammers 0.12.0
//! everywhere, so there is no optional-dependency gating to preserve).
//!
//! Import contract, carried over from Python: touching this crate must
//! never drag in the bot. The binary crate wires the pieces together.
//!
//! Notes on the port, all deliberate:
//! * `server.py` does not exist in the source checkout at HEAD `5d09531`
//!   — the webserver README describes `python server.py` as a `start.sh`
//!   entry point, but the file itself is absent. Its behaviour (foreground
//!   process, keeps serving `/status` + keep-alive, spawns the bot on
//!   completion) is [`run_standalone_server`].
//! * `startup`'s hard-coded git identity / HF credential line and gist
//!   download are NOT ported (locked exclusion); neither is `wgptPrompt`.
//! * A successful login produces a grammers sqlite session *file*, not a
//!   Telethon `StringSession` string. `SESSION` in `.env` holds that file
//!   path; the binary uses it directly as its session file.
//! * `auth.SentCode.type` is not exposed by grammers 0.12, so the phone
//!   route answers `code_type: null` and the setup page shows the generic
//!   "code sent" message instead of inventing a delivery channel.

pub mod app;
pub mod bootstrap;
pub mod envfile;
pub mod keepalive;
pub mod login;
pub mod logs;
pub mod miniapp;
pub mod runner;
pub mod security;
pub mod setup_page;
pub mod state;

pub use app::{LoginErrorMapper, SetupApplication, create_app};
pub use bootstrap::{
    BOOTSTRAP, ConfigGate, SetupBootstrap, bootstrap, bootstrap_waiter_active, config_complete,
    ensure_config_or_setup, export_env_file, print_setup_url, server_host_port,
    spawn_bot_after_setup, wait_for_config,
};
pub use envfile::{EnvFile, read_env, write_env_atomic};
pub use keepalive::{KeepAliveConfig, KeepAlivePinger, keep_alive, start_keepalive_task};
pub use login::{
    ClientFactory, GrammersClientFactory, LoginError, LoginManager, LoginOutcome, LoginSession,
    LoginSessionUnknown, SessionFactory, default_client_factory, qr_login_url,
};
pub use logs::{BrokerLayer, LogBroker, LogStreamHub, attach_broker};
pub use miniapp::{
    MemoryStore, MiniAppApplication, MiniAppServer, MiniAppState, MiniAppStore, SUPERVISOR,
    create_miniapp, ensure_running, ensure_token, rotate_token, snapshot, supervisor,
};
pub use runner::{PortProbe, RunningServer, bind_available, make_server, serve_forever};
pub use security::{
    RateLimiter, SecretRedactor, TokenAuthority, authority, extract_token, generate_setup_token,
    redactor, token_matches,
};
pub use setup_page::SETUP_PAGE;
pub use state::{SetupState, make_state};

/// The standalone server entry point — the `python server.py` foreground
/// process from `start.sh`, as a Rust async function.
///
/// Serves the setup app on `SETUP_BIND` / `PORT`, keeps serving `/status`
/// plus the HF keep-alive pinger, and spawns the bot (this binary,
/// re-run) when setup completes. Runs until the caller drops the future
/// (the binary installs its own Ctrl-C handler around this call).
pub async fn run_standalone_server(
    env_path: Option<std::path::PathBuf>,
) -> Result<(), ultroid_core::Error> {
    let bootstrap = SetupBootstrap::new(env_path.as_deref());
    let (host, port) = bootstrap.host_port();
    let token = bootstrap.token(|| authority().mint(true));
    bootstrap.banner(&host, port, &token);
    let router = create_app(Some(token), env_path.as_deref(), true, None, None).await;
    let mut pinger = KeepAlivePinger::new(KeepAliveConfig::from_env());
    pinger.start();
    let mut server = RunningServer::start(router, &host, port)
        .await
        .map_err(ultroid_core::Error::Io)?;
    tokio::signal::ctrl_c()
        .await
        .map_err(ultroid_core::Error::Io)?;
    server.stop(Some(std::time::Duration::from_secs(5))).await;
    pinger.stop(std::time::Duration::from_secs(5)).await;
    Ok(())
}
