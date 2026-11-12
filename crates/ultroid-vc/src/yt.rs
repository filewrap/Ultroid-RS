//! Small YouTube helper. Port of `vc/_yt.py` (99 LOC).
//!
//! `YouTubeApi` talks to the `honox-five.vercel.app` proxy that the
//! Python code uses: `GET /search?q=…` returns
//! `{"results": [...]}`, `GET /download?id=…` returns the download
//! metadata (including `downloadUrl`), and `download` streams the
//! file to `./temp/{cleaned-title}.mp3`.

use crate::tools::DOWNLOADS_DIR;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tracing::info;
use ultroid_core::error::{Error, Result};

/// Base URL of the proxy (mirrors `YouTubeAPI.base_url`).
pub const BASE_URL: &str = "https://honox-five.vercel.app";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

/// Clean a filename the way Python does: keep alphanumerics and
/// ` `, `.`, `_`; strip trailing whitespace.
pub fn clean_filename(filename: &str) -> String {
    filename
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_'))
        .collect::<String>()
        .trim_end()
        .to_string()
}

pub struct YouTubeApi {
    pub base_url: String,
    client: reqwest::Client,
}

impl Default for YouTubeApi {
    fn default() -> Self {
        YouTubeApi::new()
    }
}

impl YouTubeApi {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        YouTubeApi {
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

    /// Search for videos; returns the `results` list.
    pub async fn search(&self, query: &str) -> Result<Vec<serde_json::Value>> {
        let resp = self
            .client
            .get(format!("{}/search", self.base_url))
            .headers(self.headers())
            .query(&[("q", query)])
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let data: serde_json::Value = resp.json().await?;
        Ok(data
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Download metadata + direct URL for a video id.
    pub async fn get_download_info(&self, video_id: &str) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(format!("{}/download", self.base_url))
            .headers(self.headers())
            .query(&[("id", video_id)])
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }

    /// Stream the audio from `download_url` to
    /// `{output_folder}/{cleaned filename}.mp3` (no timeout, as in
    /// Python — `timeout=None`).
    pub async fn download(
        &self,
        download_url: &str,
        filename: &str,
        output_folder: &Path,
    ) -> Result<PathBuf> {
        tokio::fs::create_dir_all(output_folder).await?;
        let clean_name = clean_filename(filename);
        let file_path = output_folder.join(format!("{clean_name}.mp3"));
        info!("[Status] Starting download: {filename}...");
        let mut resp = self
            .client
            .get(download_url)
            .headers(self.headers())
            .send()
            .await?;
        resp = resp.error_for_status()?;
        let mut file = tokio::fs::File::create(&file_path).await?;
        loop {
            match resp.chunk().await {
                Ok(Some(bytes)) => {
                    file.write_all(&bytes).await?;
                }
                Ok(None) => break,
                Err(e) => return Err(Error::Http(e)),
            }
        }
        file.flush().await?;
        info!("[Success] Downloaded to: {}", file_path.display());
        Ok(file_path)
    }

    /// Search for the first result and download it immediately
    /// (mirrors `quick_download`).
    pub async fn quick_download(&self, query: &str) -> Option<PathBuf> {
        let results = match self.search(query).await {
            Ok(r) => r,
            Err(e) => {
                info!("Search failed for {query:?}: {e}");
                return None;
            }
        };
        let Some(video) = results.into_iter().next() else {
            info!("No results found.");
            return None;
        };
        let video_id = video.get("id")?.as_str()?.to_string();
        let title = video
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let info = match self.get_download_info(&video_id).await {
            Ok(i) => i,
            Err(e) => {
                info!("Failed to retrieve download info for {video_id}: {e}");
                return None;
            }
        };
        let dl_url = info.get("downloadUrl")?.as_str()?.to_string();
        match self
            .download(&dl_url, &title, Path::new(DOWNLOADS_DIR))
            .await
        {
            Ok(p) => Some(p),
            Err(e) => {
                info!("Failed to retrieve download URL: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_filename_keeps_allowed_chars() {
        // '(' , ')' and '-' are dropped; the spaces around them stay.
        assert_eq!(
            clean_filename("Mehrama (2020) - Love Aaj Kal"),
            "Mehrama 2020  Love Aaj Kal"
        );
        assert_eq!(clean_filename("a/b:c*d?e\"f<g>h|i"), "abcdefghi");
        assert_eq!(clean_filename("  title  "), "  title");
        assert_eq!(clean_filename("name_1.2"), "name_1.2");
    }
}
