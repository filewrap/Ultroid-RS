//! `ultroid-rs` — the binary: clients, update loop, responder wiring.
//!
//! Port of the Python entry point (`pyUltroid/__main__` + `startup/`):
//! the setup gate runs first (missing API keys → the axum setup server,
//! which re-execs this binary on completion), then the database opens,
//! the plugin + assistant registries build, the grammers clients
//! connect (user, assistant bot, optional SESSION2/3), the VC engine
//! starts on the user client, the mini-app settings server starts, and
//! every update flows through `Registry::dispatch`.
//!
//! Rich messages go out ONLY on the assistant-bot client (see
//! `responder.rs`); the user client refuses them. Sessions are
//! grammers sqlite files — never Telethon StringSession strings.

mod clients;
mod login;
mod responder;

use grammers_client::{
    Client,
    message::Message as GramMessage,
    session::types::{PeerAuth, PeerId, PeerKind, PeerRef},
    tl,
    update::{CallbackQuery, Update},
};
use responder::GrammersResponder;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use ultroid_core::{
    ChatType, ClientKind, Config, Ctx, Database, Error, Registry, Result, Strings,
    business::GuestMode, db::open_database, listen::WaiterHub, notices::sanitize_error_text,
};
use ultroid_vc::{Bridge, Engine, VcDatabase};

/// Shared state for every update loop.
struct Shared {
    config: Arc<Config>,
    db: Arc<dyn Database>,
    registry: Arc<Registry>,
    waiters: WaiterHub,
    strings: Arc<Strings>,
    guests: GuestMode,
    http: reqwest::Client,
    bridge: Option<Arc<Bridge>>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // First run: missing API keys → setup server (re-execs on success).
    // This never returns while setup is pending.
    ultroid_web::bootstrap().ensure().await?;

    let config = Arc::new(Config::from_env()?);
    let db = open_database(&config).await?;
    ultroid_agent::lifecycle::mark_lifecycle_registered();

    let mut registry = Registry::new();
    ultroid_plugins::register(&mut registry);
    registry.set_plugin("assistant");
    ultroid_assistant::register(&mut registry);
    let registry = Arc::new(registry);
    tracing::info!(
        "registered {} commands (plugins + assistant)",
        registry.command_count()
    );

    let strings = Strings::load(&config.lang);
    let guests = GuestMode::new(db.clone());

    // -- clients (connected first, loops spawned after Shared exists) -----
    // User client (primary). Skipped in BOT_MODE, where only the bot runs.
    let mut connected_clients: Vec<clients::ConnectedClient> = Vec::new();
    let mut user_client: Option<Client> = None;
    if !config.bot_mode {
        let connected =
            clients::connect(&config, &config.session_path("ultroid"), ClientKind::User).await?;
        login::ensure_user_authorised(&connected.client, &config.api_hash).await?;
        let me = connected
            .client
            .get_me()
            .await
            .map_err(|e| Error::Telegram(e.to_string()))?;
        tracing::info!(
            "user client signed in as @{} (id {})",
            me.username().unwrap_or("?"),
            me.id().bot_api_dialog_id_unchecked()
        );
        user_client = Some(connected.client.clone());
        connected_clients.push(connected);
    }

    // Assistant bot client.
    if let Some(token) = &config.bot_token {
        let connected =
            clients::connect(&config, &config.session_path("asst"), ClientKind::AsstBot).await?;
        clients::bot_sign_in(&connected.client, &config, token).await?;
        connected_clients.push(connected);
    }

    // Extra user accounts.
    for (name, present) in [
        ("session2", config.session2.is_some()),
        ("session3", config.session3.is_some()),
    ] {
        if present {
            let connected =
                clients::connect(&config, &config.session_path(name), ClientKind::ExtraUser)
                    .await?;
            login::ensure_user_authorised(&connected.client, &config.api_hash).await?;
            connected_clients.push(connected);
        }
    }

    // -- voice chat: real ntgcalls engine on the user client ---------------
    // (BOT_MODE disables voice entirely, mirroring runtime.py.)
    let mut bridge: Option<Arc<Bridge>> = None;
    if let Some(user) = &user_client {
        if !config.bot_mode {
            let b = Bridge::new(user.clone());
            // NTgCalls is Send but NOT Sync, so Arc<Engine> can never
            // cross a thread boundary (tokio::spawn and std::thread::spawn
            // both refuse it). The engine is therefore BUILT inside its
            // own OS thread with a current-thread runtime and stays there;
            // only Send handles (the Bridge, the DB) move across.
            let bridge_for_vc = b.clone();
            let db_for_vc = db.clone();
            std::thread::Builder::new()
                .name("vc-engine".into())
                .spawn(move || {
                    let vc_db = VcDatabase::new(db_for_vc);
                    let engine = Engine::new(bridge_for_vc, Arc::new(vc_db));
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("vc engine runtime");
                    rt.block_on(engine.run());
                })?;
            tracing::info!("voice engine started on the user client");
            bridge = Some(b);
        }
    }

