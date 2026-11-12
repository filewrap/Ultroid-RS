//! JioSaavn music service. Port of `vc/_jiosaavn.py` (227 LOC).
//!
//! The low-level `JioSaavn` helper talks to
//! `https://jiosaavn-api-privatecvc2.vercel.app` (`GET /search/songs`
//! with `query`/`limit`/`page`), downloads the highest-quality
//! audio + cover, and tags the `.m4a`. `JiosaavnData` wraps it and
//! implements `MusicService`.
//!
//! Python tags with mutagen (`MP4` atoms `©nam`/`©ART`/`©alb`/
//! `©day`/`cprt`/`covr`); there is no mutagen in Rust and no new
//! dependency may be added, so the same tags are written with
//! `ffmpeg` (`-metadata title/artist/album/date/copyright` +
//! attached-pic cover, `-c copy` remux), which maps to exactly
//! those MP4 atoms.

use crate::dataclass::{MusicTrack, PlatformTracks, TrackInfo};
use crate::downloader::MusicService;
use crate::tools::DOWNLOADS_DIR;
use async_trait::async_trait;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{error, info};
use ultroid_core::error::{Error, Result};

/// JioSaavn API base URL (mirrors `_BASE_URL`).
pub const BASE_URL: &str = "https://jiosaavn-api-privatecvc2.vercel.app";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

fn saavn_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(jiosaavn\.com|saavn\.com)").expect("static Saavn regex"))
}

/// The `link` of the last item in a list (`""` if unavailable).
pub fn last_link(items: Option<&[serde_json::Value]>) -> String {
    let Some(items) = items else {
        return String::new();
    };
    items
        .last()
        .and_then(|i| i.get("link"))
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .to_string()
}

/// Song payload for `download_song` (the dict Python passes around).
#[derive(Debug, Clone, Default)]
pub struct SaavnSong {
    pub name: String,
    pub primary_artists: String,
    pub album: String,
    pub year: String,
    pub copyright: String,
    pub download_url: String,
    pub image_url: String,
}

impl SaavnSong {
    /// Build from a raw API song object.
    pub fn from_json(song: &serde_json::Value) -> Self {
        let album = match song.get("album") {
            Some(serde_json::Value::Object(_)) => song
                .get("album")
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
            _ => "Unknown".to_string(),
        };
        SaavnSong {
            name: song
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            primary_artists: song
                .get("primaryArtists")
                .and_then(|a| a.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            album,
            year: song
                .get("year")
                .and_then(|y| y.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    song.get("year")
                        .and_then(|y| y.as_i64())
                        .map(|y| y.to_string())
                })
                .unwrap_or_else(|| "Unknown".to_string()),
            copyright: song
                .get("copyright")
                .and_then(|c| c.as_str())
                .unwrap_or("• @AgainOwner")
                .to_string(),
            download_url: last_link(
                song.get("downloadUrl")
                    .and_then(|d| d.as_array())
                    .map(|a| a.as_slice()),
            ),
            image_url: last_link(
                song.get("image")
                    .and_then(|i| i.as_array())
                    .map(|a| a.as_slice()),
            ),
        }
    }
}

/// Low-level JioSaavn API helper.
pub struct JioSaavn {
    pub base_url: String,
    client: reqwest::Client,
}

impl Default for JioSaavn {
    fn default() -> Self {
        JioSaavn::new()
    }
}

impl JioSaavn {
    pub fn new() -> Self {
        let _ = std::fs::create_dir_all(DOWNLOADS_DIR);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        JioSaavn {
            base_url: BASE_URL.to_string(),
            client,
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(USER_AGENT),
        );
        h
    }

