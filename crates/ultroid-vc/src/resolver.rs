//! Cookieless, PO-token-free audio stream resolution.
//! Port of `vc/_resolver.py` (252 LOC).
//!
//! Two independent implementations are tried in turn, so an upstream
//! break in one does not take the other down:
//!
//! 1. **yt-dlp** with a PO-token-free client (`tv`, `visionos`,
//!    `web_embedded`, `tv_downgraded`) — driven as the `yt-dlp`
//!    binary with `-J` (the Python code uses the `yt_dlp` library;
//!    Rust has no such library, so the binary with the equivalent
//!    flags is the faithful route), format
//!    `bestaudio[protocol^=http]/bestaudio/b`, extractor arg
//!    `youtube:player_client=<client>`, no cookie jar.
//! 2. **pytubefix** with a PO-token-free client (`TV`, `VISION_OS`,
//!    `TV_EMBED`, `TV_DOWNGRADED`) — reimplemented as a direct
//!    InnerTube `player` call with the **exact client contexts from
//!    pytubefix 11.2.1** (`innertube.py`: clientName/clientVersion,
//!    User-Agent, `X-Youtube-Client-Name`; pytubefix stores an
//!    `api_key` per client but never sends it — its `base_params`
//!    is just `prettyPrint=false` — so neither does this port).
//!    Formats are picked like `streams.filter(only_audio=True)
//!    .order_by("abr").desc()`: audio-only MIME types, highest
//!    bitrate first; SABR entries (no URL, no signature cipher —
//!    they resolve to `serverAbrStreamingUrl`) are skipped.
//!
//! Honest limitation (documented, not stubbed): formats that carry
//! only a `signatureCipher` need their `s` deciphered with the
//! cipher extracted from YouTube's `base.js`
//! (`pytubefix/extract.py::apply_descrambler` + the JS-player
//! decipher). This port skips those formats and reports it when no
//! direct-URL format exists, instead of pretending the cipher was
//! solved. For the PO-token-free TV/visionOS clients the player
//! responses normally carry direct URLs, so this rarely bites.
//!
//! A TLS failure aborts the chain immediately (mirrors Python:
//! every client fails the same way, so continuing only multiplies
//! the wait).

use crate::diagnose::{FailureKind, classify_failure, summarise_attempts};
use crate::query::youtube_id;
use crate::youtube::ytdlp_json;
use regex::Regex;
use std::fmt;
use std::sync::OnceLock;
use tracing::debug;
use ultroid_core::error::Error;

/// yt-dlp clients that do NOT require a GVS PO token, best first
/// (mirrors `_resolver.py`; order matters).
pub const YTDLP_CLIENTS: &[&str] = &["tv", "visionos", "web_embedded", "tv_downgraded"];

/// pytubefix client equivalents (different naming, same idea).
pub const PYTUBEFIX_CLIENTS: &[&str] = &["TV", "VISION_OS", "TV_EMBED", "TV_DOWNGRADED"];

/// googlevideo URLs 403 without matching headers on several code
/// paths; these must be passed through to the player as stream
/// headers.
pub const STREAM_USER_AGENT: &str =
    "com.google.android.youtube/19.09.37 (Linux; U; Android 11) gzip";

/// SABR protocol markers.
const SABR_MARKERS: &[&str] = &["sabr", "ump", "/sb/"];

/// Every resolver in the chain failed for this URL (mirrors
/// `ExtractionError`).
#[derive(Debug, Clone)]
pub struct ExtractionError(pub String);

impl fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ExtractionError {}

impl From<ExtractionError> for Error {
    fn from(e: ExtractionError) -> Self {
        Error::Other(e.0)
    }
}

/// A resolved, ffmpeg-consumable audio stream.
#[derive(Debug, Clone)]
pub struct AudioStream {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub title: String,
    pub duration: u64,
    pub source: String,
    pub ext: String,
}

impl AudioStream {
    pub fn stream_headers() -> Vec<(String, String)> {
        vec![("User-Agent".to_string(), STREAM_USER_AGENT.to_string())]
    }
}

