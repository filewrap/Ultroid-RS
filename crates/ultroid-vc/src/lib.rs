//! `ultroid-vc` — voice chat: grammers signalling bridge + ntgcalls audio.
//!
//! Port of `source/vc/` (`_pytgcalls.py`, `_player.py`, `_dataclass.py`,
//! `_query.py`, `_tools.py`, `_messages.py`, `_database.py`, `_cacher.py`,
//! `_diagnose.py`) reimplemented against grammers 0.12 / TL layer 233
//! and the ntgcalls 3.0.7 Rust crate. See DESIGN.md §VC and
//! PYTGALLS-RESEARCH.md for the verified flow.
//!
//! The audio path is real: `ntgcalls` (WebRTC/SRTP/Opus) drives the
//! media, `bridge` runs the `phone.*` signalling. No live join has been
//! performed from the sandbox — that is stated in the README, never
//! claimed as verified.

pub mod bridge;
pub mod cacher;
pub mod dataclass;
pub mod database;
pub mod diagnose;
pub mod engine;
pub mod media;
pub mod messages;
pub mod player;
pub mod query;
pub mod tools;

#[cfg(test)]
mod tests;

pub use bridge::{Bridge, BridgeEvent, CachedCall, CallParticipant};
pub use dataclass::{CachedTrack, ChannelPlay, MusicTrack, PlatformTracks, TrackInfo};
pub use database::VcDatabase;
pub use engine::{CallStats, Engine, P2pOffer};
pub use media::{AudioQuality, MediaStream, ProbeResult, VideoQuality};
pub use player::{PlaybackState, PlayerRegistry, PlayerSnapshot, PlayerState};
