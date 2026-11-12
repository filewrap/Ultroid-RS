//! Music API client. Port of `vc/_api.py` (205 LOC).
//!
//! `ApiData` resolves Apple Music / Spotify / SoundCloud URLs and
//! free-text searches through the configured music API
//! (`MUSIC_API_URL` + `MUSIC_API_KEY` env vars, mirroring
//! `_tools.py`): `GET {api_url}/recommend_songs?lim=…`,
//! `get_url?url=…`, `search_track?q=…`, `get_track?id=…`, all with
//! the `X-API-Key` header (added by `HttpxClient`).
//!
//! Spotify tracks download through `SpotifyDownload`
//! (`_dl_helper.py`); anything else with a `cdnurl` downloads
//! straight to `./temp/{tc}.mp3`.

use crate::dataclass::{MusicTrack, PlatformTracks, TrackInfo};
use crate::dl_helper::SpotifyDownload;
use crate::downloader::MusicService;
use crate::httpx::{HttpxClient, api_key, api_url};
use crate::tools::DOWNLOADS_DIR;
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::error;

/// One API track as the music API returns it (Python
/// `TrackInfo(**data)` — same field names, `tc` is the track id).
#[derive(Debug, Clone, Default, Deserialize)]
struct ApiTrackInfo {
    #[serde(default)]
    url: String,
    #[serde(default)]
    cdnurl: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    tc: String,
    #[serde(default)]
    cover: String,
    #[serde(default)]
    lyrics: String,
    #[serde(default)]
    duration: i64,
    #[serde(default)]
    year: i64,
    #[serde(default)]
    platform: String,
}

impl From<ApiTrackInfo> for TrackInfo {
    fn from(a: ApiTrackInfo) -> Self {
        TrackInfo {
            url: a.url,
            cdnurl: a.cdnurl,
            key: a.key,
            title: a.name,
            artist: a.artist,
            album: a.album,
            id: a.tc,
            thumbnail: if a.cover.is_empty() {
                None
            } else {
                Some(a.cover)
            },
            lyrics: a.lyrics,
            duration_secs: a.duration.max(0) as u64,
            year: a.year,
            platform: a.platform,
            ..Default::default()
        }
    }
}

/// One search-result track (Python `MusicTrack(**track)`).
#[derive(Debug, Clone, Default, Deserialize)]
struct ApiMusicTrack {
    #[serde(default)]
    url: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    year: i64,
    #[serde(default)]
    cover: String,
    #[serde(default)]
    duration: i64,
    #[serde(default)]
    platform: String,
}

impl From<ApiMusicTrack> for MusicTrack {
    fn from(m: ApiMusicTrack) -> Self {
        MusicTrack {
            info: TrackInfo {
                url: m.url.clone(),
                title: m.name,
                artist: m.artist,
                id: m.id,
                year: m.year,
                thumbnail: if m.cover.is_empty() {
                    None
                } else {
                    Some(m.cover)
                },
                duration_secs: m.duration.max(0) as u64,
                platform: m.platform,
                ..Default::default()
            },
            stream_url: m.url,
            file_path: None,
        }
    }
}

fn apple_music_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(https?://)?(music\.apple\.com/([a-z]{2}/)?(album|playlist|song)/[a-zA-Z0-9\-_]+/[0-9]+)(\?.*)?$")
            .expect("static Apple Music regex")
    })
}

fn spotify_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(https?://)?(open\.spotify\.com/(track|playlist|album|artist)/[a-zA-Z0-9]+)(\?.*)?$")
            .expect("static Spotify URL regex")
    })
}

fn soundcloud_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(https?://)?(soundcloud\.com/[a-zA-Z0-9\-_]+/[a-zA-Z0-9\-_]+)(\?.*)?$")
            .expect("static SoundCloud regex")
    })
}

/// Clean and normalize the input query (mirrors `_sanitize_query`).
pub fn sanitize_query(query: &str) -> String {
    query
        .trim()
        .split('?')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .to_string()
}

/// API-backed music service (port of `ApiData`).
pub struct ApiData {
    pub query: Option<String>,
    client: HttpxClient,
    pub api_url: Option<String>,
    pub api_key: String,
}

impl ApiData {
    pub fn new(query: Option<&str>) -> Self {
        let url = api_url();
        ApiData {
            query: query.map(sanitize_query),
            client: HttpxClient::default(),
            api_url: if url.is_empty() {
                None
            } else {
                Some(url.trim_end_matches('/').to_string())
            },
            api_key: api_key(),
        }
    }

    async fn make_api_request(
        &self,
        endpoint: &str,
        params: Option<&[(&str, &str)]>,
    ) -> Option<serde_json::Value> {
        let Some(base) = &self.api_url else {
            error!("API configuration missing");
            return None;
        };
        if self.api_key.is_empty() {
            error!("API configuration missing");
            return None;
        }
        let url = format!("{}/{}", base, endpoint.trim_start_matches('/'));
        self.client.make_request_default(&url, params).await
    }

