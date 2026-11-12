//! Media probing + ntgcalls `MediaDescription` construction.
//! Port of `vc/_tools.py` (ffprobe probing) + `vc/_pytgcalls.py`
//! stream params + `pytgcalls/types/stream/{media_stream,audio_quality,
//! video_quality}.py` + `methods/utilities/stream_params.py`.
//!
//! Audio quality matrix (verified against py-tgcalls source):
//! STUDIO (96000 Hz, 2 ch), HIGH (48000, 2), MEDIUM (36000, 1),
//! LOW (24000, 1). Video: UHD_4K (3840×2160, 60 fps), QHD_2K
//! (2560×1440, 60), FHD_1080p (1920×1080, 60), HD_720p (1280×720, 30),
//! SD_480p (854×480, 30), SD_360p (640×360, 30).
//!
//! `MediaDescription` mapping mirrors `_parse_media_description`:
//! `AudioDescription { media_source, input = path, sample_rate, channel_count,
//! keep_open: false }`, `VideoDescription { …, width, height, fps }`.

use ntgcalls::{AudioDescription, MediaDescription, MediaSource, VideoDescription};
use serde::Deserialize;
use std::path::Path;
use ultroid_core::error::{Error, Result};

/// Audio sample rates / channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioQuality {
    Studio,
    High,
    Medium,
    Low,
}

impl AudioQuality {
    pub fn params(self) -> (u32, u8) {
        match self {
            AudioQuality::Studio => (96_000, 2),
            AudioQuality::High => (48_000, 2),
            AudioQuality::Medium => (36_000, 1),
            AudioQuality::Low => (24_000, 1),
        }
    }
}

/// Video resolution / frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoQuality {
    Uhd4k,
    Qhd2k,
    Fhd1080p,
    Hd720p,
    Sd480p,
    Sd360p,
}

impl VideoQuality {
    pub fn params(self) -> (i16, i16, u8) {
        match self {
            VideoQuality::Uhd4k => (3840, 2160, 60),
            VideoQuality::Qhd2k => (2560, 1440, 60),
            VideoQuality::Fhd1080p => (1920, 1080, 60),
            VideoQuality::Hd720p => (1280, 720, 30),
            VideoQuality::Sd480p => (854, 480, 30),
            VideoQuality::Sd360p => (640, 360, 30),
        }
    }
}

/// Everything the player needs about one media input.
#[derive(Debug, Clone)]
pub struct MediaStream {
    /// Local path, or a URL (http/https) — ntgcalls/ffmpeg handles it.
    pub input: String,
    pub audio_quality: AudioQuality,
    pub video_quality: Option<VideoQuality>,
    pub headers: Vec<(String, String)>,
    pub keep_open: bool,
    /// Extra ffmpeg arguments (e.g. `-reconnect 1` for live streams).
    pub ffmpeg_params: Vec<String>,
    pub probed: Option<ProbeResult>,
}

impl MediaStream {
    pub fn audio(input: impl Into<String>, quality: AudioQuality) -> Self {
        MediaStream {
            input: input.into(),
            audio_quality: quality,
            video_quality: None,
            headers: Vec::new(),
            keep_open: false,
            ffmpeg_params: Vec::new(),
            probed: None,
        }
    }

    pub fn video(
        input: impl Into<String>,
        audio_quality: AudioQuality,
        video_quality: VideoQuality,
    ) -> Self {
        MediaStream {
            input: input.into(),
            audio_quality,
            video_quality: Some(video_quality),
            headers: Vec::new(),
            keep_open: false,
            ffmpeg_params: Vec::new(),
            probed: None,
        }
    }

    /// `check_stream`: probe with ffprobe (JSON) and fill `probed`.
    /// Local files only — remote URLs are probed lazily by ffmpeg.
    pub async fn check_stream(&mut self) -> Result<()> {
        if self.input.starts_with("http://") || self.input.starts_with("https://") {
            return Ok(());
        }
        let probed = probe_media(Path::new(&self.input)).await?;
        self.probed = Some(probed);
        Ok(())
    }

    /// Build the ntgcalls `MediaDescription` (mirrors
    /// `StreamParams._parse_stream_description`).
    pub fn to_media_description(&self) -> MediaDescription {
        let (sample_rate, channel_count) = self.audio_quality.params();
        let source = if self.input.starts_with("http://") || self.input.starts_with("https://") {
            MediaSource::Ffmpeg
        } else {
            MediaSource::Ffmpeg
        };
        let audio = AudioDescription {
            media_source: source,
            input: self.input.clone(),
            sample_rate,
            channel_count,
            keep_open: self.keep_open,
        };
        let video = self.video_quality.map(|vq| {
            let (width, height, fps) = vq.params();
            VideoDescription {
                media_source: source,
                input: self.input.clone(),
                width,
                height,
                fps,
                keep_open: self.keep_open,
            }
        });
        MediaDescription {
            microphone: Some(audio),
            speaker: None,
            camera: video,
            screen: None,
        }
    }
}

/// ffprobe JSON subset.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProbeResult {
    pub duration_secs: Option<f64>,
    pub has_audio: bool,
    pub has_video: bool,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

#[derive(Deserialize)]
struct FfprobeJson {
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    codec_type: String,
    width: Option<i32>,
    height: Option<i32>,
    sample_rate: Option<String>,
    channels: Option<u8>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

/// Run `ffprobe -v quiet -print_format json -show_format -show_streams`.
pub async fn probe_media(path: &Path) -> Result<ProbeResult> {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| Error::Other(format!("ffprobe failed to run: {e}")))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "ffprobe failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let json: FfprobeJson = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Other(format!("ffprobe JSON parse failed: {e}")))?;
    let mut res = ProbeResult::default();
    res.duration_secs = json
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse::<f64>().ok());
    for s in &json.streams {
        match s.codec_type.as_str() {
            "audio" => {
                res.has_audio = true;
                res.sample_rate = s.sample_rate.as_ref().and_then(|r| r.parse().ok());
                res.channels = s.channels;
            }
            "video" => {
                res.has_video = true;
                res.width = s.width;
                res.height = s.height;
            }
            _ => {}
        }
    }
    Ok(res)
}

/// Duration of a local file in seconds (ffprobe), mirroring
/// `vc/_tools.py:get_audio_duration`.
pub async fn get_audio_duration(path: &Path) -> Result<f64> {
    Ok(probe_media(path).await?.duration_secs.unwrap_or(0.0))
}
