//! Failure classification for download/resolve errors.
//! Port of `vc/_diagnose.py`.
//!
//! Classification order (mirrors `classify_failure` exactly): TLS first,
//! then rate limit, geo block, unavailable, no format, network, unknown.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Tls,
    Network,
    Unavailable,
    NoFormat,
    RateLimited,
    GeoBlocked,
    Unknown,
}

impl FailureKind {
    pub fn label(self) -> &'static str {
        match self {
            FailureKind::Tls => "TLS",
            FailureKind::Network => "network",
            FailureKind::Unavailable => "unavailable",
            FailureKind::NoFormat => "no-format",
            FailureKind::RateLimited => "rate-limited",
            FailureKind::GeoBlocked => "geo-blocked",
            FailureKind::Unknown => "unknown",
        }
    }
}

const TLS_MARKERS: &[&str] = &[
    "tls",
    "ssl",
    "certificate",
    "handshake",
    "unknownissuer",
    "unknown issuer",
];
const RATE_MARKERS: &[&str] = &["429", "rate limit", "too many requests", "slow down"];
const GEO_MARKERS: &[&str] = &[
    "geo",
    "not available in your country",
    "blocked in your country",
    "region",
];
const UNAVAILABLE_MARKERS: &[&str] = &[
    "404",
    "not found",
    "unavailable",
    "removed",
    "private video",
    "deleted",
];
const NO_FORMAT_MARKERS: &[&str] = &["no format", "no suitable", "no video formats"];
const NETWORK_MARKERS: &[&str] = &[
    "connection",
    "timeout",
    "timed out",
    "dns",
    "unreachable",
    "reset",
];

fn hit(markers: &[&str], hay: &str) -> bool {
    markers.iter().any(|m| hay.contains(m))
}

/// Classify an error message (empty messages classify as Unknown).
pub fn classify_failure(error: &str) -> FailureKind {
    let hay = error.to_lowercase();
    if hit(TLS_MARKERS, &hay) {
        FailureKind::Tls
    } else if hit(RATE_MARKERS, &hay) {
        FailureKind::RateLimited
    } else if hit(GEO_MARKERS, &hay) {
        FailureKind::GeoBlocked
    } else if hit(UNAVAILABLE_MARKERS, &hay) {
        FailureKind::Unavailable
    } else if hit(NO_FORMAT_MARKERS, &hay) {
        FailureKind::NoFormat
    } else if hit(NETWORK_MARKERS, &hay) {
        FailureKind::Network
    } else {
        FailureKind::Unknown
    }
}

fn trim(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() > 300 {
        format!("{}…", &collapsed[..300])
    } else {
        collapsed
    }
}

/// One-line headline + detail for a failing URL, mirroring `diagnose`.
pub fn diagnose(errors: &[&str], url: &str) -> (String, String, FailureKind) {
    let kind = errors
        .iter()
        .map(|e| classify_failure(e))
        .find(|k| *k != FailureKind::Unknown)
        .unwrap_or(FailureKind::Unknown);
    let headline = match kind {
        FailureKind::Tls => format!("Could not verify the server for {url} (TLS)."),
        FailureKind::RateLimited => format!("The source rate-limited requests for {url}."),
        FailureKind::GeoBlocked => format!("This media is geo-blocked for {url}."),
        FailureKind::Unavailable => format!("This media is unavailable: {url}."),
        FailureKind::NoFormat => format!("No playable format found for {url}."),
        FailureKind::Network => format!("Network error while reaching {url}."),
        FailureKind::Unknown => format!("Could not play {url}."),
    };
    let detail = errors
        .iter()
        .map(|e| trim(e))
        .collect::<Vec<_>>()
        .join(" | ");
    (headline, detail, kind)
}

/// Summarise a list of attempt errors into one line.
pub fn summarise_attempts(errors: &[&str]) -> String {
    errors
        .iter()
        .map(|e| trim(e))
        .collect::<Vec<_>>()
        .join("; ")
}
