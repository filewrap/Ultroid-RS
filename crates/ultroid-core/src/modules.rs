//! Core modules — port of `pyUltroid/modules/` (`execute.py`, `updates.py`,
//! `_gorilla.py`): shell execution helpers shared by `.eval`/`.bash`.

use crate::error::{Error, Result};
use std::time::Duration;

/// Run a shell command with a timeout (seconds, clamped to [1, 120] like the
/// agent shell). Returns (stdout, stderr, exit code).
pub async fn run_shell(cmd: &str, timeout_secs: u64) -> Result<(String, String, i32)> {
    let timeout_secs = timeout_secs.clamp(1, 120);
    let fut = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), fut)
        .await
        .map_err(|_| Error::Other(format!("shell command timed out after {timeout_secs}s")))??;
    Ok((
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    ))
}