    fn parse_tracks_response(data: &serde_json::Value) -> Option<PlatformTracks> {
        let results = data.get("results")?.as_array()?;
        let tracks: Vec<MusicTrack> = results
            .iter()
            .filter_map(|t| t.as_object().map(|_| t.clone()))
            .filter_map(|t| serde_json::from_value::<ApiMusicTrack>(t).ok())
            .map(MusicTrack::from)
            .collect();
        if tracks.is_empty() {
            return None;
        }
        let platform = tracks
            .first()
            .map(|t| t.info.platform.clone())
            .unwrap_or_default();
        Some(PlatformTracks {
            platform,
            query: String::new(),
            tracks: tracks.into_iter().map(|t| t.info).collect(),
        })
    }
}

#[async_trait]
impl MusicService for ApiData {
    fn service_name(&self) -> &'static str {
        "api"
    }

    fn is_valid(&self, url: &str) -> bool {
        if url.is_empty() || self.api_url.is_none() || self.api_key.is_empty() {
            return false;
        }
        apple_music_re().is_match(url)
            || spotify_url_re().is_match(url)
            || soundcloud_re().is_match(url)
    }

    async fn get_recommendations(&self) -> Option<PlatformTracks> {
        let limit = "4".to_string();
        let data = self
            .make_api_request("recommend_songs", Some(&[("lim", limit.as_str())]))
            .await?;
        Self::parse_tracks_response(&data)
    }

    async fn get_info(&self) -> Option<PlatformTracks> {
        let query = self.query.clone()?;
        if !self.is_valid(&query) {
            return None;
        }
        let data = self
            .make_api_request("get_url", Some(&[("url", query.as_str())]))
            .await?;
        Self::parse_tracks_response(&data)
    }

    async fn search(&self) -> Option<PlatformTracks> {
        let query = self.query.clone()?;
        if self.is_valid(&query) {
            return self.get_info().await;
        }
        let data = self
            .make_api_request("search_track", Some(&[("q", query.as_str())]))
            .await?;
        Self::parse_tracks_response(&data)
    }

    async fn get_track(&self) -> Option<TrackInfo> {
        let query = self.query.clone()?;
        let data = self
            .make_api_request("get_track", Some(&[("id", query.as_str())]))
            .await?;
        serde_json::from_value::<ApiTrackInfo>(data)
            .ok()
            .map(TrackInfo::from)
    }

    async fn download_track(&self, track: &TrackInfo, _video: bool) -> Option<PathBuf> {
        if track.platform.eq_ignore_ascii_case("spotify") {
            return SpotifyDownload::new(track.clone()).process().await;
        }
        if track.cdnurl.is_empty() {
            error!("No download URL available for track {}", track.id);
            return None;
        }
        let download_path = Path::new(DOWNLOADS_DIR).join(format!("{}.mp3", track.id));
        let result = self
            .client
            .download_file(&track.cdnurl, Some(&download_path), false)
            .await;
        if !result.success {
            error!("Download failed for track {}", track.id);
            return None;
        }
        result.file_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_query_strips_params() {
        assert_eq!(
            sanitize_query("  https://open.spotify.com/track/abc?si=x#frag  "),
            "https://open.spotify.com/track/abc"
        );
        assert_eq!(
            sanitize_query("never gonna give you up"),
            "never gonna give you up"
        );
    }

    #[test]
    fn is_valid_needs_config() {
        // No MUSIC_API_URL/MUSIC_API_KEY in the test env, so no URL
        // is valid — exactly like Python with both empty.
        unsafe {
            std::env::remove_var("MUSIC_API_URL");
            std::env::remove_var("MUSIC_API_KEY");
        }
        let svc = ApiData::new(None);
        assert!(!svc.is_valid("https://open.spotify.com/track/abc123"));
    }

    #[test]
    fn url_patterns_match_python() {
        assert!(spotify_url_re().is_match("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC"));
        assert!(spotify_url_re().is_match("open.spotify.com/playlist/abc123"));
        assert!(!spotify_url_re().is_match("https://open.spotify.com/search/abc"));
        assert!(apple_music_re().is_match("https://music.apple.com/us/album/abc-def/123456789"));
        assert!(apple_music_re().is_match("https://music.apple.com/us/song/abc/123456789"));
        assert!(!apple_music_re().is_match("https://music.apple.com/us/artist/abc/123"));
        assert!(soundcloud_re().is_match("https://soundcloud.com/artist/track-name"));
        assert!(!soundcloud_re().is_match("https://soundcloud.com/artist"));
    }

    #[test]
    fn parse_tracks_response_maps_results() {
        let data = serde_json::json!({
            "results": [
                {"url": "https://x/1", "name": "One", "artist": "A", "id": "1",
                 "year": 2020, "cover": "https://x/c1", "duration": 200, "platform": "spotify"},
                {"url": "https://x/2", "name": "Two", "artist": "B", "id": "2",
                 "year": 2021, "cover": "", "duration": 180, "platform": "spotify"}
            ]
        });
        let parsed = ApiData::parse_tracks_response(&data).unwrap();
        assert_eq!(parsed.tracks.len(), 2);
        assert_eq!(parsed.tracks[0].title, "One");
        assert_eq!(parsed.tracks[0].duration_secs, 200);
        assert_eq!(parsed.platform, "spotify");
        assert!(ApiData::parse_tracks_response(&serde_json::json!({})).is_none());
        assert!(ApiData::parse_tracks_response(&serde_json::json!({"results": []})).is_none());
    }
}
