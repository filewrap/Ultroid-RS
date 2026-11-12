//! Track downloading / service dispatch. Port of `vc/_downloader.py`.
//!
//! `MusicService` is the abstract interface every provider
//! implements (`is_valid`, `get_info`, `search`,
//! `get_recommendations`, `get_track`, `download_track`).
//! `MusicServiceWrapper` picks the provider for a query in the
//! exact order Python does: YouTube URL, JioSaavn URL, API URL
//! (Apple Music / Spotify / SoundCloud), then the configured
//! default service, else the API service when configured, else
//! YouTube.
//!
//! `DEFAULT_SERVICE` in Python reads the `udB` key
//! `DEFAULT_SERVICE`, then `Var.DEFAULT_SERVICE`, then `"youtube"`.
//! The Rust port reads the environment variable `DEFAULT_SERVICE`
//! (the Var/env half of that chain — there is no `udB` access in
//! this layer); the default is still `"youtube"`.
//!
//! `song_download` is the `Call.song_download` static from
//! `_pytgcalls.py`, kept here because it *is* the service dispatch
//! for a cached track (platform → provider → `get_track` →
//! `download_track`).

use crate::api::ApiData;
use crate::dataclass::{CachedTrack, PlatformTracks, TrackInfo};
use crate::httpx::api_key;
use crate::httpx::api_url;
use crate::jiosaavn::JiosaavnData;
use crate::youtube::YouTubeData;
use async_trait::async_trait;
use std::path::PathBuf;
use tracing::{error, warn};

/// Default music service when the query matches no provider.
pub fn default_service() -> String {
    std::env::var("DEFAULT_SERVICE").unwrap_or_else(|_| "youtube".to_string())
}

/// The music-service interface (port of the Python `MusicService` ABC).
///
/// Methods return `None` on failure, mirroring Python (which logs
/// and returns `None` rather than raising).
#[async_trait]
pub trait MusicService: Send + Sync {
    /// Stable provider name (`"youtube"`, `"jiosaavn"`, `"api"`).
    fn service_name(&self) -> &'static str;
    fn is_valid(&self, url: &str) -> bool;
    async fn get_info(&self) -> Option<PlatformTracks>;
    async fn search(&self) -> Option<PlatformTracks>;
    async fn get_recommendations(&self) -> Option<PlatformTracks>;
    async fn get_track(&self) -> Option<TrackInfo>;
    async fn download_track(&self, track: &TrackInfo, video: bool) -> Option<PathBuf>;
}

/// Dispatch wrapper: one query, the right provider.
pub struct MusicServiceWrapper {
    pub query: String,
    service: Box<dyn MusicService>,
}

impl MusicServiceWrapper {
    pub fn new(query: impl Into<String>) -> Self {
        let query = query.into();
        let service = get_service(&query, &default_service());
        MusicServiceWrapper { query, service }
    }

    pub fn with_default_service(query: impl Into<String>, default: &str) -> Self {
        let query = query.into();
        let service = get_service(&query, default);
        MusicServiceWrapper { query, service }
    }

    /// Which provider was selected (the concrete type name).
    pub fn service_name(&self) -> &'static str {
        self.service.service_name()
    }
}

fn get_service(query: &str, default: &str) -> Box<dyn MusicService> {
    if YouTubeData::new(None).is_valid(query) {
        return Box::new(YouTubeData::new(Some(query)));
    }
    if JiosaavnData::new(None).is_valid(query) {
        return Box::new(JiosaavnData::new(Some(query)));
    }
    if ApiData::new(None).is_valid(query) {
        return Box::new(ApiData::new(Some(query)));
    }
    match default {
        "youtube" => Box::new(YouTubeData::new(Some(query))),
        "spotify" => Box::new(ApiData::new(Some(query))),
        "jiosaavn" => Box::new(JiosaavnData::new(Some(query))),
        _ => {
            if !api_url().is_empty() && !api_key().is_empty() {
                Box::new(ApiData::new(Some(query)))
            } else {
                Box::new(YouTubeData::new(Some(query)))
            }
        }
    }
}

#[async_trait]
impl MusicService for MusicServiceWrapper {
    fn service_name(&self) -> &'static str {
        self.service.service_name()
    }

    fn is_valid(&self, url: &str) -> bool {
        self.service.is_valid(url)
    }

    async fn get_info(&self) -> Option<PlatformTracks> {
        self.service.get_info().await
    }

    async fn search(&self) -> Option<PlatformTracks> {
        self.service.search().await
    }

    async fn get_recommendations(&self) -> Option<PlatformTracks> {
        self.service.get_recommendations().await
    }

    async fn get_track(&self) -> Option<TrackInfo> {
        self.service.get_track().await
    }

    async fn download_track(&self, track: &TrackInfo, video: bool) -> Option<PathBuf> {
        // NOTE: Python's wrapper drops `video` here
        // (`self.service.download_track(track_info)` — no second
        // argument). Mirrored exactly.
        let _ = video;
        self.service.download_track(track, false).await
    }
}

/// Download a cached track from its source platform.
///
/// Mirrors `Call.song_download` (`_pytgcalls.py`): the platform map
/// is `youtube → YouTubeData(track_id)`, `jiosaavn →
/// JiosaavnData(url)`, `spotify → ApiData(track_id)`,
/// `apple_music` / `soundcloud → ApiData(url)`. Unknown platforms
/// log a warning and return `None`.
pub async fn song_download(song: &CachedTrack) -> Option<PathBuf> {
    let handler: Box<dyn MusicService> = match song.platform.to_lowercase().as_str() {
        "youtube" => Box::new(YouTubeData::new(Some(&song.track_id))),
        "jiosaavn" => Box::new(JiosaavnData::new(Some(&song.url))),
        "spotify" => Box::new(ApiData::new(Some(&song.track_id))),
        "apple_music" => Box::new(ApiData::new(Some(&song.url))),
        "soundcloud" => Box::new(ApiData::new(Some(&song.url))),
        other => {
            warn!("Unsupported platform: {other} for track: {}", song.track_id);
            return None;
        }
    };
    let track = match handler.get_track().await {
        Some(t) => t,
        None => return None,
    };
    match handler.download_track(&track, song.is_video).await {
        Some(p) => Some(p),
        None => {
            error!("Download failed for {}", song.track_id);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_picks_youtube_for_yt_url() {
        let w = MusicServiceWrapper::new("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(w.service_name(), "youtube");
    }

    #[test]
    fn wrapper_picks_jiosaavn_for_saavn_url() {
        let w = MusicServiceWrapper::new("https://www.jiosaavn.com/song/abc");
        assert_eq!(w.service_name(), "jiosaavn");
    }

    #[test]
    fn wrapper_falls_back_to_default_for_plain_text() {
        let w = MusicServiceWrapper::with_default_service("never gonna give you up", "youtube");
        assert_eq!(w.service_name(), "youtube");
        let w = MusicServiceWrapper::with_default_service("never gonna give you up", "jiosaavn");
        assert_eq!(w.service_name(), "jiosaavn");
    }

    #[test]
    fn default_service_is_youtube_without_env() {
        unsafe { std::env::remove_var("DEFAULT_SERVICE") };
        assert_eq!(default_service(), "youtube");
    }
}
