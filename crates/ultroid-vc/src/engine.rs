//! Voice-call engine — port of `vc/_pytgcalls.py` (`Call`, 805 LOC).
//!
//! The engine owns the ntgcalls instance and drives the play lifecycle:
//! `play_media` (create offer → set stream sources → bridge join →
//! connect), `play_next` on stream end, pause/resume/mute/unmute/volume,
//! `played_time` via `NTgCalls::time` (ms → `ms_to_seconds`), and P2P
//! calls (`create_p2p_call` / `init_exchange` / `exchange_keys` /
//! `connect_p2p`). Per-chat assistant assignment and per-chat play locks
//! mirror the Python `Call` class; the queue itself lives in
//! `player.rs` / `cacher.rs`.
//!
//! Join flow (mirrors `connect_call` + `play_media`):
//! `create_call` → `set_stream_sources(Capture, desc)` → bridge
//! `join_group_call(offer)` → transport JSON → `connect(transport)`.
//! When `get_connection_mode` reports `Stream`, the connection is
//! recorded as pending (py-tgcalls stores it for the later callback
//! instead of connecting immediately); the `on_connection_change`
//! callback drains pending connections when the state becomes
//! `Connected`.
//!
//! Live-join disclaimer: this path is implemented against the verified
//! ntgcalls + grammers APIs, but no live join has ever been performed
//! from the sandbox (no account / group call exists there). See README.

use crate::bridge::{Bridge, BridgeEvent, CallParticipant};
use crate::cacher::ChatCacher;
use crate::dataclass::CachedTrack;
use crate::database::VcDatabase;
use crate::media::{AudioQuality, MediaStream};
use crate::player::PlayerRegistry;
use crate::tools::ms_to_seconds;
use grammers_client::session::types::PeerRef;
use grammers_client::tl;
use ntgcalls::{
    AuthParams, ConnectionMode, ConnectionState, DhConfig, MediaDescription, NTgCalls,
    StreamMode,
};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use ultroid_core::error::{Error, Result};

/// Events arriving from ntgcalls callbacks.
#[derive(Debug)]
enum EngineEvent {
    StreamEnded { chat_id: i64 },
    ConnectionChanged { chat_id: i64, state: ConnectionState },
}

/// Per-chat call statistics.
#[derive(Debug, Clone)]
pub struct CallStats {
    pub chat_id: i64,
    pub connection_mode: String,
    pub muted: bool,
    pub video_stopped: bool,
    pub played_seconds: u64,
    pub is_playing: bool,
}

/// The DH offer for a 1-1 call: the raw g_a bytes plus the issued
/// `phone.RequestCall` result (which carries the call id).
#[derive(Debug)]
pub struct P2pOffer {
    pub g_a: Vec<u8>,
    pub call: tl::enums::phone::PhoneCall,
}

pub struct Engine {
    bridge: Arc<Bridge>,
    calls: NTgCalls,
    db: Arc<VcDatabase>,
    players: Mutex<PlayerRegistry>,
    cacher: Mutex<ChatCacher>,
    /// Chats whose ntgcalls connection is pending (mode == Stream).
    pending: Mutex<HashMap<i64, String>>,
    /// Per-chat play locks (mirrors the Python per-chat lock map).
    locks: Mutex<HashMap<i64, Arc<Mutex<()>>>>,
    events_rx: Mutex<mpsc::UnboundedReceiver<EngineEvent>>,
}

impl Engine {
    pub fn new(bridge: Arc<Bridge>, db: Arc<VcDatabase>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Register the ntgcalls callbacks while we still own the
        // instance mutably (before the Arc wrap); the callbacks are
        // synchronous and only forward into the channel.
        let mut calls = NTgCalls::new();
        let stream_tx = tx.clone();
        calls.on_stream_end(move |chat_id, _ty, _dev| {
            let _ = stream_tx.send(EngineEvent::StreamEnded { chat_id });
        });
        let conn_tx = tx.clone();
        calls.on_connection_change(move |chat_id, info| {
            let _ = conn_tx.send(EngineEvent::ConnectionChanged {
                chat_id,
                state: info.state,
            });
        });
        Arc::new(Engine {
            bridge,
            calls,
            db,
            players: Mutex::new(PlayerRegistry::new()),
            cacher: Mutex::new(ChatCacher::new()),
            pending: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            events_rx: Mutex::new(rx),
        })
    }

