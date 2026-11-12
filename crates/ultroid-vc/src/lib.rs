//! `ultroid-vc` — voice chat: grammers signalling bridge + ntgcalls audio.
//!
//! Port of `source/vc/` (`_pytgcalls.py`, `_player.py`, `_dataclass.py`,
//! `_query.py`, `_tools.py`, `_messages.py`, `_database.py`, `_cacher.py`,
//! `_diagnose.py`, `_resolver.py`, `_youtube.py`, `_api.py`,
//! `_downloader.py`, `_dl_helper.py`, `_httpx.py`, `_saavn.py`,
//! `_jiosaavn.py`, `_spotify.py`, `_yt.py`) reimplemented against
//! grammers 0.12 / TL layer 233 and the ntgcalls 3.0.7 Rust crate.
//! See DESIGN.md §VC and PYTGALLS-RESEARCH.md for the verified flow.
//!
//! The audio path is real: `ntgcalls` (WebRTC/SRTP/Opus) drives the
//! media, `bridge` runs the `phone.*` signalling. No live join has been
//! performed from the sandbox — that is stated in the README, never
//! claimed as verified.

pub mod api;
pub mod bridge;
pub mod cacher;
pub mod command;
pub mod database;
pub mod dataclass;
pub mod diagnose;
pub mod dl_helper;
pub mod downloader;
pub mod engine;
pub mod httpx;
pub mod jiosaavn;
pub mod media;
pub mod messages;
pub mod player;
pub mod query;
pub mod resolver;
pub mod saavn;
pub mod spotify;
pub mod tools;
pub mod youtube;
pub mod yt;

#[cfg(test)]
mod tests;

pub use api::ApiData;
pub use bridge::{Bridge, BridgeEvent, CachedCall, CallParticipant};
pub use command::VcCommand;
pub use database::VcDatabase;
pub use dataclass::{CachedTrack, ChannelPlay, MusicTrack, PlatformTracks, TrackInfo};
pub use dl_helper::SpotifyDownload;
pub use downloader::{MusicService, MusicServiceWrapper, song_download};
pub use engine::{CallStats, Engine, P2pOffer};
pub use httpx::{DownloadResult, HttpxClient};
pub use jiosaavn::{JioSaavn, JiosaavnData};
pub use media::{AudioQuality, MediaStream, ProbeResult, VideoQuality};
pub use player::{PlaybackState, PlayerRegistry, PlayerSnapshot, PlayerState};
pub use resolver::{AudioStream, ExtractionError, resolve_audio};
pub use saavn::MeloApi;
pub use spotify::Spotify;
pub use youtube::{YouTube, YouTubeData};
pub use yt::YouTubeApi;
