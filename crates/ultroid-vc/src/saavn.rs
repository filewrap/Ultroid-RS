//! Saavn via the MeloAPI proxy. Port of `vc/_saavn.py` (134 LOC).
//!
//! `MeloAPI` talks to `https://meloapi.vercel.app`: `GET
//! /api/search?query=…` then `GET /api/songs/{id}?lyrics=null`,
//! taking the last `downloadUrl` entry, and downloads the audio to
//! `./temp/{song_id}.mp3`.
//!
//! Python's `prepare_song` returns a dict that carries either the
//! song fields or an `"error"` string; the Rust port returns
//! `Result<PreparedSong, Error>` with the **same error strings**
//! (`"No results found."`, `"Could not identify the song."`,
//! `"Could not find a download link."`, `"Failed to download the
//! song."`, `"API request failed: …"`, `"An unexpected error
//! occurred: …"`).
//!
//! The `requester` argument is the already-formatted Telegram
//! mention: Python builds it lazily via `plugins.inline_mention`
//! (the whole plugins runtime — not part of this crate), so the
//! caller passes the formatted string in.

use crate::tools::DOWNLOADS_DIR;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::{error, warn};
use ultroid_core::error::{Error, Result};

/// MeloAPI base URL.
pub const MELO_API_BASE: &str = "https://meloapi.vercel.app";

/// Fallback cover when a song has no images (mirrors `_saavn.py`).
pub const DEFAULT_IMAGE_URL: &str = "https://storage.googleapis.com/hardstone_img_us/umagic_crop/upload/2024-11-17/5f3acc86-e0bb-40ae-8c56-468a9a83a1a1.jpg";

/// Safely delete a file if it exists (mirrors `cleanup_file`).
pub fn cleanup_file(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!("Error cleaning up file {}: {e}", path.display());
        }
    }
}

/// Format seconds the way `str(datetime.timedelta(seconds=…))` does:
/// `H:MM:SS`, with a `N day(s), ` prefix past 24 h.
pub fn timedelta_str(seconds: u64) -> String {
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    if days == 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if days == 1 {
        format!("1 day, {h}:{m:02}:{s:02}")
    } else {
        format!("{days} days, {h}:{m:02}:{s:02}")
    }
}

/// A prepared song: search + details + downloaded audio.
#[derive(Debug, Clone)]
pub struct PreparedSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub description: Option<String>,
    pub image_url: String,
    /// Duration as a `timedelta` string, e.g. `"0:03:24"`.
    pub duration: String,
    pub path: PathBuf,
    pub requester: String,
}

/// MeloAPI client (port of `MeloAPI`).
pub struct MeloApi {
    client: reqwest::Client,
}

impl Default for MeloApi {
    fn default() -> Self {
        MeloApi::new()
    }
}

impl MeloApi {
    pub fn new() -> Self {
        let _ = std::fs::create_dir_all(DOWNLOADS_DIR);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        MeloApi { client }
    }

    /// Highest-quality image URL: prefer `500x500`, else the first
    /// entry; the default URL when the list is empty.
    pub fn best_image_url(image_list: &[serde_json::Value]) -> String {
        if image_list.is_empty() {
            return DEFAULT_IMAGE_URL.to_string();
        }
        for image in image_list {
            if image.get("quality").and_then(|q| q.as_str()) == Some("500x500") {
                if let Some(url) = image.get("url").and_then(|u| u.as_str()) {
                    return url.to_string();
                }
            }
        }
        image_list[0]
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or(DEFAULT_IMAGE_URL)
            .to_string()
    }