    async fn chat_lock(&self, chat_id: i64) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(chat_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// The engine event loop: bridge events (participants / call closed /
    /// chain blocks / signalling) + ntgcalls events. Spawn it once.
    pub async fn run(self: Arc<Self>) {
        let mut bridge_rx = self.bridge.subscribe();
        loop {
            tokio::select! {
                ev = async { self.events_rx.lock().await.recv().await } => {
                    let Some(ev) = ev else { break };
                    if let Err(e) = self.handle_engine_event(ev).await {
                        tracing::warn!("vc engine event failed: {e}");
                    }
                }
                recv = bridge_rx.recv() => {
                    match recv {
                        Ok(ev) => {
                            if let Err(e) = self.handle_bridge_event(ev).await {
                                tracing::warn!("vc bridge event failed: {e}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
    }

    async fn handle_engine_event(&self, ev: EngineEvent) -> Result<()> {
        match ev {
            EngineEvent::StreamEnded { chat_id } => {
                // `play_next` on StreamEnded (mirrors the Python handler).
                self.play_next(chat_id).await
            }
            EngineEvent::ConnectionChanged { chat_id, state } => {
                if state == ConnectionState::Connected {
                    if let Some(transport) = self.pending.lock().await.remove(&chat_id) {
                        self.calls
                            .connect(chat_id, &transport, false)
                            .await
                            .map_err(|e| Error::Other(format!("ntgcalls connect: {e}")))?;
                    }
                } else if matches!(state, ConnectionState::Failed | ConnectionState::Timeout | ConnectionState::Closed)
                {
                    self.pending.lock().await.remove(&chat_id);
                    self.players.lock().await.player(chat_id).stop();
                }
                Ok(())
            }
        }
    }

    async fn handle_bridge_event(&self, ev: BridgeEvent) -> Result<()> {
        match ev {
            BridgeEvent::ChainBlocks {
                chat_id,
                sub_chain_id,
                blocks,
                next_offset,
            } => {
                // Conference chain blocks feed ntgcalls (mirrors the
                // Python `on_update_group_call_chain_blocks` path).
                self.calls
                    .apply_blocks(chat_id, sub_chain_id, next_offset, &blocks, false)
                    .await
                    .map_err(|e| Error::Other(format!("ntgcalls apply_blocks: {e}")))?;
                Ok(())
            }
            BridgeEvent::CallClosed { chat_id }
            | BridgeEvent::Kicked { chat_id }
            | BridgeEvent::LeftGroup { chat_id } => self.end(chat_id).await,
            BridgeEvent::SignalingData { user_id, data } => {
                self.calls
                    .send_signaling_data(user_id, &data)
                    .await
                    .map_err(|e| Error::Other(format!("ntgcalls send_signaling_data: {e}")))?;
                Ok(())
            }
            BridgeEvent::ParticipantsChanged { .. } => Ok(()),
        }
    }

    // -- playback -----------------------------------------------------------

    /// Play a media stream in a chat (mirrors `Call.play_media`).
    /// `peer_ref` is the PeerRef of the triggering message (for peer
    /// resolution); pass None to fall back to dialog scan / resolve.
    pub async fn play_media(
        &self,
        chat_id: i64,
        mut stream: MediaStream,
        peer_ref: Option<PeerRef>,
    ) -> Result<()> {
        let lock = self.chat_lock(chat_id).await;
        let _guard = lock.lock().await;
        stream.check_stream().await?;
        let desc = stream.to_media_description();
        self.join_with_description(chat_id, &desc, false, peer_ref)
            .await
    }

    /// The join itself (mirrors `connect_call`): offer → sources →
    /// bridge join → transport → ntgcalls connect, with the STREAM-mode
    /// pending rule.
    async fn join_with_description(
        &self,
        chat_id: i64,
        desc: &MediaDescription,
        video_stopped: bool,
        peer_ref: Option<PeerRef>,
    ) -> Result<()> {
        let offer = self
            .calls
            .create_call(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls create_call: {e}")))?;
        self.calls
            .set_stream_sources(chat_id, StreamMode::Capture, desc)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls set_stream_sources: {e}")))?;
        let transport = self
            .bridge
            .join_group_call(chat_id, &offer, video_stopped, None, None, peer_ref, None, None)
            .await?;
        match self
            .calls
            .get_connection_mode(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls get_connection_mode: {e}")))?
        {
            ConnectionMode::Stream => {
                // Store the pending connection (py-tgcalls behaviour).
                self.pending.lock().await.insert(chat_id, transport);
                Ok(())
            }
            _ => self
                .calls
                .connect(chat_id, &transport, false)
                .await
                .map_err(|e| Error::Other(format!("ntgcalls connect: {e}"))),
        }
    }

    /// Play the next queued track in a chat (mirrors `play_next`).
    pub async fn play_next(&self, chat_id: i64) -> Result<()> {
        let next = {
            let mut players = self.players.lock().await;
            players.player(chat_id).start_next()
        };
        let Some(track) = next else {
            // Queue exhausted — leave the call (mirrors Python: end when
            // the queue empties and looping is off).
            return self.end(chat_id).await;
        };
        let input = track
            .file_path
            .clone()
            .unwrap_or_else(|| track.url.clone());
        let stream = MediaStream::audio(input, AudioQuality::High);
        self.play_media(chat_id, stream, None).await
    }

    /// Enqueue a track; if nothing is playing, start it immediately.
    /// Returns true when playback started on this call.
    pub async fn enqueue(&self, chat_id: i64, track: CachedTrack) -> Result<bool> {
        let started;
        {
            let mut players = self.players.lock().await;
            let p = players.player(chat_id);
            let was_idle = !p.is_playing() && p.current.is_none();
            p.enqueue(track.clone());
            started = was_idle;
        }
        self.cacher.lock().await.push(chat_id, track);
        if started {
            self.play_next(chat_id).await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn pause(&self, chat_id: i64) -> Result<bool> {
        let paused = self
            .calls
            .pause(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls pause: {e}")))?;
        if paused {
            self.players.lock().await.player(chat_id).pause();
        }
        Ok(paused)
    }

    pub async fn resume(&self, chat_id: i64) -> Result<bool> {
        let resumed = self
            .calls
            .resume(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls resume: {e}")))?;
        if resumed {
            self.players.lock().await.player(chat_id).resume();
        }
        Ok(resumed)
    }

    pub async fn mute(&self, chat_id: i64) -> Result<bool> {
        self.calls
            .mute(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls mute: {e}")))
    }

    pub async fn unmute(&self, chat_id: i64) -> Result<bool> {
        self.calls
            .unmute(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls unmute: {e}")))
    }

    /// Change a participant's volume (1-200; the bridge multiplies by 100).
    /// The participant peer is resolved through the bridge so the real
    /// access_hash is used — never a zero hash.
    pub async fn change_volume(&self, chat_id: i64, user_id: i64, volume: i32) -> Result<()> {
        let peer = self.bridge.resolve_input_peer(user_id, None).await?;
        self.bridge
            .edit_participant(chat_id, peer, None, Some(volume.clamp(1, 200)), None)
            .await
    }

    /// Seconds played in the current track (mirrors `played_time`).
    pub async fn played_time(&self, chat_id: i64) -> Result<u64> {
        let ms = self
            .calls
            .time(chat_id, StreamMode::Capture)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls time: {e}")))?;
        Ok(ms_to_seconds(ms))
    }

    pub async fn stats_call(&self, chat_id: i64) -> Result<CallStats> {
        let mode = self
            .calls
            .get_connection_mode(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls get_connection_mode: {e}")))?;
        let state = self
            .calls
            .get_state(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls get_state: {e}")))?;
        let played = self.played_time(chat_id).await.unwrap_or(0);
        let is_playing = self
            .players
            .lock()
            .await
            .get(chat_id)
            .map(|p| p.is_playing())
            .unwrap_or(false);
        Ok(CallStats {
            chat_id,
            connection_mode: format!("{mode:?}"),
            muted: state.muted,
            video_stopped: state.video_stopped,
            played_seconds: played,
            is_playing,
        })
    }

    pub async fn vc_users(&self, chat_id: i64) -> Result<Vec<CallParticipant>> {
        self.bridge.get_group_participants(chat_id).await
    }

    /// End playback in a chat: stop ntgcalls, leave the group call,
    /// clear player + cache (mirrors `Call.end` / KICKED handling).
    pub async fn end(&self, chat_id: i64) -> Result<()> {
        let _ = self.calls.stop(chat_id).await;
        self.pending.lock().await.remove(&chat_id);
        self.players.lock().await.player(chat_id).stop();
        self.cacher.lock().await.clear(chat_id);
        self.bridge.leave_group_call(chat_id).await?;
        Ok(())
    }

    /// Per-chat assistant assignment (mirrors `db.get/set_assistant`).
    pub async fn assistant_for(&self, chat_id: i64) -> Option<i64> {
        self.db.get_assistant(chat_id).await
    }

    pub async fn set_assistant(&self, chat_id: i64, assistant_id: Option<i64>) {
        self.db.set_assistant(chat_id, assistant_id).await;
    }

    // -- P2P (1-1 calls) ------------------------------------------------------

    /// Start a 1-1 call: create the ntgcalls side, run the DH exchange,
    /// and issue `phone.RequestCall`. The g_a hash is `sha1(g_a)[-8:]`
    /// (Telegram's DH convention). The key fingerprint is only known
    /// after the other side answers — see `accept_p2p`.
    pub async fn start_p2p_call(
        &self,
        user_id: i64,
        user_peer: tl::enums::InputUser,
        video: bool,
    ) -> Result<P2pOffer> {
        self.calls
            .create_p2p_call(user_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls create_p2p_call: {e}")))?;
        let dh = self.bridge.get_dh_config().await?;
        let dh_config = DhConfig {
            g: dh.g,
            p: dh.p,
            random: dh.random,
        };
        let g_a = self
            .calls
            .init_exchange(user_id, &dh_config, None)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls init_exchange: {e}")))?;
        let g_a_hash = {
            let mut h = Sha1::new();
            h.update(&g_a);
            let digest = h.finalize();
            digest[digest.len() - 8..].to_vec()
        };
        let call = self
            .bridge
            .request_call(user_peer, g_a_hash, video)
            .await?;
        Ok(P2pOffer { g_a, call })
    }

    /// Complete the exchange when the other side answers: `AcceptCall`
    /// with g_b, exchange keys (returns the real key fingerprint), then
    /// connect over the RTC servers from the accepted call.
    pub async fn accept_p2p(
        &self,
        user_id: i64,
        peer: tl::enums::InputPhoneCall,
        g_b: Vec<u8>,
        servers: Vec<ntgcalls::RTCServer>,
    ) -> Result<AuthParams> {
        let _call = self.bridge.accept_call(peer, g_b.clone()).await?;
        let auth = self
            .calls
            .exchange_keys(user_id, &g_b, 0)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls exchange_keys: {e}")))?;
        self.calls
            .connect_p2p(user_id, &servers, &[], true, None)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls connect_p2p: {e}")))?;
        Ok(auth)
    }

    /// Join a conference call (mirrors py-tgcalls conference join):
    /// ntgcalls gives payload/public_key/block, which go into
    /// `phone.JoinGroupCall` via the bridge, then the transport connects.
    pub async fn join_conference(
        &self,
        chat_id: i64,
        user_id: i64,
        peer_ref: Option<PeerRef>,
    ) -> Result<()> {
        let params = self
            .calls
            .init_conference(chat_id, user_id, None)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls init_conference: {e}")))?;
        let public_key: [u8; 32] = params
            .public_key
            .try_into()
            .map_err(|_| Error::Other("conference public key is not 32 bytes".into()))?;
        let transport = self
            .bridge
            .join_group_call(
                chat_id,
                &params.payload,
                false,
                None,
                None,
                peer_ref,
                Some(public_key),
                Some(params.block),
            )
            .await?;
        self.calls
            .connect(chat_id, &transport, false)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls connect: {e}")))
    }

    /// Present (screen share) in an active call.
    pub async fn start_presentation(&self, chat_id: i64) -> Result<()> {
        let payload = self
            .calls
            .init_presentation(chat_id)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls init_presentation: {e}")))?;
        let transport = self.bridge.join_presentation(chat_id, &payload).await?;
        self.calls
            .connect(chat_id, &transport, true)
            .await
            .map_err(|e| Error::Other(format!("ntgcalls connect presentation: {e}")))
    }

    pub async fn stop_presentation(&self, chat_id: i64) -> Result<()> {
        self.bridge.leave_presentation(chat_id).await
    }
}