/// True if *url* looks like a SABR/UMP stream ffmpeg cannot demux.
pub fn is_sabr(url: &str) -> bool {
    if url.is_empty() {
        return true;
    }
    let low = url.to_lowercase();
    SABR_MARKERS.iter().any(|m| low.contains(m))
}

/// Choose the best plain-HTTPS audio format from a yt-dlp info dict.
/// SABR formats are skipped. Prefer a real audio codec, then bitrate
/// (mirrors `_pick_audio`).
pub fn pick_audio(info: &serde_json::Value) -> Option<serde_json::Value> {
    let formats = info.get("formats")?.as_array()?;
    let mut audio: Vec<&serde_json::Value> = formats
        .iter()
        .filter(|f| {
            let url = f.get("url").and_then(|u| u.as_str()).unwrap_or("");
            if url.is_empty() || is_sabr(url) {
                return false;
            }
            match f.get("acodec").and_then(|a| a.as_str()) {
                None | Some("none") => false,
                Some(_) => true,
            }
        })
        .filter(|f| {
            let protocol = f
                .get("protocol")
                .and_then(|p| p.as_str())
                .unwrap_or("https");
            matches!(protocol, "https" | "http")
        })
        .collect();
    // Prefer muxed https audio, then higher bitrate.
    audio.sort_by(|a, b| {
        let pa = a
            .get("protocol")
            .and_then(|p| p.as_str())
            .unwrap_or("https");
        let pb = b
            .get("protocol")
            .and_then(|p| p.as_str())
            .unwrap_or("https");
        let ka = (
            pa != "https",
            -(a.get("abr").and_then(|x| x.as_f64()).unwrap_or(0.0)),
        );
        let kb = (
            pb != "https",
            -(b.get("abr").and_then(|x| x.as_f64()).unwrap_or(0.0)),
        );
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    audio.into_iter().next().cloned()
}

/// Resolve *url* with yt-dlp using a single PO-token-free *client*.
pub async fn ytdlp_resolve(url: &str, client: &str) -> Result<AudioStream, ExtractionError> {
    let info = ytdlp_json(&[
        "--no-playlist",
        "--format",
        "bestaudio[protocol^=http]/bestaudio/b",
        "--extractor-args",
        &format!("youtube:player_client={client}"),
        "--add-header",
        &format!("User-Agent: {STREAM_USER_AGENT}"),
        "-q",
        "--no-warnings",
        url,
    ])
    .await
    .map_err(|e| ExtractionError(format!("yt-dlp failed for {url} (client={client}): {e}")))?;
    if info.is_null() {
        return Err(ExtractionError(format!(
            "yt-dlp returned no info for {url} (client={client})"
        )));
    }
    let mut headers = AudioStream::stream_headers();
    // A single chosen format short-circuits into info['url'];
    // otherwise pick from the format list.
    let direct = info
        .get("url")
        .and_then(|u| u.as_str())
        .filter(|u| !is_sabr(u));
    let (stream_url, ext, chosen_headers): (String, String, Option<serde_json::Value>) =
        match direct {
            Some(direct_url) => (
                direct_url.to_string(),
                info.get("ext")
                    .and_then(|e| e.as_str())
                    .unwrap_or("m4a")
                    .to_string(),
                info.get("http_headers").cloned(),
            ),
            None => {
                let Some(chosen) = pick_audio(&info) else {
                    return Err(ExtractionError(format!(
                        "no ffmpeg-readable audio format for {url} (client={client}); SABR-only or token-gated"
                    )));
                };
                (
                    chosen
                        .get("url")
                        .and_then(|u| u.as_str())
                        .unwrap_or("")
                        .to_string(),
                    chosen
                        .get("ext")
                        .and_then(|e| e.as_str())
                        .unwrap_or("m4a")
                        .to_string(),
                    chosen.get("http_headers").cloned(),
                )
            }
        };
    if let Some(h) = chosen_headers.as_ref().and_then(|h| h.as_object()) {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                headers.push((k.clone(), v.to_string()));
            }
        }
    }
    Ok(AudioStream {
        url: stream_url,
        headers,
        title: info
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        duration: info.get("duration").and_then(|d| d.as_u64()).unwrap_or(0),
        source: format!("yt-dlp:{client}"),
        ext,
    })
}

// ── pytubefix tier (InnerTube) ───────────────────────────────────────────

