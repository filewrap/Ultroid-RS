//! Unit tests for the pure-logic VC modules (no network, no ntgcalls).

use crate::dataclass::CachedTrack;
use crate::diagnose::{FailureKind, classify_failure, diagnose};
use crate::media::{AudioQuality, VideoQuality};
use crate::player::PlayerRegistry;
use crate::query::{QueryKind, classify, youtube_id};
use crate::tools::{ms_to_seconds, sec_to_min};

#[test]
fn audio_quality_params_match_pytgcalls() {
    assert_eq!(AudioQuality::Studio.params(), (96_000, 2));
    assert_eq!(AudioQuality::High.params(), (48_000, 2));
    assert_eq!(AudioQuality::Medium.params(), (36_000, 1));
    assert_eq!(AudioQuality::Low.params(), (24_000, 1));
}

#[test]
fn video_quality_params_match_pytgcalls() {
    assert_eq!(VideoQuality::Fhd1080p.params(), (1920, 1080, 60));
    assert_eq!(VideoQuality::Sd360p.params(), (640, 360, 30));
    assert_eq!(VideoQuality::Uhd4k.params(), (3840, 2160, 60));
}

#[test]
fn query_classification_matches_python() {
    assert_eq!(classify("dQw4w9WgXcQ"), QueryKind::VideoId);
    assert_eq!(classify("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), QueryKind::Url);
    assert_eq!(classify("https://youtu.be/dQw4w9WgXcQ"), QueryKind::Url);
    assert_eq!(classify("https://example.com/song.mp3"), QueryKind::Url);
    assert_eq!(classify("never gonna give you up"), QueryKind::Search);
    assert_eq!(
        youtube_id("https://youtu.be/dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
}

#[test]
fn ms_to_seconds_passthrough_rule() {
    assert_eq!(ms_to_seconds(500), 500);
    assert_eq!(ms_to_seconds(1000), 1000);
    assert_eq!(ms_to_seconds(2500), 2);
    assert_eq!(sec_to_min(0), "LIVE");
    assert_eq!(sec_to_min(65), "1:05");
    assert_eq!(sec_to_min(3661), "1:01:01");
}

#[test]
fn diagnose_tls_first_then_rate() {
    assert_eq!(
        classify_failure("ssl certificate verify failed"),
        FailureKind::Tls
    );
    assert_eq!(
        classify_failure("HTTP 429 too many requests"),
        FailureKind::RateLimited
    );
    assert_eq!(
        classify_failure("video unavailable, removed"),
        FailureKind::Unavailable
    );
    let (headline, _, kind) = diagnose(
        &["tls handshake failure", "429 slow down"],
        "https://example.com/x",
    );
    assert_eq!(kind, FailureKind::Tls);
    assert!(headline.contains("TLS"));
}

#[test]
fn player_queue_loop_and_history() {
    let mut reg = PlayerRegistry::new();
    let p = reg.player(1);
    let track = |name: &str| CachedTrack {
        name: name.to_string(),
        url: format!("https://x/{name}"),
        ..Default::default()
    };
    p.enqueue(track("a"));
    p.enqueue(track("b"));
    let first = p.start_next().unwrap();
    assert_eq!(first.name, "a");
    assert!(p.is_playing());
    p.pause();
    assert!(!p.is_playing());
    p.resume();
    assert!(p.is_playing());
    // loop: finishing "a" re-queues it behind "b"
    p.loop_enabled = true;
    let second = p.finish_current().unwrap();
    assert_eq!(second.name, "b");
    let third = p.finish_current().unwrap();
    assert_eq!(third.name, "a");
    assert_eq!(p.history.len(), 2);
    p.stop();
    assert!(!p.is_playing());
    assert!(p.current.is_none());
}