    let shared = Arc::new(Shared {
        config: config.clone(),
        db: db.clone(),
        registry,
        waiters: WaiterHub::new(),
        strings,
        guests,
        http: reqwest::Client::new(),
        bridge,
    });

    let mut handles = Vec::new();
    for connected in connected_clients {
        handles.push(tokio::spawn(run_update_loop(shared.clone(), connected)));
    }

    // -- mini-app settings server ------------------------------------------
    {
        let router = ultroid_web::miniapp::create_miniapp(
            None,
            Some(Arc::new(ultroid_web::miniapp::LiveDb::new(db.clone()))),
            None,
        )
        .await;
        let running = ultroid_web::miniapp::ensure_running(
            router,
            "127.0.0.1",
            ultroid_web::miniapp::DEFAULT_MINIAPP_PORT,
        )
        .await;
        tracing::info!("mini-app settings server running: {running}");
    }

    // -- wait forever (update loops never return on their own) -------------
    for handle in handles {
        let _ = handle.await;
    }
    Ok(())
}

/// One client's update loop.
async fn run_update_loop(shared: Arc<Shared>, mut connected: clients::ConnectedClient) {
    let kind = connected.kind;
    loop {
        let update = match connected.stream.next().await {
            Ok(update) => update,
            Err(e) => {
                tracing::warn!("update stream error ({kind:?}): {e}");
                continue;
            }
        };
        if let Err(e) = handle_update(&shared, &connected.client, kind, update).await {
            tracing::warn!("update handling failed ({kind:?}): {e}");
        }
    }
}

async fn handle_update(
    shared: &Arc<Shared>,
    client: &Client,
    kind: ClientKind,
    update: Update,
) -> Result<()> {
    match update {
        // The update wrapper derefs to `grammers_client::message::Message`
        // (its `msg` field is `pub(crate)`); `&msg` coerces through Deref.
        Update::NewMessage(msg) | Update::MessageEdited(msg) => {
            handle_message(shared, client, kind, &msg).await
        }
        Update::MessageDeleted(_) => {
            tracing::debug!("message deleted");
            Ok(())
        }
        Update::CallbackQuery(query) => handle_callback(shared, client, kind, &query).await,
        Update::Raw(raw) => {
            // Voice-chat signalling fan-out (py-tgcalls Raw handler).
            if let Some(bridge) = &shared.bridge {
                if let Some(chat_id) = raw_chat_id(&raw.raw, bridge).await {
                    bridge.handle_raw_update(&raw.raw, chat_id).await;
                }
            }
            Ok(())
        }
        // Inline queries / inline sends / guest-chat mentions are answered
        // by the inline engine (plugins::inline, wave C). Until that
        // module lands, they are logged, never answered with fake data.
        other => {
            tracing::debug!("unhandled update: {other:?}");
            Ok(())
        }
    }
}

/// Build a `Ctx` from a grammers message and dispatch it.
async fn handle_message(
    shared: &Arc<Shared>,
    client: &Client,
    kind: ClientKind,
    msg: &GramMessage,
) -> Result<()> {
    let peer_id = msg.peer_id();
    let chat_id = peer_id
        .bot_api_dialog_id()
        .unwrap_or_else(|| peer_id.bot_api_dialog_id_unchecked());
    let chat_type = chat_type_of(msg);
    let sender_id = msg
        .sender_id()
        .and_then(|p| p.bot_api_dialog_id())
        .unwrap_or(0);
    let text = msg.text().to_string();
    let reply_to_id = msg.reply_to_message_id().map(|i| i as i64);
    let reply_to_text = match msg.reply_to_message_id() {
        Some(_) => client
            .get_reply_to_message(msg)
            .await
            .ok()
            .flatten()
            .map(|m| m.text().to_string()),
        None => None,
    };

    let peer_ref = match msg.peer_ref().await {
        Ok(Some(peer)) => peer,
        _ => PeerRef {
            id: peer_id,
            auth: PeerAuth::default(),
        },
    };
    let responder = Arc::new(GrammersResponder::new(
        client.clone(),
        peer_ref,
        chat_id,
        kind,
    ));

    // `listen()` waiters see every message, matched or not.
    shared
        .waiters
        .deliver(ultroid_core::listen::ListenedMessage {
            chat_id,
            sender_id,
            message_id: msg.id() as i64,
            text: text.clone(),
            reply_to_id,
        })
        .await;

    let mut ctx = Ctx {
        text,
        args: String::new(),
        captures: Vec::new(),
        command: "",
        chat_id,
        chat_type,
        sender_id,
        message_id: msg.id() as i64,
        reply_to_id,
        reply_to_text,
        is_owner: sender_id != 0 && sender_id == shared.config.owner_id,
        is_sudo: shared.config.is_sudo(sender_id),
        is_dev: shared.config.is_dev(sender_id),
        is_guest: shared.guests.is_guest(sender_id).await.unwrap_or(false),
        via_bot: msg.via_bot_id().is_some(),
        db: shared.db.clone(),
        config: shared.config.clone(),
        http: shared.http.clone(),
        responder,
        lang: shared.config.lang.clone(),
        strings: shared.strings.clone(),
        client_kind: kind,
        registry: shared.registry.clone(),
    };
    match shared.registry.dispatch(&mut ctx).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let clean = sanitize_error_text(&e.to_string());
            tracing::warn!("command '{}' failed: {clean}", ctx.command);
            let _ = ctx.reply(&format!("Error: {clean}")).await;
            Ok(())
        }
    }
}