/// One pytubefix client context, verified against pytubefix 11.2.1
/// `innertube.py` (`_default_clients`). `api_key` is stored by
/// pytubefix but never sent (see the module docs), so it is kept
/// here for reference only.
struct InnertubeClient {
    client_name: &'static str,
    client_version: &'static str,
    platform: &'static str,
    device_make: Option<&'static str>,
    os_name: Option<&'static str>,
    os_version: Option<&'static str>,
    device_model: Option<&'static str>,
    client_screen: Option<&'static str>,
    user_agent: &'static str,
    client_id_header: &'static str,
    #[allow(dead_code)]
    api_key: &'static str,
    require_js_player: bool,
}

fn innertube_client(name: &str) -> Option<InnertubeClient> {
    match name {
        "TV" => Some(InnertubeClient {
            client_name: "TVHTML5",
            client_version: "7.20260707.07.00",
            platform: "TV",
            device_make: None,
            os_name: None,
            os_version: None,
            device_model: None,
            client_screen: None,
            user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)",
            client_id_header: "7",
            api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8",
            require_js_player: true,
        }),
        "VISION_OS" => Some(InnertubeClient {
            client_name: "VISIONOS",
            client_version: "1.02",
            platform: "MOBILE",
            device_make: Some("Apple"),
            os_name: Some("visionOS"),
            os_version: Some("26.5.23O471"),
            device_model: Some("RealityDevice17,1"),
            client_screen: None,
            user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
            client_id_header: "101",
            api_key: "AIzaSyB-63vPrdThhKuerbB2N_l7Kwwcxj6yUAc",
            require_js_player: false,
        }),
        "TV_EMBED" => Some(InnertubeClient {
            client_name: "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
            client_version: "2.0",
            platform: "TV",
            device_make: None,
            os_name: None,
            os_version: None,
            device_model: None,
            client_screen: Some("EMBED"),
            user_agent: "Mozilla/5.0",
            client_id_header: "85",
            api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8",
            require_js_player: true,
        }),
        "TV_DOWNGRADED" => Some(InnertubeClient {
            client_name: "TVHTML5",
            client_version: "5.20260707",
            platform: "TV",
            device_make: None,
            os_name: None,
            os_version: None,
            device_model: None,
            client_screen: None,
            user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version",
            client_id_header: "7",
            api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8",
            require_js_player: true,
        }),
        _ => None,
    }
}

fn js_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"jsUrl":"([^"]+)""#).expect("static jsUrl regex"))
}

fn signature_timestamp_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"signatureTimestamp:(\d+)").expect("static signatureTimestamp regex")
    })
}

/// Fetch the player-JS signature timestamp the way pytubefix does
/// (`YouTube.signature_timestamp` → `extract.signature_timestamp`
/// over `base.js`): watch page → `jsUrl` → `base.js` →
/// `signatureTimestamp:(\d+)`. Best-effort — `None` when any step
/// fails, and the player call proceeds without it.
async fn fetch_signature_timestamp(
    http: &reqwest::Client,
    video_id: &str,
    user_agent: &str,
) -> Option<String> {
    let watch = http
        .get(format!("https://www.youtube.com/watch?v={video_id}"))
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await
        .ok()?;
    let html = watch.text().await.ok()?;
    let js_path = js_url_re().captures(&html)?.get(1)?.as_str().to_string();
    let js = http
        .get(format!("https://www.youtube.com{js_path}"))
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await
        .ok()?;
    let js = js.text().await.ok()?;
    signature_timestamp_re()
        .captures(&js)?
        .get(1)
        .map(|m| m.as_str().to_string())
}