    /// Search for songs; returns the `data.results` list.
    pub async fn search_songs(
        &self,
        query: &str,
        limit: usize,
        page: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let resp = self
            .client
            .get(format!("{}/search/songs", self.base_url))
            .headers(self.headers())
            .query(&[
                ("query", query),
                ("limit", &limit.to_string()),
                ("page", &page.to_string()),
            ])
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let data: serde_json::Value = resp.json().await?;
        Ok(data
            .get("data")
            .and_then(|d| d.get("results"))
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default())
    }

    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let resp = client.get(url).headers(self.headers()).send().await?;
        let resp = resp.error_for_status()?;
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Write mutagen-equivalent MP4 metadata with ffmpeg.
    fn apply_metadata(file_path: &Path, song: &SaavnSong, image_data: &[u8]) -> Result<()> {
        let cover_path = file_path.with_extension("cover.jpg");
        std::fs::write(&cover_path, image_data)?;
        let tmp_path = file_path.with_extension("tagged.m4a");
        let output = std::process::Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(file_path)
            .arg("-i")
            .arg(&cover_path)
            .arg("-map")
            .arg("0")
            .arg("-map")
            .arg("1")
            .arg("-c")
            .arg("copy")
            .arg("-metadata")
            .arg(format!("title={}", song.name))
            .arg("-metadata")
            .arg(format!("artist={}", song.primary_artists))
            .arg("-metadata")
            .arg(format!("album={}", song.album))
            .arg("-metadata")
            .arg(format!("date={}", song.year))
            .arg("-metadata")
            .arg(format!("copyright={}", song.copyright))
            .arg("-disposition:v")
            .arg("attached_pic")
            .arg(&tmp_path)
            .output();
        let _ = std::fs::remove_file(&cover_path);
        let output = output?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Other(format!(
                "ffmpeg metadata tagging failed: {}",
                stderr.trim()
            )));
        }
        std::fs::rename(&tmp_path, file_path)?;
        Ok(())
    }

    /// Download the highest-quality version and apply metadata.
    pub async fn download_song(&self, song: &SaavnSong, output_folder: &Path) -> Result<PathBuf> {
        if song.download_url.is_empty() {
            return Err(Error::Other("song has no download URL".to_string()));
        }
        let file_name = format!("{} - {}.m4a", song.name, song.primary_artists).replace('/', "-");
        let file_path = output_folder.join(file_name);
        info!("[JioSavn] Downloading: {}...", song.name);
        let (audio, image) = tokio::join!(
            self.download_bytes(&song.download_url),
            self.download_bytes(&song.image_url)
        );
        let audio_bytes = audio?;
        let image_bytes = image?;
        tokio::fs::write(&file_path, &audio_bytes).await?;
        let file_path_clone = file_path.clone();
        let song_clone = song.clone();
        tokio::task::spawn_blocking(move || {
            Self::apply_metadata(&file_path_clone, &song_clone, &image_bytes)
        })
        .await
        .map_err(|e| Error::Other(format!("metadata task failed: {e}")))??;
        info!("[JioSavn] Saved and Tagged: {}", file_path.display());
        Ok(file_path)
    }
}

/// JioSaavn service implementing the `MusicService` interface.
pub struct JiosaavnData {
    pub query: String,
    saavn: JioSaavn,
}

impl JiosaavnData {
    pub fn new(query: Option<&str>) -> Self {
        JiosaavnData {
            query: query.unwrap_or_default().to_string(),
            saavn: JioSaavn::new(),
        }
    }

    fn album_name(song: &serde_json::Value) -> String {
        match song.get("album") {
            Some(serde_json::Value::Object(_)) => song
                .get("album")
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
            _ => "Unknown".to_string(),
        }
    }

    fn song_to_track(song: &serde_json::Value) -> MusicTrack {
        let id = song
            .get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                song.get("id")
                    .and_then(|i| i.as_i64())
                    .map(|i| i.to_string())
            })
            .unwrap_or_default();
        MusicTrack {
            info: TrackInfo {
                url: song
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: song
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("Unknown")
                    .to_string(),
                artist: song
                    .get("primaryArtists")
                    .and_then(|a| a.as_str())
                    .unwrap_or("Unknown")
                    .to_string(),
                id,
                year: song.get("year").and_then(|y| y.as_i64()).unwrap_or(0),
                thumbnail: Some(last_link(
                    song.get("image")
                        .and_then(|i| i.as_array())
                        .map(|a| a.as_slice()),
                )),
                duration_secs: song.get("duration").and_then(|d| d.as_u64()).unwrap_or(0),
                platform: "jiosaavn".to_string(),
                ..Default::default()
            },
            stream_url: song
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string(),
            file_path: None,
        }
    }

    fn platform_tracks(tracks: Vec<MusicTrack>, query: &str) -> Option<PlatformTracks> {
        if tracks.is_empty() {
            return None;
        }
        Some(PlatformTracks {
            platform: "jiosaavn".to_string(),
            query: query.to_string(),
            tracks: tracks.into_iter().map(|t| t.info).collect(),
        })
    }
}

