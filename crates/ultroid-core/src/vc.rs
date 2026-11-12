//! Voice-chat actor surface.
//!
//! The `Engine` (ntgcalls + `phone.*` signalling) lives in its own OS
//! thread and is not `Sync`, so plugins cannot hold it. Instead the
//! binary hands every `Ctx` a `VcControl` handle: an mpsc sender to
//! that thread. Commands are request/reply (oneshot) so the plugin
//! learns whether the join/play/pause actually happened.
//!
//! `VcTrack` is a plain-data mirror of `ultroid_vc::dataclass::CachedTrack`
//! (core may not depend on the vc crate); the binary converts it back.

use crate::{Error, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Where a track should be played: a group or a channel post context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcChannelPlay {
    pub chat_id: i64,
    pub is_channel: bool,
}

/// A track to enqueue, as plain data (mirrors `CachedTrack`).
#[derive(Debug, Clone)]
pub struct VcTrack {
    pub url: String,
    pub name: String,
    pub artist: String,
    pub loop_count: u32,
    pub user: String,
    pub file_path: Option<String>,
    pub thumbnail: Option<String>,
    pub track_id: String,
    pub duration: u64,
    pub is_video: bool,
    pub platform: String,
    pub channel: Option<VcChannelPlay>,
}

impl VcTrack {
    /// Build from the fields the plugins already carry. `channel`
    /// is `(chat_id, is_channel)` (`None` for a plain group play).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        url: impl Into<String>,
        name: impl Into<String>,
        artist: impl Into<String>,
        user: impl Into<String>,
        file_path: Option<String>,
        thumbnail: Option<String>,
        track_id: impl Into<String>,
        duration: u64,
        is_video: bool,
        platform: impl Into<String>,
        channel: Option<(i64, bool)>,
    ) -> Self {
        VcTrack {
            url: url.into(),
            name: name.into(),
            artist: artist.into(),
            loop_count: 0,
            user: user.into(),
            file_path,
            thumbnail,
            track_id: track_id.into(),
            duration,
            is_video,
            platform: platform.into(),
            channel: channel.map(|(chat_id, is_channel)| VcChannelPlay {
                chat_id,
                is_channel,
            }),
        }
    }
}

/// Actor handle to the voice engine thread. Implemented by the binary
/// as an mpsc sender; every method awaits the engine's reply, so a
/// dead engine surfaces as an error instead of silent nothing.
#[async_trait]
pub trait VcControl: Send + Sync {
    /// Enqueue a track; starts playback immediately when idle.
    /// Returns true when playback started on this call.
    async fn enqueue(&self, chat_id: i64, track: VcTrack) -> Result<bool>;
    /// Skip to the next queued track (ends the call when the queue
    /// is empty, mirroring Python `play_next`).
    async fn play_next(&self, chat_id: i64) -> Result<()>;
    async fn pause(&self, chat_id: i64) -> Result<bool>;
    async fn resume(&self, chat_id: i64) -> Result<bool>;
    async fn mute(&self, chat_id: i64) -> Result<bool>;
    async fn unmute(&self, chat_id: i64) -> Result<bool>;
    /// Stop ntgcalls, leave the group call, clear player + cache.
    async fn end(&self, chat_id: i64) -> Result<()>;
}

/// Convenience alias used on `Ctx` / `Shared`.
pub type VcHandle = Arc<dyn VcControl>;

/// Error used when the engine thread is gone (channel closed).
pub fn vc_engine_gone() -> Error {
    Error::Other("voice engine is not running".to_string())
}
