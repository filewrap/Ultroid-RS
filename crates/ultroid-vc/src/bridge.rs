//! MTProto bridge for voice calls — the Rust reimplementation of
//! py-tgcalls' Telethon bridge (`pytgcalls/mtproto/telethon_client.py`,
//! 1,055 lines) against grammers.
//!
//! The bridge is the *signalling* half of voice chat (py-tgcalls' job
//! split): it resolves peers/calls, runs the `phone.JoinGroupCall`
//! handshake, and fans raw updates out. The *audio* half is the
//! `ntgcalls` crate (see `engine.rs`). This module is new code written
//! against the grammers 0.12 / TL layer 233 API — not a copy of the
//! Python bridge.
//!
//! Join handshake (mirrors `connect_call.py` + `TelethonClient.join_group_call`):
//! 1. `get_input_call`: resolve peer -> `channels.GetFullChannel` /
//!    `messages.GetFullChat` -> `InputGroupCall`; warm the participant
//!    cache with `phone.GetGroupCall(limit=-1)`. Scheduled calls return None.
//! 2. `phone.JoinGroupCall { params: DataJSON(offer) }` -> scan the
//!    returned `Updates` for `UpdateGroupCallConnection.params.data`
//!    (the transport JSON for ntgcalls).
//! 3. DC handling (mirrors `_invoke`): group-call traffic lives on the
//!    call's DC (`GroupCall.stream_dc_id`); invoke there via
//!    `invoke_in_dc`, and on a 303 migrate error re-resolve the DC and
//!    retry once. grammers does NOT auto-handle FILE_MIGRATE (verified:
//! no MIGRATE handling in grammers-mtsender 0.12), so the bridge does it.
//!
//! Voice joins on the USER client (VC session / primary), never the bot
//! client — bots cannot join group calls as participants.

use grammers_client::session::types::{PeerAuth, PeerId, PeerRef};
use grammers_client::{Client, InvocationError, tl};
use grammers_mtsender::RpcError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use ultroid_core::error::{Error, Result};

/// A resolved group call, cached per chat.
#[derive(Debug, Clone)]
pub struct CachedCall {
    pub input: tl::types::InputGroupCall,
    /// DC hosting the call's stream (`GroupCall.stream_dc_id`).
    pub stream_dc: Option<i32>,
    pub default_join_as: Option<tl::enums::Peer>,
}

/// A group-call participant (parsed from `GroupCallParticipant`).
#[derive(Debug, Clone)]
pub struct CallParticipant {
    pub peer_id: i64,
    pub muted: bool,
    pub volume: i32,
    pub video_stopped: bool,
    pub raise_hand: bool,
    pub can_self_unmute: bool,
    pub just_joined: bool,
    pub left: bool,
}

/// Events fanned out from raw updates (mirrors §2.4 of PYTGALLS-RESEARCH.md).
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    ParticipantsChanged {
        chat_id: i64,
        participants: Vec<CallParticipant>,
    },
    CallClosed {
        chat_id: i64,
    },
    ChainBlocks {
        chat_id: i64,
        sub_chain_id: i32,
        blocks: Vec<Vec<u8>>,
        next_offset: i32,
    },
    SignalingData {
        user_id: i64,
        data: Vec<u8>,
    },
    Kicked {
        chat_id: i64,
    },
    LeftGroup {
        chat_id: i64,
    },
}

pub struct Bridge {
    client: Client,
    calls: Mutex<HashMap<i64, CachedCall>>,
    participants: Mutex<HashMap<i64, Vec<CallParticipant>>>,
    events: broadcast::Sender<BridgeEvent>,
}

