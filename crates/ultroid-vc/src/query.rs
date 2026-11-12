//! Query classification. Port of `vc/_query.py`.
//!
//! `QueryKind`: bare 11-char YouTube id, YouTube URL, any http(s) URL,
//! anything else = search text.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    VideoId,
    Url,
    Search,
}

fn youtube_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:youtube\.com/(?:watch\?v=|shorts/|embed/|v/)|youtu\.be/)([A-Za-z0-9_-]{11})",
        )
        .expect("static YouTube regex")
    })
}

fn bare_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").expect("static bare-id regex"))
}

/// Classify a user query, mirroring `_query.py`.
pub fn classify(query: &str) -> QueryKind {
    let q = query.trim();
    if youtube_url_re().is_match(q) || (q.starts_with("http") && q.contains("youtube")) {
        return QueryKind::Url;
    }
    if q.starts_with("http://") || q.starts_with("https://") {
        return QueryKind::Url;
    }
    if bare_id_re().is_match(q) {
        return QueryKind::VideoId;
    }
    QueryKind::Search
}

/// Extract a YouTube video id from a URL or bare id.
pub fn youtube_id(query: &str) -> Option<String> {
    let q = query.trim();
    if let Some(c) = youtube_url_re().captures(q) {
        return Some(c[1].to_string());
    }
    if bare_id_re().is_match(q) {
        return Some(q.to_string());
    }
    None
}

/// First line of the status message for a query.
pub fn status_head(query: &str) -> String {
    format!("Searching for “{query}”…")
}

/// One row of a results list.
pub fn status_row(index: usize, title: &str, duration: &str) -> String {
    format!("{}. {} ({})", index + 1, title, duration)
}
