//! Extended `Responder` operations over grammers raw invokes.
//!
//! The `Responder` trait in `ultroid-core` is transport-free; this file
//! is the real implementation for a grammers `Client`. Every method was
//! verified against grammers-client 0.12.0 source and the generated
//! layer-233 TL code before being written (`grammers-tl-types` build
//! output under the scratch `target-tlcheck` dir):
//!
//! * message fetch — `Client::get_messages_by_id` (channels use
//!   `channels.GetMessages`, others `messages.GetMessages` internally);
//!   media download — `Client::download_media` over `Media: Downloadable`.
//! * peer resolution — `Client::resolve_peer` (fresh `users.GetUsers` /
//!   `messages.GetChats` / `channels.GetChannels` per kind) with a
//!   dialogs-scan fallback; `Peer::to_ref()` gives the authorised ref.
//! * sends — high-level `Client::send_message` where the `InputMessage`
//!   builder covers the call (text/html/file/markup/poll/geo/scheduled);
//!   raw `messages.SendMessage` only for the message-effect send, which
//!   the builder cannot express (`effect: Option<i64>`).
//! * inline click — raw `messages.GetInlineBotResults` +
//!   `messages.SendInlineBotResult` with a real `InputReplyTo::Message`,
//!   because grammers' `InlineResult::send` hardcodes `reply_to: None`.
//! * admin — `channels.EditBanned` / `EditAdmin` /
//!   `messages.EditChatDefaultBannedRights` with the TL rights structs
//!   mapped field-for-field from the core plain types.
//! * everything else — the raw function named in each method's doc comment.
//!
//! Dialog ids throughout are bot API dialog ids
//! (`PeerId::bot_api_dialog_id`). Errors keep the real server reason via
//! `Error::Telegram`; a missing message/peer is `Error::Other` naming it.

