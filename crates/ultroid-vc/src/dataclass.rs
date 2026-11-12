//! VC data classes. Port of `vc/_dataclass.py`.

use serde::{Deserialize, Serialize};

/// Per-chat channel-play state (a channel's VC played into a group).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelPlay {
    pub chat_id: i64,
    pub is_channel: bool,
}

/// A cached track: everything the queue needs to (re)play it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedTrack {
    pub url: String,
    pub name: String,
    pub artist: String,
    #[serde(default)]
    pub loop_count: u32,
    pub user: String,
    pub file_path: Option<String>,
    pub thumbnail: Option<String>,
    pub track_id: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub is_video: bool,
    pub platform: String,
    pub channel: Option<ChannelPlay>,
}

impl CachedTrack {
    pub fn display(&self) -> String {
        if self.artist.is_empty() {
            self.name.clone()
        } else {
            format!("{} — {}", self.artist, self.name)
        }
    }
}

/// Lightweight track info (search results / resolver output).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: u64,
    pub url: String,
    pub thumbnail: Option<String>,
    pub platform: String,
    pub is_video: bool,
}

/// A resolved, playable music track (with its stream URL).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicTrack {
    pub info: TrackInfo,
    pub stream_url: String,
    pub file_path: Option<String>,
}

/// A page of platform search results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformTracks {
    pub platform: String,
    pub query: String,
    pub tracks: Vec<TrackInfo>,
}
