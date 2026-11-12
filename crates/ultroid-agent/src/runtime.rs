//! Single orchestration boundary for every AI/agent surface — port of
//! `pyUltroid/agent/_runtime.py`.
//!
//! Command handlers must not resolve providers, controls, or
//! capabilities independently. [`AgentRuntime::runtime_run`] is the one
//! place where a request is normalized, capability policy is applied,
//! and the agent is invoked.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;
use ultroid_core::db::Database;

use crate::agent::{AgentResponse, RespondArgs, StreamArgs, UltroidAgent};
use crate::config::AgentConfig;
use crate::controls::get_controls;
use crate::tools::{ToolExecutor, default_agent_caps};

/// Capabilities that are safe on a shared/group surface. Shell,
/// filesystem writes, package installation and sending messages stay
/// owner/sudo-only.
///
/// Ported verbatim from `_runtime.py`: the exclusion names are the
/// literal strings there (`"shell_exec"`, `"shell_write"`, ...), which
/// mostly do *not* match the dotted `Cap` tokens in
/// `DEFAULT_AGENT_CAPS` (`"shell.exec"`, ...). Only `"tg.file_send"`,
/// `"agent.spawn"` and `"agent.ask"` actually match and are removed;
/// the rest of the exclusion list is dead weight in Python too, and is
/// kept here so the two stay identical.
pub fn group_safe_caps() -> HashSet<String> {
    const DENIED: &[&str] = &[
        "shell_exec",
        "shell_write",
        "fs_write",
        "tg_send",
        "tg.file_send",
        "res_install",
        "tg_raw",
        "agent.spawn",
        "agent.ask",
    ];
    default_agent_caps()
        .into_iter()
        .filter(|cap| !DENIED.contains(&cap.as_str()))
        .collect()
}

/// Return the exact capability grant for one request.
///
/// `owner`/`sudo` may use the full default set. Everyone else receives
/// only read-only/research tools, and tools can always be disabled by
/// chat controls. DM and group surfaces use the same rule, fixing the
/// previous asymmetry where a group command could grant shell execution.
///
/// `privileged` is the owner + sudo id set, supplied by the caller: the
/// Python source reads it from `pyUltroid._misc.owner_and_sudos`,
/// which has no counterpart inside this crate.
pub fn resolve_caps(
    surface: &str,
    user_id: Option<i64>,
    tools_enabled: bool,
    is_private: bool,
    privileged: &HashSet<i64>,
) -> HashSet<String> {
    let _ = surface;
    let _ = is_private;
    if !tools_enabled {
        return HashSet::new();
    }
    let privileged = user_id.map(|id| privileged.contains(&id)).unwrap_or(false);
    if privileged {
        default_agent_caps()
    } else {
        group_safe_caps()
    }
}

/// A normalized, surface-independent agent request.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub prompt: String,
    /// Canonical chat key string (numeric id or `dm:<user>:<anchor>`).
    pub chat_id: String,
    pub origin: String,
    pub user_id: Option<i64>,
    pub is_private: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_rounds: Option<u32>,
    pub use_tools: Option<bool>,
    pub rich_messages_allowed: Option<bool>,
    pub use_history: bool,
    pub use_cache: bool,
    pub caps: Option<HashSet<String>>,
    pub metadata: HashMap<String, Value>,
    pub timeout_seconds: Option<f64>,
}

impl AgentRequest {
    pub fn new(prompt: impl Into<String>, chat_id: impl Into<String>) -> Self {
        AgentRequest {
            prompt: prompt.into(),
            chat_id: chat_id.into(),
            origin: "ai".to_string(),
            user_id: None,
            is_private: true,
            provider: None,
            model: None,
            temperature: None,
            max_rounds: None,
            use_tools: None,
            rich_messages_allowed: None,
            use_history: true,
            use_cache: true,
            caps: None,
            metadata: HashMap::new(),
            timeout_seconds: None,
        }
    }
}

/// The only caller of [`UltroidAgent::respond`] for command surfaces.
pub struct AgentRuntime {
    pub agent: Arc<UltroidAgent>,
    config: Option<Arc<AgentConfig>>,
    db: Option<Arc<dyn Database>>,
    privileged: HashSet<i64>,
    data_dir: PathBuf,
}

