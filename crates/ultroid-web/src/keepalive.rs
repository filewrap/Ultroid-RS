//! HuggingFace keep-alive pinger.
//!
//! Port of `pyUltroid/webserver/keepalive.py`: [`KeepAliveConfig`] is a
//! frozen value object parsed out of the environment; [`KeepAlivePinger`]
//! is the actor — it owns the pacing loop, the stop flag and the task, so
//! the server entry point no longer has to reach for a bare thread.
//!
//! Rust shape: the daemon thread becomes a tokio task, and the interruptible
//! sleep becomes a `tokio::select!` between the delay and a stop watch
//! channel, so `stop()` costs at most one in-flight HTTP call, never a
//! half-finished sleep.

use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tokio::sync::watch;

pub const DEFAULT_MIN: u64 = 7200;
pub const DEFAULT_MAX: u64 = 28800;
pub const FLOOR_INTERVAL: u64 = 60;
pub const REQUEST_TIMEOUT_SECS: u64 = 10;

/// The falsy strings for `KEEP_ALIVE`, lower-cased before comparison.
pub const FALSY: &[&str] = &["0", "false", "no", "off"];

/// One pinger's parameters, all of them environment-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAliveConfig {
    pub url: Option<String>,
    pub token: Option<String>,
    pub enabled: bool,
    pub min_interval: u64,
    pub max_interval: u64,
}

impl KeepAliveConfig {
    pub fn from_env() -> Self {
        Self::from_map(&std::env::vars().collect::<std::collections::HashMap<_, _>>())
    }

    pub fn from_map(source: &std::collections::HashMap<String, String>) -> Self {
        let url = source.get("SPACE_URL").cloned();
        let token = source.get("HF_TOKEN").cloned();
        let flag = source
            .get("KEEP_ALIVE")
            .map(|v| v.trim().to_lowercase())
            .unwrap_or_else(|| "1".to_string());
        let enabled = url.is_some() && token.is_some() && !FALSY.contains(&flag.as_str());

        let int_or = |name: &str, default: u64| -> u64 {
            source
                .get(name)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(default)
        };
        let min_interval = FLOOR_INTERVAL.max(int_or("KEEP_ALIVE_MIN_SECONDS", DEFAULT_MIN));
        let max_interval = min_interval.max(int_or("KEEP_ALIVE_MAX_SECONDS", DEFAULT_MAX));
        Self {
            url,
            token,
            enabled,
            min_interval,
            max_interval,
        }
    }

    /// `authorization: bearer <token>` + `content-type: application/json`,
    /// exactly as Python's `headers` property.
    pub fn headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "authorization".to_string(),
                format!("bearer {}", self.token.clone().unwrap_or_default()),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    }
}

/// The pacing loop, plus the task that runs it.
///
/// `stop` is a real flag rather than "kill the task": the loop checks it
/// between pings, so a stop request costs at most one sleep interval and
/// never a half-finished HTTP call.
pub struct KeepAlivePinger {
    config: Arc<KeepAliveConfig>,
    stop_tx: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
    client: reqwest::Client,
}

