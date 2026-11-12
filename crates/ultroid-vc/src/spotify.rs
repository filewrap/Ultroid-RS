//! Spotify helper. Port of `vc/_spotify.py` (127 LOC).
//!
//! Credentials come from the environment (`SPOTIFY_CLIENT_ID` /
//! `SPOTIFY_CLIENT_SECRET` — the exact names Python reads, via
//! `Var` or env). A missing credential is an `Error::Config`
//! naming the variable, never a silent empty-credential request.
//!
//! * `get_spotify_token` — client-credentials flow against
//!   `https://accounts.spotify.com/api/token`.
//! * `get_track_id` — `track/<id>` from a URL, else a
//!   `https://api.spotify.com/v1/search` lookup.
//! * `get_track_details` — `GET /v1/tracks/{id}`.
//! * `add_mp3_metadata` — ID3 tags (TIT2/TPE1/TALB + APIC cover);
//!   Python writes them with mutagen and swallows every failure,
//!   so the Rust port writes them with `ffmpeg` and likewise
//!   swallows failures (a song that failed tagging can still play).
//!
//! `BASE_MATE` (`https://spotmate.online`) is kept verbatim although
//! Python never uses it in this file.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use ultroid_core::error::{Error, Result};

/// Spotmate base URL (mirrors Python; unused in this file).
pub const BASE_MATE: &str = "https://spotmate.online";
/// Temp directory for downloads (mirrors `TEMP_DIR = "./temp"`).
pub const TEMP_DIR: &str = "./temp";

/// Spotify client id (`SPOTIFY_CLIENT_ID`).
pub fn spotify_client_id() -> String {
    std::env::var("SPOTIFY_CLIENT_ID").unwrap_or_default()
}

/// Spotify client secret (`SPOTIFY_CLIENT_SECRET`).
pub fn spotify_client_secret() -> String {
    std::env::var("SPOTIFY_CLIENT_SECRET").unwrap_or_default()
}

fn track_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"track/([a-zA-Z0-9]+)").expect("static Spotify track regex"))
}

/// Extract a Spotify track id from a URL or a search query.
pub fn extract_track_id(query: &str) -> Option<String> {
    track_id_re().captures(query).map(|c| c[1].to_string())
}

pub struct Spotify {
    client: reqwest::Client,
}

impl Default for Spotify {
    fn default() -> Self {
        Spotify::new()
    }
}

impl Spotify {
    pub fn new() -> Self {
        let _ = std::fs::create_dir_all(TEMP_DIR);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Spotify { client }
    }

    /// Fetch a Spotify API access token (client-credentials flow).
    /// Missing credentials → `Error::Config` naming the variables.
    /// HTTP / JSON failures → `Ok(None)`, mirroring Python (which
    /// returns `None` on `HTTPError`/`KeyError`).
    pub async fn get_spotify_token(&self) -> Result<Option<String>> {
        let id = spotify_client_id();
        let secret = spotify_client_secret();
        if id.is_empty() || secret.is_empty() {
            let missing = if id.is_empty() {
                "SPOTIFY_CLIENT_ID"
            } else {
                "SPOTIFY_CLIENT_SECRET"
            };
            return Err(Error::Config(format!(
                "missing Spotify credential: set {missing} (and SPOTIFY_CLIENT_SECRET)"
            )));
        }
        let form = [("grant_type", "client_credentials")];
        let resp = self
            .client
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(&id, Some(&secret))
            .form(&form)
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        Ok(data
            .get("access_token")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()))
    }

    /// Spotify track id from a URL (`track/<id>`) or, failing that,
    /// the first search result.
    pub async fn get_track_id(&self, query: &str, token: &str) -> Option<String> {
        if let Some(id) = extract_track_id(query) {
            return Some(id);
        }
        let resp = self
            .client
            .get("https://api.spotify.com/v1/search")
            .query(&[("q", query), ("type", "track"), ("limit", "1")])
            .bearer_auth(token)
            .send()
            .await
            .ok()?;
        let resp = resp.error_for_status().ok()?;
        let data: serde_json::Value = resp.json().await.ok()?;
        data.get("tracks")
            .and_then(|t| t.get("items"))
            .and_then(|i| i.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string())
    }

    /// Detailed metadata for a track id.
    pub async fn get_track_details(&self, song_id: &str, token: &str) -> Option<serde_json::Value> {
        let resp = self
            .client
            .get(format!("https://api.spotify.com/v1/tracks/{song_id}"))
            .bearer_auth(token)
            .send()
            .await
            .ok()?;
        let resp = resp.error_for_status().ok()?;
        resp.json().await.ok()
    }

    /// Add ID3 tags (title, artist, album, cover art) to an MP3.
    /// Failures are swallowed (mirrors Python's bare `except: pass`).
    pub fn add_mp3_metadata(
        &self,
        file_path: &Path,
        details: &serde_json::Value,
        cover_path: &Path,
    ) {
        let title = details.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let artist = details
            .get("artists")
            .and_then(|a| a.as_array())
            .map(|artists| {
                artists
                    .iter()
                    .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let album = details
            .get("album")
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("");
        let tmp_path = file_path.with_extension("tagged.mp3");
        let output = std::process::Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(file_path)
            .arg("-i")
            .arg(cover_path)
            .arg("-map")
            .arg("0")
            .arg("-map")
            .arg("1")
            .arg("-c")
            .arg("copy")
            .arg("-id3v2_version")
            .arg("3")
            .arg("-metadata")
            .arg(format!("title={title}"))
            .arg("-metadata")
            .arg(format!("artist={artist}"))
            .arg("-metadata")
            .arg(format!("album={album}"))
            .arg("-metadata:s:v")
            .arg("title=Album cover")
            .arg("-metadata:s:v")
            .arg("comment=Cover (front)")
            .arg(&tmp_path)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let _ = std::fs::rename(&tmp_path, file_path);
            }
            _ => {
                let _ = std::fs::remove_file(&tmp_path);
            }
        }
    }

    /// Delete temporary files (mirrors `cleanup_files`).
    pub fn cleanup_files(&self, files: &[PathBuf]) {
        for file in files {
            if file.exists() {
                let _ = std::fs::remove_file(file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_track_id_from_url() {
        assert_eq!(
            extract_track_id("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC").as_deref(),
            Some("4uLU6hMCjMI75M1A2tKUQC")
        );
        assert_eq!(
            extract_track_id("https://open.spotify.com/album/abc123").as_deref(),
            None
        );
        assert_eq!(extract_track_id("never gonna give you up"), None);
    }

    #[tokio::test]
    async fn missing_credentials_are_config_errors() {
        // The test environment has no Spotify credentials; the token
        // call must name the missing variable, not attempt a request.
        unsafe {
            std::env::remove_var("SPOTIFY_CLIENT_ID");
            std::env::remove_var("SPOTIFY_CLIENT_SECRET");
        }
        let err = Spotify::new().get_spotify_token().await.unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("SPOTIFY_CLIENT_ID"));
    }
}