/// Resolve *url* with pytubefix using a single PO-token-free
/// *client* — a direct InnerTube `player` call (see module docs).
pub async fn pytubefix_resolve(url: &str, client: &str) -> Result<AudioStream, ExtractionError> {
    let video_id = youtube_id(url).ok_or_else(|| {
        ExtractionError(format!(
            "pytubefix: could not extract a video id from {url} (client={client})"
        ))
    })?;
    let spec = innertube_client(client)
        .ok_or_else(|| ExtractionError(format!("pytubefix: unknown client {client}")))?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| ExtractionError(format!("pytubefix: client build failed: {e}")))?;

    let mut client_ctx = serde_json::json!({
        "clientName": spec.client_name,
        "clientVersion": spec.client_version,
        "platform": spec.platform,
    });
    if let Some(v) = spec.device_make {
        client_ctx["deviceMake"] = serde_json::Value::String(v.to_string());
    }
    if let Some(v) = spec.os_name {
        client_ctx["osName"] = serde_json::Value::String(v.to_string());
    }
    if let Some(v) = spec.os_version {
        client_ctx["osVersion"] = serde_json::Value::String(v.to_string());
    }
    if let Some(v) = spec.device_model {
        client_ctx["deviceModel"] = serde_json::Value::String(v.to_string());
    }
    if let Some(v) = spec.client_screen {
        client_ctx["clientScreen"] = serde_json::Value::String(v.to_string());
    }
    let mut body = serde_json::json!({
        "context": { "client": client_ctx },
        "videoId": video_id,
        "contentCheckOk": "true",
    });
    if spec.require_js_player {
        if let Some(sts) = fetch_signature_timestamp(&http, &video_id, spec.user_agent).await {
            body["playbackContext"] = serde_json::json!({
                "contentPlaybackContext": { "signatureTimestamp": sts }
            });
        } else {
            debug!("pytubefix/{client}: no signatureTimestamp, continuing without it");
        }
    }

    let resp = http
        .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, spec.user_agent)
        .header("X-Youtube-Client-Name", spec.client_id_header)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            ExtractionError(format!(
                "pytubefix request failed for {url} (client={client}): {e}"
            ))
        })?;
    let player: serde_json::Value = resp
        .error_for_status()
        .map_err(|e| {
            ExtractionError(format!(
                "pytubefix HTTP error for {url} (client={client}): {e}"
            ))
        })?
        .json()
        .await
        .map_err(|e| {
            ExtractionError(format!(
                "pytubefix bad JSON for {url} (client={client}): {e}"
            ))
        })?;

    let status = player
        .get("playabilityStatus")
        .and_then(|p| p.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if status != "OK" {
        let reason = player
            .get("playabilityStatus")
            .and_then(|p| p.get("reason"))
            .and_then(|r| r.as_str())
            .unwrap_or(status);
        return Err(ExtractionError(format!(
            "pytubefix found no audio for {url} (client={client}): playability {reason}"
        )));
    }
    let title = player
        .get("videoDetails")
        .and_then(|d| d.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    // Merge formats + adaptiveFormats; skip SABR (no url, no
    // signatureCipher → serverAbrStreamingUrl) and skip
    // signatureCipher-only formats (decipher needs base.js; see
    // module docs). Keep audio-only, highest bitrate first.
    let mut formats: Vec<&serde_json::Value> = Vec::new();
    if let Some(sd) = player.get("streamingData") {
        for key in ["formats", "adaptiveFormats"] {
            if let Some(arr) = sd.get(key).and_then(|a| a.as_array()) {
                formats.extend(arr.iter());
            }
        }
    }
    let mut ciphered = 0usize;
    let mut sabr = 0usize;
    let mut candidates: Vec<&serde_json::Value> = Vec::new();
    for f in formats {
        let mime = f.get("mimeType").and_then(|m| m.as_str()).unwrap_or("");
        if !mime.starts_with("audio/") {
            continue;
        }
        match f.get("url").and_then(|u| u.as_str()) {
            Some(u) if !u.is_empty() => {
                if is_sabr(u) {
                    sabr += 1;
                    continue;
                }
                candidates.push(f);
            }
            _ => {
                if f.get("signatureCipher").is_some() {
                    ciphered += 1;
                } else {
                    sabr += 1;
                }
            }
        }
    }
    candidates.sort_by(|a, b| {
        let ba = a.get("bitrate").and_then(|x| x.as_u64()).unwrap_or(0);
        let bb = b.get("bitrate").and_then(|x| x.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    let Some(chosen) = candidates.into_iter().next() else {
        if ciphered > 0 && sabr == 0 {
            return Err(ExtractionError(format!(
                "pytubefix: all audio for {url} is signature-ciphered (client={client}); signature deciphering is not implemented in this port"
            )));
        }
        return Err(ExtractionError(format!(
            "pytubefix: all audio for {url} is SABR (client={client})"
        )));
    };
    let stream_url = chosen
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    Ok(AudioStream {
        url: stream_url,
        headers: AudioStream::stream_headers(),
        title,
        // NOTE: Python reads `stream.length`, but pytubefix's
        // `Stream` has no `length` attribute, so `getattr(stream,
        // "length", 0)` is always 0. Mirrored exactly.
        duration: 0,
        source: format!("pytubefix:{client}"),
        ext: "m4a".to_string(),
    })
}

/// Resolve a direct audio stream, trying each tier in order.
///
/// A TLS failure aborts the chain immediately (mirrors Python).
pub async fn resolve_audio(url: &str) -> Result<AudioStream, ExtractionError> {
    let mut errors: Vec<String> = Vec::new();
    for client in YTDLP_CLIENTS {
        match ytdlp_resolve(url, client).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                debug!("yt-dlp client {client} failed for {url}: {e}");
                errors.push(format!("yt-dlp/{client}: {e}"));
            }
        }
        if classify_failure(errors.last().unwrap()) == FailureKind::Tls {
            return Err(ExtractionError(format!(
                "no cookieless audio stream for {url} - TLS connection to YouTube is intercepted or blocked ({client} failed). Tried so far: {}",
                summarise_attempts(&errors.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            )));
        }
    }
    for client in PYTUBEFIX_CLIENTS {
        match pytubefix_resolve(url, client).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                debug!("pytubefix client {client} failed for {url}: {e}");
                errors.push(format!("pytubefix/{client}: {e}"));
            }
        }
        if classify_failure(errors.last().unwrap()) == FailureKind::Tls {
            return Err(ExtractionError(format!(
                "no cookieless audio stream for {url} - TLS connection to YouTube is intercepted or blocked ({client} failed). Tried so far: {}",
                summarise_attempts(&errors.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            )));
        }
    }
    Err(ExtractionError(format!(
        "no cookieless audio stream for {url}. Tried: {}",
        errors.join(" | ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_sabr_markers() {
        assert!(is_sabr(""));
        assert!(is_sabr("https://x/sabr/abc"));
        assert!(is_sabr("https://x/ump/abc"));
        assert!(is_sabr("https://x/sb/abc"));
        assert!(is_sabr("https://SABR.example.com/a"));
        assert!(!is_sabr(
            "https://rr1---sn.googlevideo.com/videoplayback?expire=1"
        ));
    }

    #[test]
    fn pick_audio_prefers_https_then_bitrate() {
        let info = serde_json::json!({
            "formats": [
                {"url": "https://x/low", "acodec": "mp4a.40.2", "protocol": "https", "abr": 48.0, "ext": "m4a"},
                {"url": "https://x/high", "acodec": "opus", "protocol": "https", "abr": 160.0, "ext": "webm"},
                {"url": "https://x/sabr", "acodec": "opus", "protocol": "https", "abr": 999.0, "ext": "webm"},
                {"url": "https://x/nocodec", "acodec": "none", "protocol": "https", "abr": 999.0, "ext": "mp4"},
                {"url": "http://x/http", "acodec": "mp4a.40.2", "protocol": "http", "abr": 999.0, "ext": "m4a"}
            ]
        });
        let picked = pick_audio(&info).unwrap();
        assert_eq!(picked["url"], "https://x/high");
    }

    #[test]
    fn pick_audio_returns_none_when_only_sabr() {
        let info = serde_json::json!({
            "formats": [
                {"url": "https://x/sabr/1", "acodec": "opus", "protocol": "https", "abr": 160.0}
            ]
        });
        assert!(pick_audio(&info).is_none());
    }

    #[test]
    fn client_tables_are_complete() {
        for client in YTDLP_CLIENTS {
            assert!(!client.is_empty());
        }
        for client in PYTUBEFIX_CLIENTS {
            let spec = innertube_client(client).unwrap();
            assert!(!spec.client_name.is_empty());
            assert!(!spec.client_version.is_empty());
            assert!(!spec.user_agent.is_empty());
        }
        assert!(innertube_client("WEB").is_none());
    }
}
