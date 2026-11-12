//! `VcControl` implementation: an mpsc sender to the voice-engine
//! thread (see `ultroid_core::vc`). One command per call, answered
//! over a oneshot; a closed channel or a dropped reply means the
//! engine thread is gone.

use tokio::sync::{mpsc, oneshot};
use ultroid_core::error::Result;
use ultroid_core::vc::{VcControl, VcTrack, vc_engine_gone};
use ultroid_vc::VcCommand;
use ultroid_vc::command::engine_gone;
use ultroid_vc::dataclass::{CachedTrack, ChannelPlay};

/// The sender half of the engine's command channel.
#[derive(Clone)]
pub struct VcControlHandle {
    tx: mpsc::Sender<VcCommand>,
}

impl VcControlHandle {
    pub fn new(tx: mpsc::Sender<VcCommand>) -> Self {
        VcControlHandle { tx }
    }

    /// Send a command and await its oneshot reply.
    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> VcCommand,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(build(reply_tx))
            .await
            .map_err(|_| vc_engine_gone())?;
        reply_rx.await.map_err(|_| engine_gone())?
    }
}

#[async_trait::async_trait]
impl VcControl for VcControlHandle {
    async fn enqueue(&self, chat_id: i64, track: VcTrack) -> Result<bool> {
        self.call(|reply| VcCommand::Enqueue {
            chat_id,
            track: vc_track_to_cached(track),
            reply,
        })
        .await
    }

    async fn play_next(&self, chat_id: i64) -> Result<()> {
        self.call(|reply| VcCommand::PlayNext { chat_id, reply })
            .await
    }

    async fn pause(&self, chat_id: i64) -> Result<bool> {
        self.call(|reply| VcCommand::Pause { chat_id, reply }).await
    }

    async fn resume(&self, chat_id: i64) -> Result<bool> {
        self.call(|reply| VcCommand::Resume { chat_id, reply })
            .await
    }

    async fn mute(&self, chat_id: i64) -> Result<bool> {
        self.call(|reply| VcCommand::Mute { chat_id, reply }).await
    }

    async fn unmute(&self, chat_id: i64) -> Result<bool> {
        self.call(|reply| VcCommand::Unmute { chat_id, reply })
            .await
    }

    async fn end(&self, chat_id: i64) -> Result<()> {
        self.call(|reply| VcCommand::End { chat_id, reply }).await
    }
}

/// Convert the core plain-data track back into the vc crate's
/// `CachedTrack` (field-for-field; the two types are kept in sync
/// by hand — see `ultroid_core::vc::VcTrack`).
fn vc_track_to_cached(t: VcTrack) -> CachedTrack {
    CachedTrack {
        url: t.url,
        name: t.name,
        artist: t.artist,
        loop_count: t.loop_count,
        user: t.user,
        file_path: t.file_path,
        thumbnail: t.thumbnail,
        track_id: t.track_id,
        duration: t.duration,
        is_video: t.is_video,
        platform: t.platform,
        channel: t.channel.map(|c| ChannelPlay {
            chat_id: c.chat_id,
            is_channel: c.is_channel,
        }),
    }
}