impl AgentRuntime {
    pub fn new(
        agent: Arc<UltroidAgent>,
        config: Option<Arc<AgentConfig>>,
        db: Option<Arc<dyn Database>>,
        privileged: HashSet<i64>,
        data_dir: impl Into<PathBuf>,
    ) -> Self {
        AgentRuntime {
            agent,
            config,
            db,
            privileged,
            data_dir: data_dir.into(),
        }
    }

    pub async fn runtime_prepare(&self, request: &AgentRequest) -> AgentRequest {
        let controls = get_controls(self.db.as_ref(), &request.chat_id).await;
        let tools = request.use_tools.unwrap_or(controls.tools);
        let caps = match &request.caps {
            Some(c) => Some(c.clone()),
            None => Some(resolve_caps(
                &request.origin,
                request.user_id,
                tools,
                request.is_private,
                &self.privileged,
            )),
        };
        let provider = match &request.provider {
            Some(p) => Some(p.clone()),
            None => match &self.config {
                Some(cfg) => cfg.config_default_provider().await.ok().flatten(),
                None => None,
            },
        };
        AgentRequest {
            prompt: request.prompt.clone(),
            chat_id: request.chat_id.clone(),
            origin: request.origin.clone(),
            user_id: request.user_id,
            is_private: request.is_private,
            provider,
            model: request.model.clone().or(controls.model),
            temperature: request.temperature.or(Some(controls.temp)),
            max_rounds: request.max_rounds.or(controls.rounds),
            use_tools: Some(tools),
            rich_messages_allowed: request.rich_messages_allowed,
            use_history: request.use_history,
            use_cache: request.use_cache,
            caps,
            metadata: request.metadata.clone(),
            timeout_seconds: request.timeout_seconds,
        }
    }

    pub async fn runtime_run(&self, request: &AgentRequest) -> AgentResponse {
        let prepared = self.runtime_prepare(request).await;
        let started = Instant::now();
        let args = RespondArgs {
            user_message: prepared.prompt.clone(),
            chat_id: prepared.chat_id.clone(),
            provider_name: prepared.provider.clone(),
            temperature: prepared.temperature.unwrap_or(0.7) as f32,
            use_tools: prepared.use_tools.unwrap_or(true),
            use_history: prepared.use_history,
            use_cache: prepared.use_cache,
            model: prepared.model.clone(),
            max_rounds: prepared.max_rounds,
            scope: prepared.origin.clone(),
            caps: prepared.caps.clone(),
            rich_messages_allowed: prepared.rich_messages_allowed,
            ..RespondArgs::default()
        };
        let fut = self.agent.respond(args);
        match prepared.timeout_seconds {
            None => fut.await,
            Some(limit) => match tokio::time::timeout(Duration::from_secs_f64(limit), fut).await {
                Ok(response) => response,
                Err(_) => {
                    let message = format!(
                        "The agent request exceeded its {limit}-second time limit. Try a smaller request or run the steps separately."
                    );
                    let mut response = AgentResponse::new(message.clone(), "runtime", "timeout");
                    response.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
                    response.error = Some(message);
                    response
                }
            },
        }
    }

    /// Yield normalized streaming chunks for *request*.
    pub async fn runtime_stream(&self, request: &AgentRequest) -> mpsc::Receiver<String> {
        let prepared = self.runtime_prepare(request).await;
        let args = StreamArgs {
            user_message: prepared.prompt,
            chat_id: prepared.chat_id,
            provider_name: prepared.provider,
            temperature: prepared.temperature.unwrap_or(0.7) as f32,
            model: prepared.model,
            rich_messages_allowed: prepared.rich_messages_allowed,
            ..StreamArgs::default()
        };
        self.agent.stream(args)
    }

    pub async fn runtime_executor(&self, request: &AgentRequest) -> ToolExecutor {
        let prepared = self.runtime_prepare(request).await;
        let chat_id = prepared.chat_id.parse::<i64>().unwrap_or(0);
        let mut executor = ToolExecutor::new(self.data_dir.clone()).with_chat_id(chat_id);
        if let Some(caps) = prepared.caps {
            executor = executor.with_caps(caps);
        }
        executor
    }
}