#[async_trait]
impl MusicService for JiosaavnData {
    fn service_name(&self) -> &'static str {
        "jiosaavn"
    }

    fn is_valid(&self, url: &str) -> bool {
        !url.is_empty() && saavn_url_re().is_match(url)
    }

    async fn get_info(&self) -> Option<PlatformTracks> {
        if self.query.is_empty() || !self.is_valid(&self.query) {
            return None;
        }
        match self.saavn.search_songs(&self.query, 1, 1).await {
            Ok(results) => {
                let tracks: Vec<MusicTrack> = results.iter().map(Self::song_to_track).collect();
                Self::platform_tracks(tracks, &self.query)
            }
            Err(e) => {
                error!("JioSaavn get_info failed: {e}");
                None
            }
        }
    }

    async fn search(&self) -> Option<PlatformTracks> {
        if self.query.is_empty() {
            return None;
        }
        if self.is_valid(&self.query) {
            return self.get_info().await;
        }
        match self.saavn.search_songs(&self.query, 5, 1).await {
            Ok(results) => {
                let tracks: Vec<MusicTrack> = results.iter().map(Self::song_to_track).collect();
                Self::platform_tracks(tracks, &self.query)
            }
            Err(e) => {
                error!("JioSaavn search failed: {e}");
                None
            }
        }
    }

    async fn get_recommendations(&self) -> Option<PlatformTracks> {
        // The JioSaavn API used here exposes no recommendations endpoint.
        None
    }

    async fn get_track(&self) -> Option<TrackInfo> {
        if self.query.is_empty() {
            return None;
        }
        let results = match self.saavn.search_songs(&self.query, 1, 1).await {
            Ok(r) => r,
            Err(e) => {
                error!("JioSaavn get_track failed: {e}");
                return None;
            }
        };
        let song = results.into_iter().next()?;
        let id = song
            .get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                song.get("id")
                    .and_then(|i| i.as_i64())
                    .map(|i| i.to_string())
            })
            .unwrap_or_default();
        Some(TrackInfo {
            url: song
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .to_string(),
            cdnurl: last_link(
                song.get("downloadUrl")
                    .and_then(|d| d.as_array())
                    .map(|a| a.as_slice()),
            ),
            key: String::new(),
            title: song
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            artist: song
                .get("primaryArtists")
                .and_then(|a| a.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            album: Self::album_name(&song),
            id,
            thumbnail: Some(last_link(
                song.get("image")
                    .and_then(|i| i.as_array())
                    .map(|a| a.as_slice()),
            )),
            lyrics: String::new(),
            duration_secs: song.get("duration").and_then(|d| d.as_u64()).unwrap_or(0),
            year: song.get("year").and_then(|y| y.as_i64()).unwrap_or(0),
            platform: "jiosaavn".to_string(),
            ..Default::default()
        })
    }

    async fn download_track(&self, track: &TrackInfo, _video: bool) -> Option<PathBuf> {
        // Reconstruct the song payload from TrackInfo so the helper
        // can stream-download the audio and tag it without an extra
        // API call (mirrors Python).
        let song = SaavnSong {
            name: track.title.clone(),
            primary_artists: track.artist.clone(),
            album: track.album.clone(),
            year: track.year.to_string(),
            copyright: String::new(),
            download_url: track.cdnurl.clone(),
            image_url: track.thumbnail.clone().unwrap_or_default(),
        };
        match self
            .saavn
            .download_song(&song, Path::new(DOWNLOADS_DIR))
            .await
        {
            Ok(p) => Some(p),
            Err(e) => {
                error!("JioSaavn download_track failed: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_link_takes_last() {
        let items = serde_json::json!([
            {"link": "https://x/96"},
            {"link": "https://x/320"}
        ]);
        let arr = items.as_array().unwrap();
        assert_eq!(last_link(Some(arr)), "https://x/320");
        assert_eq!(last_link(None), "");
        assert_eq!(last_link(Some(&[])), "");
    }

    #[test]
    fn is_valid_matches_saavn_domains() {
        let svc = JiosaavnData::new(None);
        assert!(svc.is_valid("https://www.jiosaavn.com/song/abc/xyz"));
        assert!(svc.is_valid("https://www.saavn.com/song/abc"));
        assert!(!svc.is_valid("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(!svc.is_valid(""));
    }

    #[test]
    fn song_to_track_maps_fields() {
        let song = serde_json::json!({
            "url": "https://www.jiosaavn.com/song/x",
            "name": "Mehrama",
            "primaryArtists": "Darshan Raval",
            "id": "abc123",
            "year": 2020,
            "image": [{"link": "https://x/50"}, {"link": "https://x/500"}],
            "duration": 233,
            "downloadUrl": [{"link": "https://cdn/96"}, {"link": "https://cdn/320"}]
        });
        let track = JiosaavnData::song_to_track(&song);
        assert_eq!(track.info.id, "abc123");
        assert_eq!(track.info.title, "Mehrama");
        assert_eq!(track.info.artist, "Darshan Raval");
        assert_eq!(track.info.year, 2020);
        assert_eq!(track.info.duration_secs, 233);
        assert_eq!(track.info.thumbnail.as_deref(), Some("https://x/500"));
        assert_eq!(track.info.platform, "jiosaavn");
    }

    #[test]
    fn album_name_handles_forms() {
        let dict = serde_json::json!({"album": {"name": "Love Aaj Kal"}});
        assert_eq!(JiosaavnData::album_name(&dict), "Love Aaj Kal");
        let string = serde_json::json!({"album": "Love Aaj Kal"});
        assert_eq!(JiosaavnData::album_name(&string), "Love Aaj Kal");
        let missing = serde_json::json!({});
        assert_eq!(JiosaavnData::album_name(&missing), "Unknown");
    }
}