impl Bridge {
    pub fn new(client: Client) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Bridge {
            client,
            calls: Mutex::new(HashMap::new()),
            participants: Mutex::new(HashMap::new()),
            events,
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BridgeEvent> {
        self.events.subscribe()
    }

    // -- invoke with DC-migration retry -----------------------------------

    /// Invoke a raw TL request, on the call DC when known; on a 303
    /// migrate error, re-resolve the DC from the error and retry once
    /// (mirrors `TelethonClient._invoke` / `BridgedClient.extract_dc`).
    pub async fn invoke_dc<R: tl::RemoteCall>(
        &self,
        request: &R,
        dc: Option<i32>,
    ) -> std::result::Result<R::Return, InvocationError> {
        let first = match dc {
            Some(dc_id) => self.client.invoke_in_dc(dc_id, request).await,
            None => self.client.invoke(request).await,
        };
        match first {
            Ok(v) => Ok(v),
            Err(err) => {
                let migrate_dc = match &err {
                    InvocationError::Rpc(RpcError { code: 303, value: Some(dc), .. }) => {
                        Some(*dc as i32)
                    }
                    InvocationError::Rpc(RpcError { code: 303, name, .. }) => {
                        // grammers strips the trailing _N into `value`, but
                        // parse the name too, defensively.
                        name.rsplit('_')
                            .next()
                            .and_then(|s| s.parse::<i32>().ok())
                    }
                    _ => None,
                };
                match migrate_dc {
                    Some(new_dc) if Some(new_dc) != dc => {
                        self.client.invoke_in_dc(new_dc, request).await
                    }
                    _ => Err(err),
                }
            }
        }
    }

    // -- peer resolution ---------------------------------------------------

    /// Resolve a chat id to an `InputPeer`, mirroring
    /// `TelethonClient.get_input_entity`: prefer the PeerRef from the
    /// triggering message, else scan dialogs, else `resolve_peer` to warm
    /// the session cache and convert the resolved peer.
    pub async fn resolve_input_peer(
        &self,
        chat_id: i64,
        peer_ref: Option<PeerRef>,
    ) -> Result<tl::enums::InputPeer> {
        if let Some(pr) = peer_ref {
            return Ok(pr.into());
        }
        let peer_id = PeerId::from_bot_api_dialog_id(chat_id)
            .ok_or_else(|| Error::Other(format!("invalid chat id {chat_id}")))?;
        // Dialog scan (grants the cached access_hash).
        let mut iter = self.client.iter_dialogs();
        while let Some(dialog) = iter
            .next()
            .await
            .map_err(|e| Error::Telegram(e.to_string()))?
        {
            if dialog.peer_ref().id == peer_id {
                let pr = dialog.peer_ref();
                return Ok(pr.into());
            }
        }
        // Last resort: resolve (fetches + caches the peer), then convert.
        let peer = self
            .client
            .resolve_peer(PeerRef {
                id: peer_id,
                auth: PeerAuth::default(),
            })
            .await
            .map_err(|e| Error::Telegram(format!("resolve peer {chat_id}: {e}")))?;
        let pr = peer
            .to_ref()
            .await
            .map_err(|e| Error::Telegram(format!("peer ref for {chat_id}: {e}")))?
            .ok_or_else(|| Error::Other(format!("peer {chat_id} not in session cache")))?;
        Ok(pr.into())
    }

    // -- call resolution ---------------------------------------------------

    /// Resolve the active `InputGroupCall` for a chat (None when there is
    /// no call, or it is scheduled). Mirrors `TelethonClient.get_call`.
    pub async fn get_input_call(
        &self,
        chat_id: i64,
        peer_ref: Option<PeerRef>,
    ) -> Result<Option<CachedCall>> {
        if let Some(cached) = self.calls.lock().await.get(&chat_id) {
            return Ok(Some(cached.clone()));
        }
        let input_peer = self.resolve_input_peer(chat_id, peer_ref).await?;
        let (call, default_join_as) = match &input_peer {
            tl::enums::InputPeer::Channel(ch) => {
                let res = self
                    .invoke_dc(
                        &tl::functions::channels::GetFullChannel {
                            channel: tl::enums::InputChannel::Channel(tl::types::InputChannel {
                                channel_id: ch.channel_id,
                                access_hash: ch.access_hash,
                            }),
                        },
                        None,
                    )
                    .await
                    .map_err(|e| Error::Telegram(format!("GetFullChannel: {e}")))?;
                let tl::enums::messages::ChatFull::Full(r) = res;
                match r.full_chat {
                    tl::enums::ChatFull::ChannelFull(full) => {
                        (full.call, full.groupcall_default_join_as)
                    }
                    _ => (None, None),
                }
            }
            tl::enums::InputPeer::Chat(ch) => {
                let res = self
                    .invoke_dc(
                        &tl::functions::messages::GetFullChat {
                            chat_id: ch.chat_id,
                        },
                        None,
                    )
                    .await
                    .map_err(|e| Error::Telegram(format!("GetFullChat: {e}")))?;
                let tl::enums::messages::ChatFull::Full(r) = res;
                match r.full_chat {
                    tl::enums::ChatFull::Full(full) => {
                        (full.call, full.groupcall_default_join_as)
                    }
                    _ => (None, None),
                }
            }
            _ => {
                return Err(Error::Other(format!(
                    "chat {chat_id} is not a group/channel"
                )))
            }
        };
        let Some(input) = call else {
            return Ok(None);
        };
        let input_call = match &input {
            tl::enums::InputGroupCall::Call(c) => c.clone(),
            _ => return Ok(None),
        };
        // Warm the participant cache, like py-tgcalls does.
        let group = self
            .invoke_dc(
                &tl::functions::phone::GetGroupCall {
                    call: tl::enums::InputGroupCall::Call(input_call.clone()),
                    limit: -1,
                },
                None,
            )
            .await
            .map_err(|e| Error::Telegram(format!("GetGroupCall: {e}")))?;
        let tl::enums::phone::GroupCall::Call(group) = group;
        let call = match group.call {
            tl::enums::GroupCall::Call(c) => c,
            _ => return Ok(None),
        };
        if call.schedule_date.is_some() {
            return Ok(None);
        }
        let mut parts = Vec::new();
        for p in &group.participants {
            if let Some(cp) = parse_participant(p) {
                parts.push(cp);
            }
        }
        self.participants.lock().await.insert(chat_id, parts);
        let cached = CachedCall {
            input: input_call,
            stream_dc: call.stream_dc_id,
            default_join_as,
        };
        self.calls.lock().await.insert(chat_id, cached.clone());
        Ok(Some(cached))
    }

    pub async fn drop_call_cache(&self, chat_id: i64) {
        self.calls.lock().await.remove(&chat_id);
        self.participants.lock().await.remove(&chat_id);
    }

    // -- join ---------------------------------------------------------------

    /// Join a group call with an ntgcalls offer (JSON payload).
    /// Returns the transport JSON for `NTgCalls::connect`.
    /// Mirrors `TelethonClient.join_group_call` exactly, including the
    /// `'{"transport": null}'` sentinel when no connection update arrives,
    /// and the one retry after re-resolving on GroupcallForbidden/Invalid.
    /// `public_key`/`block` are the conference-join params (None for a
    /// normal group-call join).
    pub async fn join_group_call(
        &self,
        chat_id: i64,
        payload: &str,
        video_stopped: bool,
        join_as: Option<tl::enums::InputPeer>,
        invite_hash: Option<String>,
        peer_ref: Option<PeerRef>,
        public_key: Option<[u8; 32]>,
        block: Option<Vec<u8>>,
    ) -> Result<String> {
        self.join_group_call_inner(chat_id, payload, video_stopped, join_as, invite_hash, peer_ref, public_key, block, false)
            .await
    }

    async fn join_group_call_inner(
        &self,
        chat_id: i64,
        payload: &str,
        video_stopped: bool,
        join_as: Option<tl::enums::InputPeer>,
        invite_hash: Option<String>,
        peer_ref: Option<PeerRef>,
        public_key: Option<[u8; 32]>,
        block: Option<Vec<u8>>,
        retried: bool,
    ) -> Result<String> {
        let cached = self
            .get_input_call(chat_id, peer_ref)
            .await?
            .ok_or_else(|| Error::Other("no active group call in this chat".to_string()))?;
        let dc = cached.stream_dc;
        let result = self
            .invoke_dc(
                &tl::functions::phone::JoinGroupCall {
                    muted: false,
                    video_stopped,
                    call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                    join_as: join_as.clone().unwrap_or(tl::enums::InputPeer::PeerSelf),
                    invite_hash: invite_hash.clone(),
                    public_key,
                    block: block.clone(),
                    params: tl::enums::DataJson::Json(tl::types::DataJson {
                        data: payload.to_string(),
                    }),
                },
                dc,
            )
            .await;
        let updates = match result {
            Ok(u) => u,
            Err(InvocationError::Rpc(ref e))
                if !retried && (e.is("GROUPCALL_FORBIDDEN") || e.is("GROUPCALL_INVALID")) =>
            {
                self.drop_call_cache(chat_id).await;
                return Box::pin(self.join_group_call_inner(
                    chat_id,
                    payload,
                    video_stopped,
                    join_as,
                    invite_hash,
                    peer_ref,
                    public_key,
                    block,
                    true,
                ))
                .await;
            }
            Err(e) => return Err(Error::Telegram(format!("JoinGroupCall: {e}"))),
        };
        // Scan the returned Updates for the connection params.
        let mut transport: Option<String> = None;
        let mut new_input: Option<tl::types::InputGroupCall> = None;
        for update in updates_list(&updates) {
            match update {
                tl::enums::Update::GroupCallConnection(conn) => {
                    let tl::enums::DataJson::Json(d) = conn.params;
                    transport = Some(d.data);
                }
                tl::enums::Update::GroupCall(u) => {
                    if let tl::enums::GroupCall::Call(c) = u.call {
                        new_input = Some(tl::types::InputGroupCall {
                            id: c.id,
                            access_hash: c.access_hash,
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(input) = new_input {
            self.calls.lock().await.insert(
                chat_id,
                CachedCall {
                    input,
                    stream_dc: cached.stream_dc,
                    default_join_as: cached.default_join_as,
                },
            );
        }
        Ok(transport.unwrap_or_else(|| "{\"transport\": null}".to_string()))
    }

    // -- lifecycle ------------------------------------------------------------

    pub async fn create_group_call(
        &self,
        chat_id: i64,
        title: Option<String>,
        peer_ref: Option<PeerRef>,
    ) -> Result<()> {
        let peer = self.resolve_input_peer(chat_id, peer_ref).await?;
        self.invoke_dc(
            &tl::functions::phone::CreateGroupCall {
                rtmp_stream: false,
                peer,
                random_id: rand::random::<i32>(),
                title,
                schedule_date: None,
            },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("CreateGroupCall: {e}")))?;
        self.drop_call_cache(chat_id).await;
        Ok(())
    }

    pub async fn leave_group_call(&self, chat_id: i64) -> Result<()> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(());
        };
        self.invoke_dc(
            &tl::functions::phone::LeaveGroupCall {
                call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                source: 0,
            },
            cached.stream_dc,
        )
        .await
        .map_err(|e| Error::Telegram(format!("LeaveGroupCall: {e}")))?;
        self.drop_call_cache(chat_id).await;
        Ok(())
    }

    pub async fn discard_group_call(&self, chat_id: i64) -> Result<()> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(());
        };
        self.invoke_dc(
            &tl::functions::phone::DiscardGroupCall {
                call: tl::enums::InputGroupCall::Call(cached.input.clone()),
            },
            cached.stream_dc,
        )
        .await
        .map_err(|e| Error::Telegram(format!("DiscardGroupCall: {e}")))?;
        self.drop_call_cache(chat_id).await;
        Ok(())
    }

    pub async fn get_group_participants(
        &self,
        chat_id: i64,
    ) -> Result<Vec<CallParticipant>> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut offset = String::new();
        loop {
            let res = self
                .invoke_dc(
                    &tl::functions::phone::GetGroupParticipants {
                        call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                        ids: Vec::new(),
                        sources: Vec::new(),
                        offset: offset.clone(),
                        limit: 100,
                    },
                    cached.stream_dc,
                )
                .await
                .map_err(|e| Error::Telegram(format!("GetGroupParticipants: {e}")))?;
            let tl::enums::phone::GroupParticipants::Participants(res) = res;
            for p in &res.participants {
                if let Some(cp) = parse_participant(p) {
                    out.push(cp);
                }
            }
            if res.next_offset.is_empty() {
                break;
            }
            offset = res.next_offset;
        }
        self.participants.lock().await.insert(chat_id, out.clone());
        Ok(out)
    }

    /// Current stream timestamp in ms (`channels[0].last_timestamp_ms`),
    /// mirroring `get_stream_timestamp`.
    pub async fn get_stream_timestamp_ms(&self, chat_id: i64) -> Result<u64> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(0);
        };
        let res = self
            .invoke_dc(
                &tl::functions::phone::GetGroupCallStreamChannels {
                    call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                },
                cached.stream_dc,
            )
            .await
            .map_err(|e| Error::Telegram(format!("GetGroupCallStreamChannels: {e}")))?;
        let tl::enums::phone::GroupCallStreamChannels::Channels(res) = res;
        Ok(res
            .channels
            .first()
            .map(|c| {
                let tl::enums::GroupCallStreamChannel::Channel(ch) = c;
                ch.last_timestamp_ms as u64
            })
            .unwrap_or(0))
    }

    pub async fn get_subchain_blocks(
        &self,
        chat_id: i64,
        sub_chain_id: i32,
        offset: i32,
        limit: i32,
    ) -> Result<Option<(Vec<Vec<u8>>, i32)>> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(None);
        };
        let res = self
            .invoke_dc(
                &tl::functions::phone::GetGroupCallChainBlocks {
                    call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                    sub_chain_id,
                    offset,
                    limit,
                },
                cached.stream_dc,
            )
            .await
            .map_err(|e| Error::Telegram(format!("GetGroupCallChainBlocks: {e}")))?;
        for update in updates_list(&res) {
            if let tl::enums::Update::GroupCallChainBlocks(b) = update {
                return Ok(Some((b.blocks, b.next_offset)));
            }
        }
        Ok(None)
    }

    /// Volume / status edit. Volume is 1-200 in Ultroid; the wire takes
    /// volume*100 (py-tgcalls `change_volume`).
    pub async fn edit_participant(
        &self,
        chat_id: i64,
        participant: tl::enums::InputPeer,
        muted: Option<bool>,
        volume: Option<i32>,
        video_stopped: Option<bool>,
    ) -> Result<()> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Err(Error::Other("no active group call".into()));
        };
        self.invoke_dc(
            &tl::functions::phone::EditGroupCallParticipant {
                call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                participant,
                muted,
                volume: volume.map(|v| v * 100),
                raise_hand: None,
                video_stopped,
                video_paused: None,
                presentation_paused: None,
            },
            cached.stream_dc,
        )
        .await
        .map_err(|e| Error::Telegram(format!("EditGroupCallParticipant: {e}")))?;
        Ok(())
    }

    pub async fn join_presentation(&self, chat_id: i64, payload: &str) -> Result<String> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Err(Error::Other("no active group call".into()));
        };
        let res = self
            .invoke_dc(
                &tl::functions::phone::JoinGroupCallPresentation {
                    call: tl::enums::InputGroupCall::Call(cached.input.clone()),
                    params: tl::enums::DataJson::Json(tl::types::DataJson {
                        data: payload.to_string(),
                    }),
                },
                cached.stream_dc,
            )
            .await
            .map_err(|e| Error::Telegram(format!("JoinGroupCallPresentation: {e}")))?;
        for update in updates_list(&res) {
            if let tl::enums::Update::GroupCallConnection(conn) = update {
                let tl::enums::DataJson::Json(d) = conn.params;
                return Ok(d.data);
            }
        }
        Ok("{\"transport\": null}".to_string())
    }

    pub async fn leave_presentation(&self, chat_id: i64) -> Result<()> {
        let Some(cached) = self.get_input_call(chat_id, None).await? else {
            return Ok(());
        };
        self.invoke_dc(
            &tl::functions::phone::LeaveGroupCallPresentation {
                call: tl::enums::InputGroupCall::Call(cached.input.clone()),
            },
            cached.stream_dc,
        )
        .await
        .map_err(|e| Error::Telegram(format!("LeaveGroupCallPresentation: {e}")))?;
        Ok(())
    }

    // -- P2P -----------------------------------------------------------------

    pub async fn get_dh_config(&self) -> Result<tl::types::messages::DhConfig> {
        let res = self
            .invoke_dc(
                &tl::functions::messages::GetDhConfig {
                    version: 0,
                    random_length: 256,
                },
                None,
            )
            .await
            .map_err(|e| Error::Telegram(format!("GetDhConfig: {e}")))?;
        match res {
            tl::enums::messages::DhConfig::Config(d) => Ok(d),
            _ => Err(Error::Telegram("GetDhConfig returned an unexpected type".into())),
        }
    }

    fn protocol(&self) -> tl::enums::PhoneCallProtocol {
        // ntgcalls' own protocol (min/max layer, UDP flags, library
        // versions) — the same values py-tgcalls' binding reports.
        // `get_protocol` is an associated function, no instance needed.
        let p = ntgcalls::NTgCalls::get_protocol().unwrap_or(ntgcalls::Protocol {
            min_layer: 65,
            max_layer: 92,
            udp_p2p: true,
            udp_reflector: true,
            library_versions: vec!["ntgcalls 3.0.7".to_string()],
        });
        tl::enums::PhoneCallProtocol::Protocol(tl::types::PhoneCallProtocol {
            udp_p2p: p.udp_p2p,
            udp_reflector: p.udp_reflector,
            min_layer: p.min_layer,
            max_layer: p.max_layer,
            library_versions: p.library_versions,
        })
    }

    pub async fn request_call(
        &self,
        user: tl::enums::InputUser,
        g_a_hash: Vec<u8>,
        video: bool,
    ) -> Result<tl::enums::phone::PhoneCall> {
        self.invoke_dc(
            &tl::functions::phone::RequestCall {
                video,
                user_id: user,
                random_id: rand::random::<i32>(),
                g_a_hash,
                protocol: self.protocol(),
            },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("RequestCall: {e}")))
    }

    pub async fn accept_call(
        &self,
        peer: tl::enums::InputPhoneCall,
        g_b: Vec<u8>,
    ) -> Result<tl::enums::phone::PhoneCall> {
        self.invoke_dc(
            &tl::functions::phone::AcceptCall {
                peer,
                g_b,
                protocol: self.protocol(),
            },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("AcceptCall: {e}")))
    }

    pub async fn confirm_call(
        &self,
        peer: tl::enums::InputPhoneCall,
        g_a: Vec<u8>,
        key_fingerprint: i64,
    ) -> Result<tl::enums::phone::PhoneCall> {
        self.invoke_dc(
            &tl::functions::phone::ConfirmCall {
                peer,
                g_a,
                key_fingerprint,
                protocol: self.protocol(),
            },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("ConfirmCall: {e}")))
    }

    pub async fn discard_call(
        &self,
        peer: tl::enums::InputPhoneCall,
        duration: i32,
        reason: tl::enums::PhoneCallDiscardReason,
    ) -> Result<tl::enums::Updates> {
        self.invoke_dc(
            &tl::functions::phone::DiscardCall {
                video: false,
                peer,
                duration,
                reason,
                connection_id: 0,
            },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("DiscardCall: {e}")))
    }

    pub async fn send_signaling_data(
        &self,
        peer: tl::enums::InputPhoneCall,
        data: Vec<u8>,
    ) -> Result<bool> {
        self.invoke_dc(
            &tl::functions::phone::SendSignalingData { peer, data },
            None,
        )
        .await
        .map_err(|e| Error::Telegram(format!("SendSignalingData: {e}")))
    }

    // -- raw update fan-out ---------------------------------------------------

    /// Feed one raw `tl::enums::Update` (from `Update::Raw`) into the
    /// bridge, mirroring the Telethon `@app.on(Raw())` handler (§2.4).
    pub async fn handle_raw_update(&self, update: &tl::enums::Update, chat_id: i64) {
        match update {
            tl::enums::Update::GroupCallParticipants(u) => {
                let mut parts = Vec::new();
                for p in &u.participants {
                    if let Some(cp) = parse_participant(p) {
                        parts.push(cp);
                    }
                }
                self.participants.lock().await.insert(chat_id, parts.clone());
                let _ = self.events.send(BridgeEvent::ParticipantsChanged {
                    chat_id,
                    participants: parts,
                });
            }
            tl::enums::Update::GroupCall(u) => match &u.call {
                tl::enums::GroupCall::Call(c) => {
                    self.calls.lock().await.insert(
                        chat_id,
                        CachedCall {
                            input: tl::types::InputGroupCall {
                                id: c.id,
                                access_hash: c.access_hash,
                            },
                            stream_dc: c.stream_dc_id,
                            default_join_as: None,
                        },
                    );
                }
                tl::enums::GroupCall::Discarded(_) => {
                    self.drop_call_cache(chat_id).await;
                    let _ = self.events.send(BridgeEvent::CallClosed { chat_id });
                }
            },
            tl::enums::Update::GroupCallChainBlocks(b) => {
                let _ = self.events.send(BridgeEvent::ChainBlocks {
                    chat_id,
                    sub_chain_id: b.sub_chain_id,
                    blocks: b.blocks.clone(),
                    next_offset: b.next_offset,
                });
            }
            tl::enums::Update::PhoneCallSignalingData(s) => {
                let _ = self.events.send(BridgeEvent::SignalingData {
                    user_id: s.phone_call_id,
                    data: s.data.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Flatten any `Updates`-shaped return into its update list.
fn updates_list(updates: &tl::enums::Updates) -> Vec<tl::enums::Update> {
    match updates {
        tl::enums::Updates::Updates(u) => u.updates.clone(),
        tl::enums::Updates::Combined(u) => u.updates.clone(),
        tl::enums::Updates::UpdateShort(u) => vec![u.update.clone()],
        _ => Vec::new(),
    }
}

fn parse_participant(p: &tl::enums::GroupCallParticipant) -> Option<CallParticipant> {
    let tl::enums::GroupCallParticipant::Participant(p) = p;
    let peer_id = match &p.peer {
        tl::enums::Peer::User(u) => u.user_id,
        tl::enums::Peer::Chat(_) => return None,
        tl::enums::Peer::Channel(c) => c.channel_id,
    };
    // Layer 233 has no `video_stopped` / `raise_hand` bools:
    // video presence is `video_joined`, a raised hand is a set
    // `raise_hand_rating`; volume is optional (default 100).
    Some(CallParticipant {
        peer_id,
        muted: p.muted,
        volume: p.volume.unwrap_or(100),
        video_stopped: !p.video_joined,
        raise_hand: p.raise_hand_rating.is_some(),
        can_self_unmute: p.can_self_unmute,
        just_joined: p.just_joined,
        left: p.left,
    })
}