/// Route an inline-button callback through the assistant handlers.
/// Unknown callback data is answered exactly "🖐️" (no alert) — the
/// Phase-10 ruling for unknown callbacks.
async fn handle_callback(
    shared: &Arc<Shared>,
    client: &Client,
    kind: ClientKind,
    query: &CallbackQuery,
) -> Result<()> {
    let data = String::from_utf8_lossy(query.data()).into_owned();
    let peer_id = query.peer_id();
    let chat_id = peer_id
        .bot_api_dialog_id()
        .unwrap_or_else(|| peer_id.bot_api_dialog_id_unchecked());
    let message_id = match &query.raw {
        tl::enums::Update::BotCallbackQuery(u) => u.msg_id as i64,
        tl::enums::Update::InlineBotCallbackQuery(_) => 0,
        _ => 0,
    };
    let sender_id = query.sender_id().bot_api_dialog_id().unwrap_or(0);
    let peer_ref = PeerRef {
        id: peer_id,
        auth: PeerAuth::default(),
    };
    let responder = Arc::new(GrammersResponder::new(
        client.clone(),
        peer_ref,
        chat_id,
        kind,
    ));
    let mut ctx = Ctx {
        text: data.clone(),
        args: String::new(),
        captures: Vec::new(),
        command: "",
        chat_id,
        chat_type: ChatType::Unknown,
        sender_id,
        message_id,
        reply_to_id: None,
        reply_to_text: None,
        is_owner: sender_id != 0 && sender_id == shared.config.owner_id,
        is_sudo: shared.config.is_sudo(sender_id),
        is_dev: shared.config.is_dev(sender_id),
        is_guest: shared.guests.is_guest(sender_id).await.unwrap_or(false),
        via_bot: false,
        db: shared.db.clone(),
        config: shared.config.clone(),
        http: shared.http.clone(),
        responder,
        lang: shared.config.lang.clone(),
        strings: shared.strings.clone(),
        client_kind: kind,
        registry: shared.registry.clone(),
    };

    // Each assistant handler reports whether it owned the data.
    let handled = ultroid_assistant::start::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::initial::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::localization::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::callbackstuffs::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::games::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::ludo_arcade::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::matrix_arcade::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::rich_showcase::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::ytdl::handle_callback(&mut ctx, &data).await?
        || ultroid_assistant::agent::handle_callback(&mut ctx, &data).await?;
    if !handled {
        // Unknown callback: exactly "🖐️", no alert.
        if let Err(e) = query.answer().text("🖐️").send().await {
            tracing::debug!("callback answer failed: {e}");
        }
    }
    Ok(())
}

/// Resolve the chat a raw group-call update belongs to.
async fn raw_chat_id(raw: &tl::enums::Update, bridge: &Bridge) -> Option<i64> {
    match raw {
        tl::enums::Update::GroupCall(u) => match &u.peer {
            Some(peer) => {
                let peer_id = PeerId::from(peer.clone());
                peer_id.bot_api_dialog_id()
            }
            None => None,
        },
        tl::enums::Update::GroupCallParticipants(u) => {
            let call_id = match &u.call {
                tl::enums::InputGroupCall::Call(c) => c.id,
                _ => return None,
            };
            bridge.chat_id_for_call(call_id).await
        }
        tl::enums::Update::GroupCallChainBlocks(u) => {
            let call_id = match &u.call {
                tl::enums::InputGroupCall::Call(c) => c.id,
                _ => return None,
            };
            bridge.chat_id_for_call(call_id).await
        }
        _ => None,
    }
}

fn chat_type_of(msg: &GramMessage) -> ChatType {
    if let Some(peer) = msg.peer() {
        return match peer {
            grammers_client::peer::Peer::User(_) => ChatType::Private,
            grammers_client::peer::Peer::Group(_) => ChatType::Group,
            grammers_client::peer::Peer::Channel(_) => {
                if msg.post() {
                    ChatType::Channel
                } else {
                    ChatType::Supergroup
                }
            }
            grammers_client::peer::Peer::Community(_) => ChatType::Channel,
        };
    }
    match msg.peer_id().kind() {
        PeerKind::User => ChatType::Private,
        PeerKind::Chat => ChatType::Group,
        PeerKind::Channel => ChatType::Supergroup,
    }
}
