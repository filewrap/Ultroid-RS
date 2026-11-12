//! HTTP client wrapper. Port of `vc/_httpx.py`.
//!
//! `HttpxClient` mirrors the Python class exactly: 10 s request
//! timeout, 60 s download timeout, no redirects by default
//! (`max_redirects=0` → `follow_redirects=False`), 8 KiB streaming
//! chunks, `MAX_RETRIES = 2` attempts with `BACKOFF_FACTOR = 1.0`
//! exponential backoff on `make_request`, and the `X-API-Key` header
//! on any URL that starts with the configured music API URL
//! (`MUSIC_API_URL` / `MUSIC_API_KEY` env vars, mirroring
//! `_tools.py`).
//!
//! Python's `httpx` errors map onto reqwest like this:
//! `TooManyRedirects` → redirect-policy exhaustion, `HTTPStatusError`
//! → a non-2xx status, `RequestError` → any transport failure.

use crate::tools::DOWNLOADS_DIR;
use regex::Regex;
use reqwest::redirect::Policy;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{error, warn};
use ultroid_core::error::Result as CoreResult;

/// Default timeout for general HTTP requests (seconds).
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;
/// Default timeout for file downloads (seconds).
pub const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 60;
/// Streaming chunk size (8 KiB).
pub const CHUNK_SIZE: usize = 8192;
/// Maximum attempts for `make_request`.
pub const MAX_RETRIES: usize = 2;
/// Base delay for exponential backoff (seconds).
pub const BACKOFF_FACTOR: f64 = 1.0;

/// Music API base URL (`MUSIC_API_URL`, mirroring `_tools.py`).
pub fn api_url() -> String {
    std::env::var("MUSIC_API_URL").unwrap_or_default()
}

/// Music API key (`MUSIC_API_KEY`, mirroring `_tools.py`).
pub fn api_key() -> String {
    std::env::var("MUSIC_API_KEY").unwrap_or_default()
}

fn content_disposition_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"filename="?([^"]+)"?"#).expect("static filename regex"))
}

/// Percent-decode (`urllib.parse.unquote`: `%XX` only, `+` stays `+`).
pub fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Result of a file download, mirroring the Python `DownloadResult`.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub success: bool,
    pub file_path: Option<PathBuf>,
    pub error: Option<String>,
}

impl DownloadResult {
    pub fn ok(path: PathBuf) -> Self {
        DownloadResult {
            success: true,
            file_path: Some(path),
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        DownloadResult {
            success: false,
            file_path: None,
            error: Some(error.into()),
        }
    }
}

/// The HTTP client wrapper.
#[derive(Debug, Clone)]
pub struct HttpxClient {
    client: reqwest::Client,
    timeout: Duration,
    download_timeout: Duration,
}

impl Default for HttpxClient {
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT_SECS, DEFAULT_DOWNLOAD_TIMEOUT_SECS, 0)
    }
}