    /// Download a file to `./temp/{filename}` in 8 KiB chunks.
    pub async fn download_file(&self, url: &str, filename: &str) -> Option<PathBuf> {
        let file_path = Path::new(DOWNLOADS_DIR).join(filename);
        let mut resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to download {url}: {e}");
                return None;
            }
        };
        resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to download {url}: {e}");
                return None;
            }
        };
        let mut file = match tokio::fs::File::create(&file_path).await {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to create {}: {e}", file_path.display());
                return None;
            }
        };
        loop {
            match resp.chunk().await {
                Ok(Some(bytes)) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        error!("Failed to write {}: {e}", file_path.display());
                        return None;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    error!("Failed to download {url}: {e}");
                    return None;
                }
            }
        }
        if let Err(e) = file.flush().await {
            error!("Failed to flush {}: {e}", file_path.display());
            return None;
        }
        Some(file_path)
    }

    /// Search, fetch details, download — the full `prepare_song`
    /// flow. The `requester` is the caller's formatted mention.
    pub async fn prepare_song(&self, requester: &str, query: &str) -> Result<PreparedSong> {
        let search_url = format!("{MELO_API_BASE}/api/search");
        let resp = self
            .client
            .get(&search_url)
            .query(&[("query", query)])
            .send()
            .await
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let resp = resp
            .error_for_status()
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let song_data = data
            .get("data")
            .and_then(|d| d.get("songs"))
            .and_then(|s| s.get("results"))
            .and_then(|r| r.as_array())
            .and_then(|r| r.first())
            .cloned();
        let Some(song_data) = song_data else {
            return Err(Error::Other("No results found.".to_string()));
        };
        let song_id = song_data
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();
        if song_id.is_empty() {
            return Err(Error::Other("Could not identify the song.".to_string()));
        }

        let details_url = format!("{MELO_API_BASE}/api/songs/{song_id}");
        let resp = self
            .client
            .get(&details_url)
            .query(&[("lyrics", "null")])
            .send()
            .await
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let resp = resp
            .error_for_status()
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let details_data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("API request failed: {e}")))?;
        let mut download_url: Option<String> = None;
        if details_data
            .get("success")
            .and_then(|s| s.as_bool())
            .unwrap_or(false)
        {
            if let Some(song_info) = details_data
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
            {
                if let Some(urls) = song_info.get("downloadUrl").and_then(|u| u.as_array()) {
                    download_url = urls
                        .last()
                        .and_then(|u| u.get("url"))
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string());
                }
            }
        }
        let Some(download_url) = download_url else {
            return Err(Error::Other("Could not find a download link.".to_string()));
        };

        let song_title = song_data
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("Unknown Title")
            .replace('/', "-");
        let audio_path = match self
            .download_file(&download_url, &format!("{song_id}.mp3"))
            .await
        {
            Some(p) => p,
            None => return Err(Error::Other("Failed to download the song.".to_string())),
        };
        let images = song_data
            .get("image")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(PreparedSong {
            id: song_id,
            title: song_title,
            artist: song_data
                .get("primaryArtists")
                .and_then(|a| a.as_str())
                .unwrap_or("Unknown Artist")
                .to_string(),
            description: song_data
                .get("description")
                .and_then(|d| d.as_str())
                .map(|s| s.to_string()),
            image_url: Self::best_image_url(&images),
            duration: timedelta_str(
                song_data
                    .get("duration")
                    .and_then(|d| d.as_u64())
                    .unwrap_or(0),
            ),
            path: audio_path,
            requester: requester.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timedelta_str_matches_python() {
        assert_eq!(timedelta_str(0), "0:00:00");
        assert_eq!(timedelta_str(204), "0:03:24");
        assert_eq!(timedelta_str(3723), "1:02:03");
        assert_eq!(timedelta_str(86_400), "1 day, 0:00:00");
        assert_eq!(timedelta_str(2 * 86_400 + 3661), "2 days, 1:01:01");
    }

    #[test]
    fn best_image_url_prefers_500x500() {
        let images = serde_json::json!([
            {"quality": "150x150", "url": "https://x/small.jpg"},
            {"quality": "500x500", "url": "https://x/big.jpg"}
        ]);
        let list = images.as_array().unwrap();
        assert_eq!(MeloApi::best_image_url(list), "https://x/big.jpg");
        assert_eq!(MeloApi::best_image_url(&[]), DEFAULT_IMAGE_URL);
        let only_small = serde_json::json!([{"quality": "150x150", "url": "https://x/small.jpg"}]);
        assert_eq!(
            MeloApi::best_image_url(only_small.as_array().unwrap()),
            "https://x/small.jpg"
        );
    }
}