impl KeepAlivePinger {
    pub fn new(config: KeepAliveConfig) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config: Arc::new(config),
            stop_tx,
            stop_rx,
            task: None,
            client,
        }
    }

    /// One request. Returns success; never raises.
    pub async fn ping_once(&self) -> bool {
        let url = match &self.config.url {
            Some(u) => u.clone(),
            None => return false,
        };
        let mut request = self.client.get(&url);
        for (name, value) in self.config.headers() {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) => {
                match response.json::<serde_json::Value>().await {
                    Ok(body) => tracing::info!(target: "pyUltroid.webserver.keepalive", "{body}"),
                    Err(_) => {
                        tracing::info!(target: "pyUltroid.webserver.keepalive", "keep-alive ping ok")
                    }
                }
                true
            }
            Err(err) => {
                tracing::warn!(target: "pyUltroid.webserver.keepalive", "keep_alive ping failed: {}", err);
                false
            }
        }
    }

    /// A random delay in `[min_interval, max_interval]` seconds.
    pub fn next_delay(&self) -> u64 {
        rand::thread_rng().gen_range(self.config.min_interval..=self.config.max_interval)
    }

    /// Ping, sleep, repeat until stopped. Blocks the calling task.
    pub async fn run(&self) {
        if !self.config.enabled {
            tracing::info!(target: "pyUltroid.webserver.keepalive", "keep_alive: disabled (SPACE_URL/HF_TOKEN unset or KEEP_ALIVE=0) - pinger skipped.");
            return;
        }
        let mut stop_rx = self.stop_rx.clone();
        loop {
            self.ping_once().await;
            let delay = Duration::from_secs(self.next_delay());
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        break;
                    }
                }
            }
            if *stop_rx.borrow() {
                break;
            }
        }
    }

    /// Run [`KeepAlivePinger::run`] on a tokio task. Idempotent: a second
    /// call is a no-op. Returns `true` when this call started the task.
    pub fn start(&mut self) -> bool {
        if self.task.is_some() {
            return false;
        }
        let config = self.config.clone();
        let stop_rx = self.stop_rx.clone();
        let client = self.client.clone();
        self.task = Some(tokio::spawn(async move {
            let pinger = KeepAlivePinger {
                config,
                stop_tx: watch::channel(false).0,
                stop_rx,
                task: None,
                client,
            };
            pinger.run().await;
        }));
        true
    }

    /// Signal stop and wait up to `timeout` for the task to finish.
    pub async fn stop(&mut self, timeout: Duration) -> bool {
        let _ = self.stop_tx.send(true);
        let Some(task) = self.task.take() else {
            return false;
        };
        match tokio::time::timeout(timeout, task).await {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    pub fn running(&self) -> bool {
        self.task
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }
}

/// Run the pinger to completion on the calling task (module surface).
pub async fn keep_alive(config: KeepAliveConfig) {
    KeepAlivePinger::new(config).run().await;
}

/// Start the pinger on a background task (module surface). The returned
/// handle is detached: dropping it does not stop the pinger; use
/// [`KeepAlivePinger::stop`] on an owned pinger for a clean shutdown.
pub fn start_keepalive_task(config: Option<KeepAliveConfig>) -> tokio::task::JoinHandle<()> {
    let pinger = KeepAlivePinger::new(config.unwrap_or_else(KeepAliveConfig::from_env));
    tokio::spawn(async move {
        pinger.run().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn config_disabled_without_url_or_token() {
        let cfg = KeepAliveConfig::from_map(&map(&[]));
        assert!(!cfg.enabled);
        let cfg = KeepAliveConfig::from_map(&map(&[
            ("SPACE_URL", "https://x"),
            ("HF_TOKEN", "t"),
            ("KEEP_ALIVE", "0"),
        ]));
        assert!(!cfg.enabled);
        let cfg = KeepAliveConfig::from_map(&map(&[
            ("SPACE_URL", "https://x"),
            ("HF_TOKEN", "t"),
            ("KEEP_ALIVE", "OFF"),
        ]));
        assert!(!cfg.enabled);
    }

    #[test]
    fn config_defaults_and_floor() {
        let cfg = KeepAliveConfig::from_map(&map(&[("SPACE_URL", "https://x"), ("HF_TOKEN", "t")]));
        assert!(cfg.enabled);
        assert_eq!(cfg.min_interval, DEFAULT_MIN);
        assert_eq!(cfg.max_interval, DEFAULT_MAX);
        let cfg = KeepAliveConfig::from_map(&map(&[
            ("SPACE_URL", "https://x"),
            ("HF_TOKEN", "t"),
            ("KEEP_ALIVE_MIN_SECONDS", "1"),
            ("KEEP_ALIVE_MAX_SECONDS", "5"),
        ]));
        assert_eq!(cfg.min_interval, FLOOR_INTERVAL);
        assert_eq!(cfg.max_interval, FLOOR_INTERVAL);
    }

    #[test]
    fn headers_bearer_lowercase() {
        let cfg =
            KeepAliveConfig::from_map(&map(&[("SPACE_URL", "https://x"), ("HF_TOKEN", "tok123")]));
        let headers = cfg.headers();
        assert_eq!(
            headers[0],
            ("authorization".to_string(), "bearer tok123".to_string())
        );
        assert_eq!(
            headers[1],
            ("content-type".to_string(), "application/json".to_string())
        );
    }
}