impl HttpxClient {
    pub fn new(timeout_secs: u64, download_timeout_secs: u64, max_redirects: usize) -> Self {
        let redirect = if max_redirects > 0 {
            Policy::limited(max_redirects)
        } else {
            Policy::none()
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .redirect(redirect)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        HttpxClient {
            client,
            timeout: Duration::from_secs(timeout_secs),
            download_timeout: Duration::from_secs(download_timeout_secs),
        }
    }

    /// The underlying reqwest client (for calls that need headers/params
    /// beyond `make_request`).
    pub fn inner(&self) -> &reqwest::Client {
        &self.client
    }

    /// Extra headers for a URL: `X-API-Key` when it starts with the
    /// configured music API URL.
    fn api_headers(&self, url: &str) -> Vec<(String, String)> {
        let base = api_url();
        if !base.is_empty() && url.starts_with(&base) {
            vec![("X-API-Key".to_string(), api_key())]
        } else {
            Vec::new()
        }
    }

    /// Download a file, streaming it in 8 KiB chunks.
    ///
    /// When `file_path` is `None`, the filename comes from the
    /// `Content-Disposition` header, else the last URL segment, else a
    /// random hex name — all under `DOWNLOADS_DIR`, mirroring Python.
    /// An existing file is returned as-is unless `overwrite` is set.
    pub async fn download_file(
        &self,
        url: &str,
        file_path: Option<&Path>,
        overwrite: bool,
    ) -> DownloadResult {
        if url.is_empty() {
            return DownloadResult::failed("Empty URL provided");
        }
        let mut req = self.client.get(url).timeout(self.download_timeout);
        for (k, v) in self.api_headers(url) {
            req = req.header(k, v);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = handle_http_error(&e, url);
                error!("{msg}");
                return DownloadResult::failed(msg);
            }
        };
        let resp = match resp.error_for_status() {
            Ok(r) => r,
            Err(e) => {
                let msg = handle_http_error(&e, url);
                error!("{msg}");
                return DownloadResult::failed(msg);
            }
        };

        let path = match file_path {
            Some(p) => p.to_path_buf(),
            None => {
                let cd = resp
                    .headers()
                    .get(reqwest::header::CONTENT_DISPOSITION)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let filename = match content_disposition_re().captures(&cd) {
                    Some(c) => unquote(&c[1]),
                    None => {
                        let name = url
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .split(['?', '#'])
                            .next()
                            .unwrap_or("");
                        if name.is_empty() {
                            format!("{:x}", rand::random::<u64>())
                        } else {
                            name.to_string()
                        }
                    }
                };
                Path::new(DOWNLOADS_DIR).join(filename)
            }
        };

        if path.exists() && !overwrite {
            return DownloadResult::ok(path);
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                let msg = format!("could not create {}: {e}", parent.display());
                error!("{msg}");
                return DownloadResult::failed(msg);
            }
        }
        let mut file = match tokio::fs::File::create(&path).await {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("could not create {}: {e}", path.display());
                error!("{msg}");
                return DownloadResult::failed(msg);
            }
        };
        let mut stream = resp;
        loop {
            match stream.chunk().await {
                Ok(Some(bytes)) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        let msg = format!("write to {} failed: {e}", path.display());
                        error!("{msg}");
                        return DownloadResult::failed(msg);
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let msg = handle_http_error(&e, url);
                    error!("{msg}");
                    return DownloadResult::failed(msg);
                }
            }
        }
        if let Err(e) = file.flush().await {
            let msg = format!("flush {} failed: {e}", path.display());
            error!("{msg}");
            return DownloadResult::failed(msg);
        }
        tracing::debug!("Successfully downloaded file to {}", path.display());
        DownloadResult::ok(path)
    }

    /// GET `url` with retries and exponential backoff, returning the
    /// parsed JSON body, or `None` on failure (mirrors Python, which
    /// logs and returns `None`).
    pub async fn make_request(
        &self,
        url: &str,
        params: Option<&[(&str, &str)]>,
        max_retries: usize,
        backoff_factor: f64,
    ) -> Option<serde_json::Value> {
        if url.is_empty() {
            warn!("Empty URL provided");
            return None;
        }
        let max_retries = max_retries.max(1);
        for attempt in 0..max_retries {
            let mut req = self.client.get(url).timeout(self.timeout);
            if let Some(p) = params {
                req = req.query(p);
            }
            for (k, v) in self.api_headers(url) {
                req = req.header(k, v);
            }
            match req.send().await {
                Ok(resp) => match resp.error_for_status() {
                    Ok(resp) => match resp.json::<serde_json::Value>().await {
                        Ok(json) => return Some(json),
                        Err(e) => {
                            error!("Invalid JSON response from {url}: {e}");
                            return None;
                        }
                    },
                    Err(e) => {
                        let msg = handle_http_error(&e, url);
                        if attempt + 1 == max_retries {
                            error!("{msg}");
                            return None;
                        }
                        warn!("{msg}");
                    }
                },
                Err(e) => {
                    let msg = handle_http_error(&e, url);
                    if attempt + 1 == max_retries {
                        error!("{msg}");
                        return None;
                    }
                    warn!("{msg}");
                }
            }
            // Exponential backoff: backoff_factor * 2^attempt.
            let delay = Duration::from_secs_f64(backoff_factor * 2f64.powi(attempt as i32));
            tokio::time::sleep(delay).await;
        }
        None
    }

    /// `make_request` with the Python defaults (`MAX_RETRIES`,
    /// `BACKOFF_FACTOR`).
    pub async fn make_request_default(
        &self,
        url: &str,
        params: Option<&[(&str, &str)]>,
    ) -> Option<serde_json::Value> {
        self.make_request(url, params, MAX_RETRIES, BACKOFF_FACTOR)
            .await
    }
}

/// Mirror of the Python `_handle_http_error` messages.
pub fn handle_http_error(e: &reqwest::Error, url: &str) -> String {
    if e.is_redirect() {
        format!("Too many redirects for {url}: {e}")
    } else if let Some(status) = e.status() {
        format!("HTTP error {status} for {url}")
    } else if e.is_timeout() || e.is_connect() || e.is_body() || e.is_decode() {
        format!("Request failed for {url}: {e}")
    } else {
        format!("Unexpected error for {url}: {e}")
    }
}

/// Convenience: build a default client (mirrors `HttpxClient()`).
pub fn default_client() -> CoreResult<HttpxClient> {
    Ok(HttpxClient::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_decodes_percent_only() {
        assert_eq!(unquote("a%20b+c%2Fd"), "a b+c/d");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("%"), "%");
    }

    #[test]
    fn api_headers_only_for_api_url() {
        // Without MUSIC_API_URL configured, no header is ever added.
        let client = HttpxClient::default();
        assert!(client.api_headers("https://example.com/x").is_empty());
    }

    #[test]
    fn download_result_shapes() {
        let ok = DownloadResult::ok(PathBuf::from("/tmp/x.mp3"));
        assert!(ok.success);
        assert_eq!(ok.file_path.as_deref(), Some(Path::new("/tmp/x.mp3")));
        let bad = DownloadResult::failed("Empty URL provided");
        assert!(!bad.success);
        assert_eq!(bad.error.as_deref(), Some("Empty URL provided"));
    }
}
