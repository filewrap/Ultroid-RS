//! Small VC helpers. Port of `vc/_tools.py`.
//!
//! `ms_to_seconds`: values ≤ 1000 pass through unchanged, otherwise
//! integer-divided by 1000 (mirrors the Python helper exactly — the
//! ≤1000 pass-through is intentional, `played_time` uses it).

use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;
use ultroid_core::error::Result;

/// The downloads directory (mirrors `_tools.py` `DOWNLOADS_DIR = "./temp"`).
pub const DOWNLOADS_DIR: &str = "./temp";

/// Seconds → `M:SS` or `H:MM:SS`; 0 → `LIVE` (mirrors `_messages.fmt_duration`).
pub fn sec_to_min(seconds: u64) -> String {
    if seconds == 0 {
        return "LIVE".to_string();
    }
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn ms_to_seconds(ms: u64) -> u64 {
    if ms <= 1000 { ms } else { ms / 1000 }
}

/// Remove files older than `max_age_secs` from a directory, mirroring
/// `cleanup_downloads_dir("./temp", 3600)`.
pub async fn cleanup_downloads_dir(dir: &Path, max_age_secs: u64) -> Result<()> {
    let now = std::time::SystemTime::now();
    let mut rd = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| ultroid_core::error::Error::Other(format!("read {dir:?}: {e}")))?;
    while let Some(entry) = rd
        .next_entry()
        .await
        .map_err(|e| ultroid_core::error::Error::Other(e.to_string()))?
    {
        let meta = entry
            .metadata()
            .await
            .map_err(|e| ultroid_core::error::Error::Other(e.to_string()))?;
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = now.duration_since(modified) {
                if age > Duration::from_secs(max_age_secs) {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        }
    }
    Ok(())
}

/// Wait for `dur` while yielding (used for stream-end polling loops).
pub async fn wait_secs(dur: Duration) {
    sleep(dur).await;
}