use super::responder::{GrammersResponder, telegram};
use async_trait::async_trait;
use grammers_client::{
    Client,
    message::{InputMessage, Message},
    peer::{Peer, User},
    session::types::{PeerAuth, PeerId, PeerKind, PeerRef},
    tl,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use ultroid_core::{
    AdminRights, BannedRights, BoostStatus, ChatKind, ChatSummary, ClientKind, DialogInfo, Error,
    MessageSummary, ParticipantInfo, Responder, Result, StickerItem, StickerSetInfo, ThemeInfo,
    UserSummary, WallpaperInfo, rich::RichDoc, tg::InlineButton,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn random_id() -> i64 {
    // grammers' generate_random_id is pub(crate); rand 0.8 is the
    // workspace's random source (rand::random is stable in edition 2024).
    rand::random::<i64>()
}

fn other(msg: impl Into<String>) -> Error {
    Error::Other(msg.into())
}

/// A grammers `User` → the plain summary plugins see.
fn user_summary(user: &User) -> UserSummary {
    let premium = match &user.raw {
        tl::enums::User::User(u) => u.premium,
        _ => false,
    };
    UserSummary {
        id: user.id().bot_api_dialog_id_unchecked(),
        first_name: user.first_name().unwrap_or("").to_string(),
        last_name: user.last_name().map(str::to_string),
        username: user.username().map(str::to_string),
        premium,
        bot: user.is_bot(),
    }
}

/// A raw TL `User` enum → summary (for `users.GetFullUser` results).
fn user_summary_raw(user: &tl::enums::User) -> UserSummary {
    match user {
        tl::enums::User::User(u) => UserSummary {
            // A user id IS its bot API dialog id (positive).
            id: u.id,
            first_name: u.first_name.clone().unwrap_or_default(),
            last_name: u.last_name.clone(),
            username: u.username.clone(),
            premium: u.premium,
            bot: u.bot,
        },
        tl::enums::User::Empty(_) => UserSummary {
            id: 0,
            first_name: String::new(),
            last_name: None,
            username: None,
            premium: false,
            bot: false,
        },
    }
}

fn tl_banned(r: BannedRights) -> tl::types::ChatBannedRights {
    tl::types::ChatBannedRights {
        until_date: r.until_date.unwrap_or(0),
        view_messages: r.view_messages,
        send_messages: r.send_messages,
        send_media: r.send_media,
        send_stickers: r.send_stickers,
        send_gifs: r.send_gifs,
        send_games: r.send_games,
        send_inline: r.send_inline,
        embed_links: r.embed_links,
        send_polls: r.send_polls,
        change_info: r.change_info,
        invite_users: r.invite_users,
        pin_messages: r.pin_messages,
        manage_topics: r.manage_topics,
        send_photos: r.send_photos,
        send_videos: r.send_videos,
        send_roundvideos: r.send_roundvideos,
        send_audios: r.send_audios,
        send_voices: r.send_voices,
        send_docs: r.send_docs,
        send_plain: r.send_plain,
        edit_rank: r.edit_rank,
        send_reactions: r.send_reactions,
        manage_linked_peers: r.manage_linked_peers,
    }
}

fn tl_admin(r: AdminRights) -> tl::types::ChatAdminRights {
    tl::types::ChatAdminRights {
        change_info: r.change_info,
        post_messages: r.post_messages,
        edit_messages: r.edit_messages,
        delete_messages: r.delete_messages,
        ban_users: r.ban_users,
        invite_users: r.invite_users,
        pin_messages: r.pin_messages,
        add_admins: r.add_admins,
        anonymous: r.anonymous,
        manage_call: r.manage_call,
        other: r.other,
        manage_topics: r.manage_topics,
        post_stories: r.post_stories,
        edit_stories: r.edit_stories,
        delete_stories: r.delete_stories,
        manage_direct_messages: r.manage_direct_messages,
        manage_ranks: r.manage_ranks,
        manage_linked_peers: r.manage_linked_peers,
        manage_welcome_messages: r.manage_welcome_messages,
    }
}

/// Resolve a bot API dialog id to an authorised `PeerRef`.
///
/// `resolve_peer` fetches the peer fresh from the server (users /
/// chats / channels each have their own call); when that fails — the
/// peer is not reachable that way — fall back to scanning the dialog
/// list, which is how the VC bridge resolves peers too.
async fn resolve_ref(client: &Client, dialog_id: i64) -> Result<PeerRef> {
    let pid = PeerId::from_bot_api_dialog_id(dialog_id)
        .ok_or_else(|| other(format!("cannot resolve dialog id {dialog_id}")))?;
    let probe = PeerRef {
        id: pid,
        auth: PeerAuth::default(),
    };
    match client.resolve_peer(probe).await {
        Ok(peer) => match peer.to_ref().await {
            Ok(Some(r)) => Ok(r),
            _ => Ok(probe),
        },
        Err(_) => {
            let mut dialogs = client.iter_dialogs();
            while let Ok(Some(d)) = dialogs.next().await {
                if d.peer_id().bot_api_dialog_id() == Some(dialog_id) {
                    return Ok(d.peer_ref());
                }
            }
            Err(other(format!(
                "peer {dialog_id} not found (not in dialogs)"
            )))
        }
    }
}

/// Fetch one message by id (or fail if it was deleted / never existed).
async fn fetch_message(client: &Client, chat_id: i64, msg_id: i32) -> Result<Message> {
    let peer = resolve_ref(client, chat_id).await?;
    let mut msgs = client
        .get_messages_by_id(peer, &[msg_id])
        .await
        .map_err(telegram)?;
    match msgs.pop().flatten() {
        Some(m) => Ok(m),
        None => Err(other(format!(
            "message {msg_id} in chat {chat_id} not found"
        ))),
    }
}

/// The id of the message a `messages.*` send produced, from its `Updates`
/// response (the sent message arrives as `UpdateNewMessage` /
/// `UpdateNewChannelMessage`).
fn updates_message_id(updates: tl::enums::Updates) -> Option<i64> {
    let list = match updates {
        tl::enums::Updates::Updates(u) => u.updates,
        tl::enums::Updates::Combined(u) => u.updates,
        _ => return None,
    };
    for u in list {
        let msg = match u {
            tl::enums::Update::NewMessage(m) => Some(m.message),
            tl::enums::Update::NewChannelMessage(m) => Some(m.message),
            _ => None,
        };
        if let Some(m) = msg {
            if let tl::enums::Message::Message(m) = m {
                return Some(m.id as i64);
            }
        }
    }
    None
}

/// `InputDocument` for an existing document (from a fetched message's
/// media), for sticker-set / story re-use.
fn input_document_of(msg: &Message) -> Result<tl::enums::InputDocument> {
    match msg.media() {
        Some(grammers_client::media::Media::Document(doc)) => {
            let d = &doc.raw.document;
            match d {
                Some(tl::enums::Document::Document(d)) => Ok(tl::enums::InputDocument::Document(
                    tl::types::InputDocument {
                        id: d.id,
                        access_hash: d.access_hash,
                        file_reference: d.file_reference.clone(),
                    },
                )),
                _ => Err(other("replied message has no document".to_string())),
            }
        }
        _ => Err(other("replied message has no document".to_string())),
    }
}

/// One sized variant of a profile photo, downloadable by its
/// `InputPhotoFileLocation`. grammers' own `PhotoSize::make_from` is
/// `pub(crate)`, so the binary builds the location itself and implements
/// the public `Downloadable` trait on this thin wrapper.
struct PhotoThumb {
    photo_id: i64,
    access_hash: i64,
    file_reference: Vec<u8>,
    thumb_size: String,
    size: Option<usize>,
    dc_id: i32,
}

impl grammers_client::media::Downloadable for PhotoThumb {
    fn to_raw_input_location(&self) -> Option<tl::enums::InputFileLocation> {
        Some(
            tl::types::InputPhotoFileLocation {
                id: self.photo_id,
                access_hash: self.access_hash,
                file_reference: self.file_reference.clone(),
                thumb_size: self.thumb_size.clone(),
            }
            .into(),
        )
    }

    fn size(&self) -> Option<usize> {
        self.size
    }

    fn dc_id(&self) -> Option<i32> {
        Some(self.dc_id)
    }
}

// ---------------------------------------------------------------------------
// the trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Responder for GrammersResponder {
    // -- basic reply / edit / send (the original Responder surface) --------

    async fn reply(&self, text: &str) -> Result<i64> {
        let msg = self
            .client()
            .send_message(self.peer(), InputMessage::new().text(text))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn reply_html(&self, html: &str) -> Result<i64> {
        let msg = self
            .client()
            .send_message(self.peer(), InputMessage::new().html(html))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn edit(&self, msg_id: i64, text: &str) -> Result<()> {
        self.client()
            .edit_message(self.peer(), msg_id as i32, InputMessage::new().text(text))
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn edit_html(&self, msg_id: i64, html: &str) -> Result<()> {
        self.client()
            .edit_message(self.peer(), msg_id as i32, InputMessage::new().html(html))
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn delete(&self, msg_id: i64) -> Result<()> {
        self.client()
            .delete_messages(self.peer(), &[msg_id as i32])
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn send_file(&self, path: &Path, caption: &str) -> Result<i64> {
        let uploaded = self.client().upload_file(path).await.map_err(telegram)?;
        let msg = self
            .client()
            .send_message(
                self.peer(),
                InputMessage::new().text(caption).file(uploaded),
            )
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_rich(&self, doc: &RichDoc) -> Result<i64> {
        match self.kind() {
            ClientKind::AsstBot => {
                let sender = ultroid_rich::RichSender::new_bot(self.client().clone());
                let sent = sender.send_rich(self.peer(), doc).await.map_err(|e| {
                    // A failed rich send must keep the real server reason
                    // (and never silently fall back to plain text here —
                    // the caller decides the fallback).
                    Error::Telegram(e.to_string())
                })?;
                Ok(sent.id() as i64)
            }
            _ => Err(Error::RichOnlyBot),
        }
    }

    fn chat_id(&self) -> i64 {
        self.chat_id_value()
    }

    // -- media / messages ---------------------------------------------------

    async fn download_media(&self, chat_id: i64, msg_id: i32, dest: &Path) -> Result<PathBuf> {
        let msg = fetch_message(self.client(), chat_id, msg_id).await?;
        let media = msg
            .media()
            .ok_or_else(|| other(format!("message {msg_id} has no media to download")))?;
        self.client()
            .download_media(&media, dest)
            .await
            .map_err(telegram)?;
        Ok(dest.to_path_buf())
    }

    async fn message_summary(&self, chat_id: i64, msg_id: i32) -> Result<Option<MessageSummary>> {
        let peer = resolve_ref(self.client(), chat_id).await?;
        let mut msgs = self
            .client()
            .get_messages_by_id(peer, &[msg_id])
            .await
            .map_err(telegram)?;
        Ok(msgs.pop().flatten().map(|m| MessageSummary {
            id: m.id(),
            sender_id: m.sender_id().and_then(|p| p.bot_api_dialog_id()),
            text: m.text().to_string(),
            has_media: m.media().is_some(),
        }))
    }

    // -- users / chats ------------------------------------------------------

    async fn resolve_user(&self, query: &str) -> Result<UserSummary> {
        let q = query.trim().trim_start_matches('@');
        if !q.is_empty() && q.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(id) = q.parse::<i64>() {
                if let Some(pid) = PeerId::from_bot_api_dialog_id(id) {
                    if pid.kind() == PeerKind::User {
                        return self.user_full(id).await;
                    }
                }
            }
        }
        let peer = self
            .client()
            .resolve_username(q)
            .await
            .map_err(telegram)?
            .ok_or_else(|| other(format!("username @{q} is not occupied")))?;
        match peer {
            Peer::User(u) => Ok(user_summary(&u)),
            _ => Err(other(format!("@{q} is not a user"))),
        }
    }

    async fn user_full(&self, dialog_id: i64) -> Result<UserSummary> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let input_user: tl::enums::InputUser = (&peer).into();
        let res = self
            .client()
            .invoke(&tl::functions::users::GetFullUser { id: input_user })
            .await
            .map_err(telegram)?;
        match res {
            tl::enums::users::UserFull::Full(f) => f
                .users
                .iter()
                .find(|u| matches!(u, tl::enums::User::User(u) if u.id == dialog_id))
                .map(user_summary_raw)
                .ok_or_else(|| other(format!("user {dialog_id} not in GetFullUser result"))),
        }
    }

    async fn chat_info(&self, dialog_id: i64) -> Result<ChatSummary> {
        let pid = PeerId::from_bot_api_dialog_id(dialog_id)
            .ok_or_else(|| other(format!("cannot resolve dialog id {dialog_id}")))?;
        match pid.kind() {
            PeerKind::User => {
                let u = self.user_full(dialog_id).await?;
                let title = format!(
                    "{}{}",
                    u.first_name,
                    u.last_name
                        .as_ref()
                        .map(|l| format!(" {l}"))
                        .unwrap_or_default()
                );
                Ok(ChatSummary {
                    id: dialog_id,
                    title,
                    username: u.username,
                    kind: ChatKind::User,
                })
            }
            PeerKind::Chat => {
                let res = self
                    .client()
                    .invoke(&tl::functions::messages::GetFullChat {
                        chat_id: -dialog_id,
                    })
                    .await
                    .map_err(telegram)?;
                chat_summary_from_full(res, dialog_id, ChatKind::Group)
            }
            PeerKind::Channel => {
                let peer = resolve_ref(self.client(), dialog_id).await?;
                let channel: tl::enums::InputChannel = (&peer).into();
                let res = self
                    .client()
                    .invoke(&tl::functions::channels::GetFullChannel { channel })
                    .await
                    .map_err(telegram)?;
                // A channel full chat is a megagroup unless the channel is
                // a broadcast channel; the kind distinction needs the
                // Channel object, which `chat_summary_from_full` reads.
                chat_summary_from_full(res, dialog_id, ChatKind::Supergroup)
            }
        }
    }

    async fn me(&self) -> Result<UserSummary> {
        let me = self.client().get_me().await.map_err(telegram)?;
        Ok(user_summary(&me))
    }

    async fn dialogs(&self) -> Result<Vec<DialogInfo>> {
        let mut out = Vec::new();
        let mut iter = self.client().iter_dialogs();
        while let Ok(Some(d)) = iter.next().await {
            let id = d.peer_id().bot_api_dialog_id().unwrap_or(0);
            let title = match d.peer() {
                Peer::User(u) => u.full_name(),
                Peer::Group(g) => g.title().unwrap_or("").to_string(),
                Peer::Channel(c) => c.title().to_string(),
                Peer::Community(c) => c.title().to_string(),
            };
            let (unread, pinned, archived) = match &d.raw {
                tl::enums::Dialog::Dialog(dd) => {
                    (dd.unread_count, dd.pinned, dd.folder_id == Some(1))
                }
                tl::enums::Dialog::Folder(dd) => (
                    dd.unread_muted_messages_count + dd.unread_unmuted_messages_count,
                    dd.pinned,
                    true,
                ),
                // Communities report no unread counters.
                tl::enums::Dialog::Community(dd) => (0, dd.pinned, false),
            };
            out.push(DialogInfo {
                id,
                title,
                unread_count: unread,
                pinned,
                archived,
            });
        }
        Ok(out)
    }

    // -- sends ---------------------------------------------------------------

    async fn send_to(&self, dialog_id: i64, text: &str) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let msg = self
            .client()
            .send_message(peer, InputMessage::new().text(text))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_html_to(&self, dialog_id: i64, html: &str) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let msg = self
            .client()
            .send_message(peer, InputMessage::new().html(html))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_file_to(&self, dialog_id: i64, path: &Path, caption: &str) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let uploaded = self.client().upload_file(path).await.map_err(telegram)?;
        let msg = self
            .client()
            .send_message(peer, InputMessage::new().text(caption).file(uploaded))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_markup_to(
        &self,
        dialog_id: i64,
        text: &str,
        buttons: Vec<Vec<InlineButton>>,
    ) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let msg = self
            .client()
            .send_message(
                peer,
                InputMessage::new()
                    .text(text)
                    .reply_markup(ultroid_rich::to_reply_markup(&buttons)),
            )
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_poll(
        &self,
        dialog_id: i64,
        question: &str,
        options: Vec<String>,
        multiple: bool,
        quiz: bool,
    ) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let answers: Vec<tl::enums::PollAnswer> = options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                tl::enums::PollAnswer::Answer(tl::types::PollAnswer {
                    text: tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
                        text: o.clone(),
                        entities: vec![],
                    }),
                    option: vec![b'0' + (i as u8)],
                    added_by: None,
                    date: None,
                    media: None,
                })
            })
            .collect();
        let poll = tl::enums::Poll::Poll(tl::types::Poll {
            id: random_id(),
            closed: false,
            public_voters: false,
            multiple_choice: multiple,
            quiz,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: false,
            subscribers_only: false,
            question: tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
                text: question.to_string(),
                entities: vec![],
            }),
            answers,
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        });
        let media = tl::enums::InputMedia::Poll(Box::new(tl::types::InputMediaPoll {
            poll,
            correct_answers: None,
            attached_media: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        }));
        let msg = self
            .client()
            .send_message(peer, InputMessage::new().media(media))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_scheduled(&self, dialog_id: i64, text: &str, delay_secs: u64) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let when = SystemTime::now() + Duration::from_secs(delay_secs);
        let msg = self
            .client()
            .send_message(
                peer,
                InputMessage::new().text(text).schedule_date(Some(when)),
            )
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn send_effect(&self, dialog_id: i64, text: &str, effect_id: i64) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let updates = self
            .client()
            .invoke(&tl::functions::messages::SendMessage {
                no_webpage: true,
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: peer.into(),
                reply_to: None,
                message: text.to_string(),
                random_id: random_id(),
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: Some(effect_id),
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: None,
            })
            .await
            .map_err(telegram)?;
        updates_message_id(updates)
            .ok_or_else(|| other("send with effect: server returned no message id"))
    }

    async fn send_geo_live(
        &self,
        dialog_id: i64,
        latitude: f64,
        longitude: f64,
        period_secs: i32,
    ) -> Result<i64> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let media = tl::enums::InputMedia::GeoLive(tl::types::InputMediaGeoLive {
            stopped: false,
            geo_point: tl::enums::InputGeoPoint::Point(tl::types::InputGeoPoint {
                lat: latitude,
                long: longitude,
                accuracy_radius: None,
            }),
            heading: None,
            period: Some(period_secs),
            proximity_notification_radius: None,
        });
        let msg = self
            .client()
            .send_message(peer, InputMessage::new().media(media))
            .await
            .map_err(telegram)?;
        Ok(msg.id() as i64)
    }

    async fn forward_to(&self, from_dialog_id: i64, msg_id: i32, to_dialog_id: i64) -> Result<i64> {
        let from = resolve_ref(self.client(), from_dialog_id).await?;
        let to = resolve_ref(self.client(), to_dialog_id).await?;
        let updates = self
            .client()
            .invoke(&tl::functions::messages::ForwardMessages {
                silent: false,
                background: false,
                with_my_score: false,
                drop_author: false,
                drop_media_captions: false,
                noforwards: false,
                allow_paid_floodskip: false,
                from_ephemeral: false,
                from_peer: from.into(),
                id: vec![msg_id],
                random_id: vec![random_id()],
                to_peer: to.into(),
                top_msg_id: None,
                reply_to: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                video_timestamp: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(telegram)?;
        updates_message_id(updates).ok_or_else(|| other("forward: server returned no message id"))
    }

    async fn delete_from(&self, dialog_id: i64, msg_ids: &[i32]) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .delete_messages(peer, msg_ids)
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn pin_message(&self, dialog_id: i64, msg_id: i32, silent: bool) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::messages::UpdatePinnedMessage {
                silent,
                unpin: false,
                pm_oneside: false,
                peer: peer.into(),
                id: msg_id,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn unpin_message(&self, dialog_id: i64, msg_id: i32) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::messages::UpdatePinnedMessage {
                silent: true,
                unpin: true,
                pm_oneside: false,
                peer: peer.into(),
                id: msg_id,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn unpin_all(&self, dialog_id: i64) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::messages::UnpinAllMessages {
                peer: peer.into(),
                top_msg_id: None,
                saved_peer_id: None,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    // -- admin ----------------------------------------------------------------

    async fn ban_user(&self, chat_id: i64, user_id: i64, rights: BannedRights) -> Result<()> {
        let peer = resolve_ref(self.client(), chat_id).await?;
        let channel: tl::enums::InputChannel = (&peer).into();
        let target = resolve_ref(self.client(), user_id).await?;
        self.client()
            .invoke(&tl::functions::channels::EditBanned {
                channel,
                participant: target.into(),
                banned_rights: tl::enums::ChatBannedRights::Rights(tl_banned(rights)),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn unban_user(&self, chat_id: i64, user_id: i64) -> Result<()> {
        self.ban_user(chat_id, user_id, BannedRights::default())
            .await
    }

    async fn promote_user(
        &self,
        chat_id: i64,
        user_id: i64,
        rights: AdminRights,
        rank: &str,
    ) -> Result<()> {
        let peer = resolve_ref(self.client(), chat_id).await?;
        let channel: tl::enums::InputChannel = (&peer).into();
        let target = resolve_ref(self.client(), user_id).await?;
        let user: tl::enums::InputUser = (&target).into();
        self.client()
            .invoke(&tl::functions::channels::EditAdmin {
                channel,
                user_id: user,
                admin_rights: tl::enums::ChatAdminRights::Rights(tl_admin(rights)),
                rank: if rank.is_empty() {
                    None
                } else {
                    Some(rank.to_string())
                },
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn edit_default_rights(&self, chat_id: i64, rights: BannedRights) -> Result<()> {
        let peer = resolve_ref(self.client(), chat_id).await?;
        self.client()
            .invoke(&tl::functions::messages::EditChatDefaultBannedRights {
                peer: peer.into(),
                banned_rights: tl::enums::ChatBannedRights::Rights(tl_banned(rights)),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn participants(
        &self,
        chat_id: i64,
        limit: i32,
        offset: i32,
    ) -> Result<(Vec<ParticipantInfo>, i32)> {
        let peer = resolve_ref(self.client(), chat_id).await?;
        let channel: tl::enums::InputChannel = (&peer).into();
        let res = self
            .client()
            .invoke(&tl::functions::channels::GetParticipants {
                channel,
                filter: tl::enums::ChannelParticipantsFilter::ChannelParticipantsRecent,
                offset,
                limit,
                hash: 0,
            })
            .await
            .map_err(telegram)?;
        let p = match res {
            tl::enums::channels::ChannelParticipants::Participants(p) => p,
            _ => return Err(other("GetParticipants returned an unexpected shape")),
        };
        let users: std::collections::HashMap<i64, UserSummary> = p
            .users
            .iter()
            .map(|u| {
                let s = user_summary_raw(u);
                (s.id, s)
            })
            .collect();
        let mut items = Vec::new();
        for part in &p.participants {
            let (user_id, admin, owner) = match part {
                tl::enums::ChannelParticipant::Participant(cp) => (cp.user_id, false, false),
                tl::enums::ChannelParticipant::ParticipantSelf(cp) => (cp.user_id, false, false),
                tl::enums::ChannelParticipant::Creator(cp) => (cp.user_id, true, true),
                tl::enums::ChannelParticipant::Admin(cp) => (cp.user_id, true, false),
                tl::enums::ChannelParticipant::Banned(cp) => (
                    PeerId::from(cp.peer.clone())
                        .bot_api_dialog_id()
                        .unwrap_or(0),
                    false,
                    false,
                ),
                tl::enums::ChannelParticipant::Left(cp) => (
                    PeerId::from(cp.peer.clone())
                        .bot_api_dialog_id()
                        .unwrap_or(0),
                    false,
                    false,
                ),
            };
            if let Some(user) = users.get(&user_id) {
                items.push(ParticipantInfo {
                    user: user.clone(),
                    admin,
                    owner,
                });
            }
        }
        Ok((items, p.count))
    }

    // -- folders / profile ----------------------------------------------------

    async fn set_archived(&self, dialog_id: i64, archived: bool) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::folders::EditPeerFolders {
                folder_peers: vec![tl::enums::InputFolderPeer::Peer(
                    tl::types::InputFolderPeer {
                        peer: peer.into(),
                        folder_id: if archived { 1 } else { 0 },
                    },
                )],
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn update_profile(
        &self,
        first_name: Option<&str>,
        last_name: Option<&str>,
        about: Option<&str>,
    ) -> Result<()> {
        self.client()
            .invoke(&tl::functions::account::UpdateProfile {
                first_name: first_name.map(str::to_string),
                last_name: last_name.map(str::to_string),
                about: about.map(str::to_string),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn upload_profile_photo(&self, path: &Path) -> Result<()> {
        let uploaded = self.client().upload_file(path).await.map_err(telegram)?;
        self.client()
            .invoke(&tl::functions::photos::UploadProfilePhoto {
                fallback: false,
                bot: None,
                file: Some(uploaded.raw),
                video: None,
                video_start_ts: None,
                video_emoji_markup: None,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn delete_profile_photos(&self, limit: i32) -> Result<i32> {
        let me = self.client().get_me().await.map_err(telegram)?;
        let input_user: tl::enums::InputUser = (&PeerRef {
            id: me.id(),
            auth: PeerAuth::default(),
        })
            .into();
        let mut deleted = 0i32;
        let mut offset = 0i32;
        // Page through the photo list; `limit <= 0` means "all".
        for _ in 0..100 {
            let remaining = if limit > 0 { limit - deleted } else { i32::MAX };
            if remaining <= 0 {
                break;
            }
            let page = self
                .client()
                .invoke(&tl::functions::photos::GetUserPhotos {
                    user_id: input_user.clone(),
                    offset,
                    max_id: 0,
                    limit: remaining.min(100),
                })
                .await
                .map_err(telegram)?;
            let photos = match page {
                tl::enums::photos::Photos::Photos(p) => p.photos,
                tl::enums::photos::Photos::Slice(p) => p.photos,
            };
            if photos.is_empty() {
                break;
            }
            let ids: Vec<tl::enums::InputPhoto> = photos
                .iter()
                .filter_map(|p| match p {
                    tl::enums::Photo::Photo(ph) => {
                        Some(tl::enums::InputPhoto::Photo(tl::types::InputPhoto {
                            id: ph.id,
                            access_hash: ph.access_hash,
                            file_reference: ph.file_reference.clone(),
                        }))
                    }
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                break;
            }
            self.client()
                .invoke(&tl::functions::photos::DeletePhotos { id: ids })
                .await
                .map_err(telegram)?;
            deleted += photos.len() as i32;
            offset += photos.len() as i32;
            if (photos.len() as i32) < remaining.min(100) {
                break;
            }
        }
        Ok(deleted)
    }

    async fn download_profile_photos(
        &self,
        dialog_id: i64,
        dir: &Path,
        limit: i32,
    ) -> Result<Vec<PathBuf>> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let input_user: tl::enums::InputUser = (&peer).into();
        let page = self
            .client()
            .invoke(&tl::functions::photos::GetUserPhotos {
                user_id: input_user,
                offset: 0,
                max_id: 0,
                limit: limit.max(1).min(100),
            })
            .await
            .map_err(telegram)?;
        let photos = match page {
            tl::enums::photos::Photos::Photos(p) => p.photos,
            tl::enums::photos::Photos::Slice(p) => p.photos,
        };
        let mut out = Vec::new();
        for (i, photo) in photos.iter().enumerate() {
            let tl::enums::Photo::Photo(ph) = photo else {
                continue;
            };
            // Largest sized variant wins; the thumb is downloaded through
            // the local `PhotoThumb` wrapper (grammers' own `PhotoSize`
            // builder is `pub(crate)`, so the location is built here).
            let best = ph
                .sizes
                .iter()
                .filter_map(|s| match s {
                    tl::enums::PhotoSize::Size(sz) => Some((
                        (sz.w as i64) * (sz.h as i64),
                        sz.r#type.clone(),
                        sz.size as usize,
                    )),
                    tl::enums::PhotoSize::Progressive(sz) => Some((
                        (sz.w as i64) * (sz.h as i64),
                        sz.r#type.clone(),
                        sz.sizes.iter().map(|v| *v as usize).max().unwrap_or(0),
                    )),
                    _ => None,
                })
                .max_by_key(|(area, _, _)| *area);
            let Some((_, thumb_size, size)) = best else {
                continue;
            };
            let thumb = PhotoThumb {
                photo_id: ph.id,
                access_hash: ph.access_hash,
                file_reference: ph.file_reference.clone(),
                thumb_size,
                size: Some(size),
                dc_id: ph.dc_id,
            };
            let dest = dir.join(format!("profile-{}.jpg", i + 1));
            self.client()
                .download_media(&thumb, &dest)
                .await
                .map_err(telegram)?;
            out.push(dest);
        }
        Ok(out)
    }

    // -- inline ---------------------------------------------------------------

    async fn inline_query_click(
        &self,
        bot_dialog_id: i64,
        query: &str,
        chat_id: i64,
        reply_to: Option<i32>,
    ) -> Result<()> {
        // grammers' `InlineResult::send` cannot set `reply_to`, so the
        // query + click are raw invokes: `GetInlineBotResults` gives the
        // query id and the first result, `SendInlineBotResult` clicks it
        // into the chat (mirroring Telethon's `result.click(...)`).
        let bot_ref = resolve_ref(self.client(), bot_dialog_id).await?;
        let bot_user: tl::enums::InputUser = (&bot_ref).into();
        let target = resolve_ref(self.client(), chat_id).await?;
        let target_peer: tl::enums::InputPeer = target.into();
        let results = self
            .client()
            .invoke(&tl::functions::messages::GetInlineBotResults {
                bot: bot_user,
                peer: target_peer.clone(),
                geo_point: None,
                query: query.to_string(),
                offset: String::new(),
            })
            .await
            .map_err(telegram)?;
        let tl::enums::messages::BotResults::Results(r) = results;
        let first = r
            .results
            .first()
            .ok_or_else(|| other(format!("inline query '{query}' returned no results")))?;
        let id = match first {
            tl::enums::BotInlineResult::Result(x) => x.id.clone(),
            tl::enums::BotInlineResult::BotInlineMediaResult(x) => x.id.clone(),
        };
        self.client()
            .invoke(&tl::functions::messages::SendInlineBotResult {
                silent: false,
                background: false,
                clear_draft: false,
                hide_via: false,
                peer: target_peer,
                reply_to: reply_to.map(|msg_id| {
                    tl::enums::InputReplyTo::Message(tl::types::InputReplyToMessage {
                        reply_to_msg_id: msg_id,
                        top_msg_id: None,
                        reply_to_peer_id: None,
                        quote_text: None,
                        quote_entities: None,
                        quote_offset: None,
                        monoforum_peer_id: None,
                        todo_item_id: None,
                        poll_option: None,
                        quote_rich: None,
                    })
                }),
                random_id: random_id(),
                query_id: r.query_id,
                id,
                schedule_date: None,
                send_as: None,
                quick_reply_shortcut: None,
                allow_paid_stars: None,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    // -- boosts ----------------------------------------------------------------

    async fn boost_status(&self, dialog_id: i64) -> Result<BoostStatus> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        let res = self
            .client()
            .invoke(&tl::functions::premium::GetBoostsStatus { peer: peer.into() })
            .await
            .map_err(telegram)?;
        let tl::enums::premium::BoostsStatus::Status(b) = res;
        Ok(BoostStatus {
            level: b.level,
            boosts: b.boosts,
            gift_boosts: b.gift_boosts,
            next_level_boosts: b.next_level_boosts,
        })
    }

    async fn apply_boost(&self, dialog_id: i64) -> Result<()> {
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::premium::ApplyBoost {
                slots: None,
                peer: peer.into(),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    // -- stories ---------------------------------------------------------------

    async fn delete_stories(&self, story_ids: Vec<i32>) -> Result<()> {
        self.client()
            .invoke(&tl::functions::stories::DeleteStories {
                peer: tl::enums::InputPeer::PeerSelf,
                id: story_ids,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn set_story_stealth(&self, enabled: bool) -> Result<()> {
        // Telethon's `ToggleStealthModeRequest(enabled)` sets both flags.
        self.client()
            .invoke(&tl::functions::stories::ActivateStealthMode {
                past: enabled,
                future: enabled,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn post_story(&self, chat_id: i64, msg_id: i32, caption: &str) -> Result<()> {
        let msg = fetch_message(self.client(), chat_id, msg_id).await?;
        // Re-use the existing photo/document by reference instead of a
        // download + re-upload round-trip — the server result is the same.
        let media = match msg.media() {
            Some(grammers_client::media::Media::Photo(p)) => match &p.raw.photo {
                Some(tl::enums::Photo::Photo(ph)) => {
                    tl::enums::InputMedia::Photo(tl::types::InputMediaPhoto {
                        spoiler: false,
                        live_photo: false,
                        id: tl::enums::InputPhoto::Photo(tl::types::InputPhoto {
                            id: ph.id,
                            access_hash: ph.access_hash,
                            file_reference: ph.file_reference.clone(),
                        }),
                        ttl_seconds: None,
                        video: None,
                    })
                }
                _ => {
                    return Err(other(format!(
                        "message {msg_id} has no usable photo for a story"
                    )));
                }
            },
            Some(grammers_client::media::Media::Document(_)) => {
                let doc = input_document_of(&msg)?;
                tl::enums::InputMedia::Document(tl::types::InputMediaDocument {
                    spoiler: false,
                    id: doc,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                    query: None,
                })
            }
            _ => {
                return Err(other(format!(
                    "message {msg_id} has no photo/document to post as a story"
                )));
            }
        };
        self.client()
            .invoke(&tl::functions::stories::SendStory {
                pinned: false,
                noforwards: false,
                fwd_modified: false,
                peer: tl::enums::InputPeer::PeerSelf,
                media,
                media_areas: None,
                caption: if caption.is_empty() {
                    None
                } else {
                    Some(caption.to_string())
                },
                entities: None,
                // Public visibility, like Python's default story post.
                privacy_rules: vec![tl::enums::InputPrivacyRule::InputPrivacyValueAllowAll],
                random_id: random_id(),
                period: None,
                fwd_from_id: None,
                fwd_from_story: None,
                albums: None,
                music: None,
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    // -- stickers ---------------------------------------------------------------

    async fn my_sticker_sets(&self) -> Result<Vec<StickerSetInfo>> {
        let res = self
            .client()
            .invoke(&tl::functions::messages::GetMyStickers {
                offset_id: 0,
                limit: 100,
            })
            .await
            .map_err(telegram)?;
        let tl::enums::messages::MyStickers::Stickers(m) = res;
        let mut out = Vec::new();
        for covered in &m.sets {
            let set: &tl::enums::StickerSet = match covered {
                tl::enums::StickerSetCovered::Covered(c) => &c.set,
                tl::enums::StickerSetCovered::StickerSetMultiCovered(c) => &c.set,
                tl::enums::StickerSetCovered::StickerSetFullCovered(c) => &c.set,
                tl::enums::StickerSetCovered::StickerSetNoCovered(c) => &c.set,
            };
            let tl::enums::StickerSet::Set(s) = set;
            out.push(sticker_set_info(s, 0, Vec::new()));
        }
        Ok(out)
    }

    async fn sticker_set(&self, short_name: &str) -> Result<StickerSetInfo> {
        let res = self
            .client()
            .invoke(&tl::functions::messages::GetStickerSet {
                stickerset: tl::enums::InputStickerSet::ShortName(
                    tl::types::InputStickerSetShortName {
                        short_name: short_name.to_string(),
                    },
                ),
                hash: 0,
            })
            .await
            .map_err(telegram)?;
        let s = match res {
            tl::enums::messages::StickerSet::Set(s) => s,
            tl::enums::messages::StickerSet::NotModified => {
                return Err(other(format!("sticker set @{short_name} not modified")));
            }
        };
        let tl::enums::StickerSet::Set(set) = &s.set;
        let mut stickers = Vec::new();
        for pack in &s.packs {
            let tl::enums::StickerPack::Pack(pack) = pack;
            for doc_id in &pack.documents {
                if stickers.len() >= 100 {
                    break;
                }
                stickers.push(StickerItem {
                    emoji: pack.emoticon.clone(),
                    document_id: *doc_id,
                });
            }
        }
        Ok(sticker_set_info(set, s.documents.len() as i32, stickers))
    }

    async fn add_document_to_sticker_set(
        &self,
        chat_id: i64,
        msg_id: i32,
        short_name: &str,
        emoji: &str,
    ) -> Result<()> {
        let msg = fetch_message(self.client(), chat_id, msg_id).await?;
        let doc = input_document_of(&msg)?;
        self.client()
            .invoke(&tl::functions::stickers::AddStickerToSet {
                stickerset: tl::enums::InputStickerSet::ShortName(
                    tl::types::InputStickerSetShortName {
                        short_name: short_name.to_string(),
                    },
                ),
                sticker: tl::enums::InputStickerSetItem::Item(tl::types::InputStickerSetItem {
                    document: doc,
                    emoji: emoji.to_string(),
                    mask_coords: None,
                    keywords: None,
                }),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    // -- wallpapers / themes ----------------------------------------------------

    async fn list_wallpapers(&self) -> Result<Vec<WallpaperInfo>> {
        let res = self
            .client()
            .invoke(&tl::functions::account::GetWallPapers { hash: 0 })
            .await
            .map_err(telegram)?;
        let mut out = Vec::new();
        if let tl::enums::account::WallPapers::Papers(w) = res {
            for wp in &w.wallpapers {
                match wp {
                    tl::enums::WallPaper::Paper(t) => out.push(WallpaperInfo {
                        id: t.id,
                        // TL WallPaper has no title; the slug is the display name.
                        title: Some(t.slug.clone()),
                        dark: t.dark,
                        pattern: t.pattern,
                    }),
                    tl::enums::WallPaper::NoFile(t) => out.push(WallpaperInfo {
                        id: t.id,
                        title: None,
                        dark: false,
                        pattern: false,
                    }),
                }
            }
        }
        Ok(out)
    }

    async fn upload_wallpaper(&self, path: &Path) -> Result<()> {
        let uploaded = self.client().upload_file(path).await.map_err(telegram)?;
        let mime = match path.extension().and_then(|e| e.to_str()) {
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            _ => "image/jpeg",
        };
        let settings = || {
            tl::enums::WallPaperSettings::Settings(tl::types::WallPaperSettings {
                blur: false,
                motion: false,
                background_color: None,
                second_background_color: None,
                third_background_color: None,
                fourth_background_color: None,
                intensity: None,
                rotation: None,
                emoticon: None,
            })
        };
        let res = self
            .client()
            .invoke(&tl::functions::account::UploadWallPaper {
                for_chat: false,
                file: uploaded.raw,
                mime_type: mime.to_string(),
                settings: settings(),
            })
            .await
            .map_err(telegram)?;
        let (id, access_hash) = match res {
            tl::enums::WallPaper::Paper(t) => (t.id, t.access_hash),
            tl::enums::WallPaper::NoFile(t) => (t.id, 0),
        };
        self.client()
            .invoke(&tl::functions::account::SaveWallPaper {
                wallpaper: tl::enums::InputWallPaper::Paper(tl::types::InputWallPaper {
                    id,
                    access_hash,
                }),
                unsave: false,
                settings: settings(),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }

    async fn list_themes(&self) -> Result<Vec<ThemeInfo>> {
        let res = self
            .client()
            .invoke(&tl::functions::account::GetChatThemes { hash: 0 })
            .await
            .map_err(telegram)?;
        let mut out = Vec::new();
        if let tl::enums::account::Themes::Themes(t) = res {
            for theme in &t.themes {
                let tl::enums::Theme::Theme(theme) = theme;
                out.push(ThemeInfo {
                    id: theme.id,
                    access_hash: theme.access_hash,
                    slug: theme.slug.clone(),
                    title: theme.title.clone(),
                    emoticon: theme.emoticon.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn set_chat_theme(&self, dialog_id: i64, theme_slug: &str) -> Result<()> {
        // Layer 233's `messages.setChatTheme` takes `InputChatTheme`,
        // which identifies a theme by its emoticon — not by id. The
        // `Theme` objects from `GetChatThemes` carry that emoticon.
        let themes = self.list_themes().await?;
        let theme = themes
            .iter()
            .find(|t| t.slug == theme_slug)
            .ok_or_else(|| other(format!("chat theme '{theme_slug}' not found")))?;
        let emoticon = theme.emoticon.clone().ok_or_else(|| {
            other(format!(
                "chat theme '{theme_slug}' has no emoticon, so layer 233's SetChatTheme cannot apply it"
            ))
        })?;
        let peer = resolve_ref(self.client(), dialog_id).await?;
        self.client()
            .invoke(&tl::functions::messages::SetChatTheme {
                peer: peer.into(),
                theme: tl::enums::InputChatTheme::Theme(tl::types::InputChatTheme { emoticon }),
            })
            .await
            .map_err(telegram)?;
        Ok(())
    }
}

/// One `StickerSet` TL object (+ its sticker items) as a plain summary.
/// `count` is 0 for `GetMyStickers` entries — the server does not report
/// a count there; only `GetStickerSet` fills it.
fn sticker_set_info(
    set: &tl::types::StickerSet,
    count: i32,
    stickers: Vec<StickerItem>,
) -> StickerSetInfo {
    StickerSetInfo {
        id: set.id,
        access_hash: set.access_hash,
        title: set.title.clone(),
        short_name: set.short_name.clone(),
        count,
        emojis: set.emojis,
        stickers,
    }
}

/// Build a `ChatSummary` from a `messages.ChatFull` result: the title and
/// username live on the `Chat`/`Channel` object inside `chats`, matched
/// by id. `default_kind` is used when the object cannot be found.
fn chat_summary_from_full(
    res: tl::enums::messages::ChatFull,
    dialog_id: i64,
    default_kind: ChatKind,
) -> Result<ChatSummary> {
    // `messages.ChatFull` has a single variant; the chats list carries
    // both basic chats (`Chat::Chat`) and channels (`Chat::Channel`).
    let tl::enums::messages::ChatFull::Full(f) = res;
    for c in &f.chats {
        match c {
            tl::enums::Chat::Chat(ch)
                if PeerId::chat(ch.id).and_then(|p| p.bot_api_dialog_id()) == Some(dialog_id) =>
            {
                return Ok(ChatSummary {
                    id: dialog_id,
                    title: ch.title.clone(),
                    username: None,
                    kind: ChatKind::Group,
                });
            }
            tl::enums::Chat::Channel(ch)
                if PeerId::channel(ch.id).and_then(|p| p.bot_api_dialog_id())
                    == Some(dialog_id) =>
            {
                return Ok(ChatSummary {
                    id: dialog_id,
                    title: ch.title.clone(),
                    username: ch.username.clone(),
                    // Broadcast channels have `broadcast: true`; megagroups
                    // are the supergroup kind from the caller's default.
                    kind: if ch.broadcast {
                        ChatKind::Channel
                    } else {
                        default_kind
                    },
                });
            }
            _ => {}
        }
    }
    Err(other(format!(
        "chat {dialog_id} not in GetFullChat/GetFullChannel result"
    )))
}
