//! Startup helpers — port of `pyUltroid/startup/funcs.py`, `utils.py`,
//! `_loops.py` (background loops) and the boot sequence in `runtime.py`.

use crate::config::Config;
use crate::error::Result;
use std::sync::Arc;

/// Background loops (Python `_loops.py`): keep-alive pinger interval etc.
pub async fn keep_alive_loop(cfg: Arc<Config>) {
    if !cfg.keep_alive {
        return;
    }
    let Some(url) = cfg.space_url.clone() else {
        return;
    };
    let http = reqwest::Client::new();
    loop {
        let _ = http.get(&url).send().await;
        tokio::time::sleep(std::time::Duration::from_secs(240)).await;
    }
}

/// Ensure the data/session directories exist.
pub fn ensure_dirs(cfg: &Config) -> Result<()> {
    for sub in ["sessions", "downloads", "addons"] {
        std::fs::create_dir_all(std::path::Path::new(&cfg.data_dir).join(sub))?;
    }
    Ok(())
}
