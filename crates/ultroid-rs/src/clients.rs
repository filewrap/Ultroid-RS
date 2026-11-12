//! One grammers client: session file → `SenderPool` → `Client`.
//!
//! The connection pattern is the one grammers 0.12 requires (there is no
//! `Client::connect()`): open the `SqliteSession`, build a `SenderPool`,
//! take the `Client` from its handle, spawn the pool runner, then open
//! the update stream. Sessions are sqlite files under
//! `data_dir/sessions/<name>.db` — Telethon StringSession strings are
//! NOT portable and are never decoded here.

use grammers_client::client::UpdateStream;
use grammers_client::{
    Client,
    sender::{SenderPool, UpdatesConfiguration},
    session::storages::SqliteSession,
};
use std::sync::Arc;
use ultroid_core::{ClientKind, Config, Error, Result};

/// A connected client with its update stream attached.
pub struct ConnectedClient {
    pub client: Client,
    pub stream: UpdateStream,
    pub kind: ClientKind,
}

fn other<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(e.to_string())
}

/// Connect one client from a session file. The caller authorises it
/// afterwards (`login::ensure_user_authorised` or `bot_sign_in`).
pub async fn connect(
    cfg: &Config,
    session_path: &std::path::Path,
    kind: ClientKind,
) -> Result<ConnectedClient> {
    if let Some(parent) = session_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(Error::Io)?;
    }
    let session = SqliteSession::open(session_path).await.map_err(other)?;
    let session = Arc::new(session);
    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(session, cfg.api_id);
    let client = Client::new(handle);
    tokio::spawn(async move {
        runner.run().await;
        tracing::error!("sender pool stopped");
    });
    let stream = client
        .stream_updates(updates, UpdatesConfiguration::default())
        .await
        .map_err(other)?;
    // Warm the peer cache (grammers only resolves update gaps for peers
    // seen before); one dialog pass is enough.
    let mut dialogs = client.iter_dialogs();
    while let Ok(Some(_)) = dialogs.next().await {}
    Ok(ConnectedClient {
        client,
        stream,
        kind,
    })
}

/// Authorise the assistant bot client with its token.
pub async fn bot_sign_in(client: &Client, cfg: &Config, token: &str) -> Result<()> {
    if client
        .is_authorized()
        .await
        .map_err(|e| Error::Telegram(e.to_string()))?
    {
        return Ok(());
    }
    let user = client
        .bot_sign_in(token, &cfg.api_hash)
        .await
        .map_err(|e| Error::Telegram(e.to_string()))?;
    tracing::info!(
        "bot signed in as @{} (id {})",
        user.username().unwrap_or("?"),
        user.id().bot_api_dialog_id_unchecked()
    );
    Ok(())
}
