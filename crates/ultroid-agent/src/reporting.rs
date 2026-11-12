//! Best-effort reporting of agent activity to the log channel — port of
//! `pyUltroid/agent/_reporting.py`.
//!
//! Every report is fire-and-forget: a failing log channel must never
//! break the command that produced the activity being logged. All
//! sending happens in a background task, and any error is swallowed
//! after being written to the process log.
//!
//! The reports use the same native rich rendering as the rest of the
//! bot, with a plain-text fallback for clients that cannot render it.
//! Rich construction lives in the binary crate, behind [`ReportSink`]:
//! this module builds the header/lines and the fallback text, and the
//! sink decides how to render them.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::task::JoinHandle;
use tracing::debug;
use ultroid_core::db::Database;

const REPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// The minimal log-channel send capability reporting needs.
/// Implemented by the binary crate.
#[async_trait]
pub trait ReportSink: Send + Sync {
    /// Send the rich report; returns `true` when it was delivered.
    /// Returning `false` (or an error) falls through to the plain-text
    /// send, exactly like `_send_rich` in Python.
    async fn send_rich_report(
        &self,
        channel: &str,
        header: &str,
        lines: &[(String, String)],
    ) -> std::result::Result<bool, String>;
    /// Send the plain-text fallback.
    async fn send_text(&self, channel: &str, text: &str) -> std::result::Result<(), String>;
}

/// One agent run, for the log channel.
#[derive(Debug, Clone)]
pub struct ReportParams {
    /// The command or surface that produced the run (`.ai`, `.agent`,
    /// a DM session, ...).
    pub origin: String,
    pub chat_id: String,
    pub user: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The prompt itself is accepted for call-site parity; like Python
    /// it is never written to the log channel.
    pub prompt: String,
    pub tools_used: Vec<String>,
    pub duration_secs: Option<f64>,
    pub ok: bool,
    pub error: Option<String>,
}

fn fallback_text(header: &str, lines: &[(String, String)]) -> String {
    let body: Vec<String> = lines
        .iter()
        .map(|(label, value)| format!("{label}: {value}"))
        .collect();
    format!("{header}\n{}", body.join("\n"))
}

async fn send_report(
    sink: &Arc<dyn ReportSink>,
    channel: &str,
    header: &str,
    lines: &[(String, String)],
) {
    match sink.send_rich_report(channel, header, lines).await {
        Ok(true) => return,
        _ => {}
    }
    let text = fallback_text(header, lines);
    if let Err(e) = sink.send_text(channel, &text).await {
        debug!("[AGENT:REPORT] log channel send failed: {e}");
    }
}

/// Log one agent run to the log channel; never blocks, never raises.
///
/// Only failures are reported (a successful run returns `None`
/// immediately, exactly as in Python). Returns the background task,
/// mainly for tests.
pub fn report_run(
    db: Option<Arc<dyn Database>>,
    sink: Option<Arc<dyn ReportSink>>,
    params: ReportParams,
) -> Option<JoinHandle<()>> {
    if params.ok {
        return None;
    }
    let sink = sink?;
    let db = db?;
    let origin = params.origin.clone();
    let chat_id = params.chat_id.clone();
    let user = params.user.clone();
    let provider = params.provider.clone();
    let model = params.model.clone();
    let tools_used = params.tools_used.clone();
    let duration = params.duration_secs;
    let error = params.error.clone();
    let task = tokio::spawn(async move {
        let channel = db.get("LOG_CHANNEL").await.ok().flatten();
        let Some(channel) = channel else {
            return;
        };
        let header = format!("Agent run · {origin} · failed");
        let mut lines: Vec<(String, String)> = vec![
            ("Chat".to_string(), chat_id),
            (
                "User".to_string(),
                user.unwrap_or_else(|| " - ".to_string()),
            ),
            (
                "Provider".to_string(),
                provider.unwrap_or_else(|| " - ".to_string()),
            ),
            (
                "Model".to_string(),
                model.unwrap_or_else(|| " - ".to_string()),
            ),
            (
                "Tools".to_string(),
                if tools_used.is_empty() {
                    "none".to_string()
                } else {
                    tools_used.join(", ")
                },
            ),
            (
                "Duration".to_string(),
                match duration {
                    Some(d) => format!("{d:.1}s"),
                    None => " - ".to_string(),
                },
            ),
        ];
        if error.is_some() {
            lines.push((
                "Error".to_string(),
                "The agent run failed; details are withheld.".to_string(),
            ));
        }
        // One report, abandoned if it cannot finish in time. The
        // timeout bounds the background task: without it, a hung
        // log-channel send would leave one task pending per agent run.
        let _ = tokio::time::timeout(
            REPORT_TIMEOUT,
            send_report(&sink, &channel, &header, &lines),
        )
        .await;
    });
    Some(task)
}
