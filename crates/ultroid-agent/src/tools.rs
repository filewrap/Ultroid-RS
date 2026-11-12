//! Tool schema and executor: the agent's hands, eyes, memory, and voice.
//!
//! Port of `source/pyUltroid/agent/_tools.py`.
//!
//! What is real here:
//! - `clamp_shell_timeout`, the capability gate (`TOOL_REQUIRED_CAPS` /
//!   `DEFAULT_AGENT_CAPS` / arg-level escalation), and the shell exclusion
//!   lists (`BLOCKED_COMMANDS` / `BLOCKED_PATHS` / `BLOCKED_PATTERNS` from
//!   `pyUltroid/shell/_context.py` + the check order in `_security.py`)
//!   are ported exactly.
//! - `shell_execute` really runs via `tokio::process` with the clamped
//!   timeout, killing the child on expiry, truncating stdout to 2000 chars
//!   and stderr to 500 chars exactly as the Python source does.
//! - `web_fetch` and `web_search` really run over HTTP via `reqwest`
//!   (same endpoints, timeouts, clamps, and HTML-stripping as the source).
//! - `think` is a real echo scratchpad; `remember` is a real
//!   file-backed store under the executor's data dir, keyed
//!   `agent:memory:<key>` exactly as the Python source keys Redis.
//! - `tool_definitions` reproduces all 21 schemas (`TOOL_SCHEMAS` plus the
//!   three `INTERACTIVE_SCHEMAS` from `_interactive.py`) verbatim.
//!
//! What is NOT implemented, and why (each returns an honest error
//! `ToolResult`, the same shape the Python `execute()` returns for a
//! failed tool — never a fake success):
//! - `send_message`, `send_text_file` (delivery half), `tg_route_send`,
//!   `tg_read`, `raw_mtproto`: need the agent `_tgbridge`
//!   (`send_via_master` / `send_file_via_master` / `read_recent` /
//!   `invoke_raw`) and the master Telegram client. No Telegram client
//!   handle is passed to this executor and no Telegram dependency exists
//!   in this crate, so there is nothing to send through.
//! - `code_search`, `get_traces`, `get_bot_status`, `plugin_manager`
//!   (list half): need the tracer (`pyUltroid.tracer.tr`) and the plugin
//!   loader (`pyUltroid._core.loader`), neither ported into this crate.
//! - `update_check`: needs the updater (`pyUltroid.updater.up`).
//! - `install_package`: needs the resource extender
//!   (`pyUltroid.extender.ex.resources.install_dep`).
//! - `process_image`: needs the imaging engine (`pyUltroid.imaging.Imaging`);
//!   no image crate is among this crate's dependencies.
//! - `ask_user`, `confirm_action`, `collect_fields`: need the interactive
//!   bridge (`AskBridge` / `shared_bridge()` from `_bridge.py`).
//! - `delegate`: needs `UltroidAgent::respond` from `_agent.py` (the agent
//!   loop itself, a sibling module being ported in parallel). The depth
//!   refusal (`MAX_AGENT_DEPTH`) IS implemented.
//!
//! The local validation each of those tools performs before touching its
//! backend (filename/content/caption rules for `send_text_file`, the
//! operation table for `process_image`, action/plugin rules for
//! `plugin_manager`, the depth check for `delegate`) is ported in full,
//! so the refusal happens at exactly the same point the Python source
//! would refuse.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};
use tokio::process::Command;
use tracing::{info, warn};
use ultroid_core::error::{Error, Result};

use crate::providers::ToolCall;

// ── Timeout bounds ─────────────────────────────────────────────────────────

/// Never let a shell timeout exceed this cap regardless of prompt content.
/// (`AGENT_SHELL_MIN_TIMEOUT` in `_tools.py`.)
pub const AGENT_SHELL_MIN_TIMEOUT: f64 = 1.0;
/// (`AGENT_SHELL_MAX_TIMEOUT` in `_tools.py`.)
pub const AGENT_SHELL_MAX_TIMEOUT: f64 = 120.0;
/// Junk timeout values fall back to this. (`clamp_shell_timeout`.)
pub const AGENT_SHELL_DEFAULT_TIMEOUT: f64 = 60.0;

/// Deepest nested delegation the runtime permits. A sub-agent created by a
/// sub-agent of this depth is refused, so delegation can never recurse.
/// (`MAX_AGENT_DEPTH` in `_tools.py`.)
pub const MAX_AGENT_DEPTH: u32 = 2;

/// Coerce an LLM-supplied timeout into `[MIN, MAX]`; junk becomes 60s.
/// (`clamp_shell_timeout` in `_tools.py`.)
///
/// Deviation note: Python's `max(1.0, min(float("nan"), 120.0))` evaluates
/// to `1.0` because NaN comparisons are False; here NaN is treated as junk
/// and returns the 60s default, which matches the function's documented
/// "junk becomes 60s" contract.
pub fn clamp_shell_timeout(timeout: f64) -> f64 {
    if timeout.is_nan() {
        return AGENT_SHELL_DEFAULT_TIMEOUT;
    }
    timeout
        .max(AGENT_SHELL_MIN_TIMEOUT)
        .min(AGENT_SHELL_MAX_TIMEOUT)
}

// ── Capability tokens ──────────────────────────────────────────────────────
// (`Cap` in `pyUltroid/shell/_context.py`; token strings only.)

pub const CAP_SHELL_EXEC: &str = "shell.exec";
pub const CAP_SHELL_READ: &str = "shell.read";
pub const CAP_SHELL_WRITE: &str = "shell.write";
pub const CAP_SHELL_NET: &str = "shell.net";
pub const CAP_FS_READ: &str = "fs.read";
pub const CAP_FS_WRITE: &str = "fs.write";
pub const CAP_FS_MEDIA: &str = "fs.media";
pub const CAP_PROC_SPAWN: &str = "proc.spawn";
pub const CAP_PROC_KILL: &str = "proc.kill";
pub const CAP_PLUGIN_READ: &str = "plugin.read";
pub const CAP_PLUGIN_LOAD: &str = "plugin.load";
pub const CAP_PLUGIN_UNLOAD: &str = "plugin.unload";
pub const CAP_UPDATE_READ: &str = "update.read";
pub const CAP_UPDATE_APPLY: &str = "update.apply";
pub const CAP_RES_INSTALL: &str = "res.install";
pub const CAP_TG_SEND: &str = "tg.send";
pub const CAP_TG_SEND_FILE: &str = "tg.file_send";
pub const CAP_TG_READ: &str = "tg.read";
pub const CAP_TG_RAW: &str = "tg.raw";
pub const CAP_IMG_PROCESS: &str = "img.process";
pub const CAP_AGENT_SPAWN: &str = "agent.spawn";
pub const CAP_AGENT_ASK: &str = "agent.ask";

/// Minimum caps required to invoke each tool at all.
/// (`TOOL_REQUIRED_CAPS` in `_tools.py`.)
fn tool_required_caps(tool_name: &str, arguments: &Value) -> Option<Vec<&'static str>> {
    let mut required: Vec<&'static str> = match tool_name {
        "shell_execute" => vec![CAP_SHELL_EXEC],
        "code_search" => vec![CAP_PLUGIN_READ],
        "get_traces" => vec![],
        "update_check" => vec![CAP_UPDATE_READ],
        "install_package" => vec![CAP_RES_INSTALL],
        "process_image" => vec![CAP_IMG_PROCESS],
        "send_message" => vec![CAP_TG_SEND],
        "send_text_file" => vec![CAP_TG_SEND_FILE],
        "get_bot_status" => vec![],
        "remember" => vec![],
        "web_search" => vec![],
        "tg_route_send" => vec![CAP_TG_SEND],
        "tg_read" => vec![CAP_TG_READ],
        "plugin_manager" => vec![CAP_PLUGIN_READ],
        "raw_mtproto" => vec![CAP_TG_RAW],
        "web_fetch" => vec![],
        "think" => vec![],
        "delegate" => vec![CAP_AGENT_SPAWN],
        "ask_user" => vec![CAP_AGENT_ASK],
        "confirm_action" => vec![CAP_AGENT_ASK],
        "collect_fields" => vec![CAP_AGENT_ASK],
        _ => return None, // unknown tool — handled by the dispatch miss below
    };
    // Arg-level escalation: applying an update is privileged; checking is not.
    if tool_name == "update_check" && arg_bool(arguments, "apply", false) {
        required.push(CAP_UPDATE_APPLY);
    }
    // plugin_manager: listing needs only PLUGIN_READ, but mutating the
    // loaded plugin set is its own grant (never in DEFAULT_AGENT_CAPS).
    if tool_name == "plugin_manager" {
        let action = arg_str(arguments, "action")
            .unwrap_or("list")
            .to_lowercase();
        if action == "load" {
            required.push(CAP_PLUGIN_LOAD);
        } else if action == "unload" || action == "reload" {
            required.push(CAP_PLUGIN_UNLOAD);
        }
    }
    Some(required)
}

/// The executor's explicit capability grant.
/// (`DEFAULT_AGENT_CAPS` in `_tools.py`.)
pub fn default_agent_caps() -> HashSet<String> {
    [
        CAP_SHELL_EXEC,
        CAP_SHELL_READ,
        CAP_SHELL_WRITE,
        CAP_SHELL_NET,
        CAP_FS_READ,
        CAP_FS_WRITE,
        CAP_PLUGIN_READ,
        CAP_UPDATE_READ,
        CAP_TG_SEND,
        CAP_TG_SEND_FILE,
        CAP_TG_READ,
        CAP_AGENT_SPAWN,
        CAP_IMG_PROCESS,
        CAP_AGENT_ASK,
    ]
    .iter()
    .map(|c| c.to_string())
    .collect()
}

// ── Shell exclusion lists ──────────────────────────────────────────────────
// Ported verbatim from `pyUltroid/shell/_context.py` (`BLOCKED_COMMANDS`,
// `BLOCKED_PATHS`, `BLOCKED_PATTERNS`) and checked in the order
// `pyUltroid/shell/_security.py::SecurityGuard::check` uses:
// hardcoded command blocks → blocked-pattern scan → blocked-path literal
// scan → suspicious scan (flagged, execution continues).

/// Commands blocked regardless of trust level (substring match).
pub const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf /",
    "dd if=",
    "> /dev/sda",
    "mkfs",
    ":(){ :|:& };:", // fork bomb
    "chmod 777 /",
    "chown -R",
];

/// Paths always blocked regardless of trust or caps.
/// Checked with `*` stripped, as a substring of the command string.
pub const BLOCKED_PATHS: &[&str] = &[
    ".env",
    "*.session",
    "asst.session",
    "pyUltroid/configs.py",
    "pyUltroid/startup/",
    "/etc/passwd",
    "/etc/shadow",
    "~/.ssh",
    "/root/.ssh",
];

/// Regex patterns always blocked (searched against the command string).
pub const BLOCKED_PATTERNS: &[&str] = &[
    r"\.session$", // any .session file
    r"\.env$",     // .env in any directory
    r"id_rsa",     // SSH private keys
    r"authorized_keys",
    r"SECRET", // env var names with SECRET in them
    r"TOKEN",  // any token exfiltration attempt
    r"API_KEY",
];

/// Suspicious but not blocked — logged and flagged, execution continues.
/// (`SecurityGuard._SUSPICIOUS_PATTERNS` in `_security.py`.)
const SUSPICIOUS_PATTERNS: &[&str] = &[
    r"curl\s+.*\|\s*sh",                     // curl pipe to shell
    r"wget\s+.*\|\s*sh",                     // wget pipe to shell
    r"base64\s+--decode",                    // base64 decode exfil
    r#"python\d?\s+-c\s+['"]import socket"#, // reverse shell
    r"nc\s+-[el]",                           // netcat listener/exec
    r"/proc/self/environ",                   // env dump via /proc
    r"cat\s+/etc/(passwd|shadow)",           // credential files
    r"history\s*$",                          // shell history dump
    r"printenv|env\s*$",                     // full env dump
];

fn blocked_res() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        BLOCKED_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("hardcoded blocked pattern must compile"))
            .collect()
    })
}

fn suspicious_res() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        SUSPICIOUS_PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("hardcoded suspicious pattern must compile"))
            .collect()
    })
}

/// Run the pre-execution security check. Returns `Some(reason)` when the
/// command is denied, `None` when it may run.
fn shell_security_check(command: &str) -> Option<String> {
    // 1. Hardcoded command blocks — absolute, no override.
    for blocked in BLOCKED_COMMANDS {
        if command.contains(blocked) {
            return Some(format!("hardcoded block matched: {blocked:?}"));
        }
    }
    // 2. Hardcoded path blocks — regex scan on the command string.
    for re in blocked_res() {
        if re.is_match(command) {
            return Some(format!("blocked pattern matched: {:?}", re.as_str()));
        }
    }
    // 3. Hardcoded path literal scan.
    for path in BLOCKED_PATHS {
        if command.contains(&path.replace('*', "")) {
            return Some(format!("blocked path referenced: {path:?}"));
        }
    }
    // 4–5. Trust / capability checks live in the shell layer in Python;
    // the executor's own cap gate (`_missing_caps`) already ran before
    // dispatch, so there is nothing further to check here.
    // 6. Suspicious pattern scan — flag but allow.
    if suspicious_res().iter().any(|re| re.is_match(command)) {
        warn!("[AGENT:TOOLS] suspicious shell command (allowed): {command}");
    }
    None
}

// ── Argument helpers ───────────────────────────────────────────────────────

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_f64(args: &Value, key: &str, default: f64) -> f64 {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(default),
        _ => default,
    }
}

fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    match args.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(default)),
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn arg_obj(args: &Value, key: &str) -> Map<String, Value> {
    args.get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn err(message: impl Into<String>) -> Value {
    json!({ "error": message.into() })
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// ── codex_tool_kwargs ──────────────────────────────────────────────────────

/// Build OpenAI/Codex-compatible tool-calling kwargs.
///
/// Returns `{}` when `tools` is `None` or empty so callers can spread the
/// result unconditionally. `tool_choice` accepts `"auto"` / `"none"` /
/// `"required"` or a specific function-selector object; pass `None` to omit
/// it. `parallel_tool_calls` is omitted when `None` (the API default
/// applies).
///
/// (`codex_tool_kwargs` in `_tools.py`. The Python default for
/// `tool_choice` is `"auto"`; callers that want that pass
/// `Some(json!("auto"))`.)
pub fn codex_tool_kwargs(
    tools: Option<&[Value]>,
    tool_choice: Option<Value>,
    parallel_tool_calls: Option<bool>,
) -> Value {
    let Some(tools) = tools else {
        return json!({});
    };
    if tools.is_empty() {
        return json!({});
    }
    let mut kwargs = Map::new();
    kwargs.insert("tools".to_string(), Value::Array(tools.to_vec()));
    if let Some(choice) = tool_choice {
        kwargs.insert("tool_choice".to_string(), choice);
    }
    if let Some(parallel) = parallel_tool_calls {
        kwargs.insert("parallel_tool_calls".to_string(), json!(parallel));
    }
    Value::Object(kwargs)
}

// ── ToolResult / ToolExecutor ──────────────────────────────────────────────

/// One executed tool call, ready to be fed back to the model.
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub output: String,
    pub is_error: bool,
}

/// Executes tool calls from AI responses. Each tool returns a
/// JSON-serialisable result object; `execute` serialises it into the
/// `ToolResult.output` string and stamps `_duration_ms` on it, exactly as
/// the Python `execute` does.
///
/// `caps` is the executor's explicit capability grant (default
/// `DEFAULT_AGENT_CAPS`). `execute` checks the required caps before any
/// dispatch — a tool can never run above the executor's granted caps.
pub struct ToolExecutor {
    /// Working directory: the shell tool runs here and the `remember`
    /// store lives here.
    pub data_dir: PathBuf,
    /// Telegram chat the executor acts in (0 = none).
    pub chat_id: i64,
    /// Delegation depth of the agent that owns this executor.
    pub depth: u32,
    /// Explicit capability grant.
    pub caps: HashSet<String>,
}

impl ToolExecutor {
    /// Build an executor with the default capability grant.
    /// (`make_executor` with no `caps` in `_tools.py`.)
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            chat_id: 0,
            depth: 0,
            caps: default_agent_caps(),
        }
    }

    /// Grant a custom capability set (replaces the default).
    pub fn with_caps(mut self, caps: HashSet<String>) -> Self {
        self.caps = caps;
        self
    }

    /// Bind the executor to a Telegram chat.
    pub fn with_chat_id(mut self, chat_id: i64) -> Self {
        self.chat_id = chat_id;
        self
    }

    /// Set the delegation depth of the owning agent.
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Cap tokens `tool_name` needs but this executor was not granted.
    /// (`_missing_caps` in `_tools.py`, including the arg-level escalation.)
    pub fn missing_caps(&self, tool_name: &str, arguments: &Value) -> Vec<String> {
        let Some(required) = tool_required_caps(tool_name, arguments) else {
            return vec![]; // unknown tool — handled by the dispatch miss below
        };
        let mut missing: Vec<String> = required
            .into_iter()
            .filter(|c| !self.caps.contains(*c))
            .map(|c| c.to_string())
            .collect();
        missing.sort();
        missing
    }

    /// Dispatch a tool call. Returns the result (an error `ToolResult`
    /// when the tool is denied, unknown, or fails — mirroring the Python
    /// `execute`, which returns `{"error": ...}` dicts rather than raising).
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let t0 = Instant::now();
        let arguments = &call.arguments;
        info!(
            "[AGENT:TOOLS] Executing: {}({:?})",
            call.name,
            arguments
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        );

        let missing = self.missing_caps(&call.name, arguments);
        let mut result = if !missing.is_empty() {
            warn!(
                "[AGENT:TOOLS] DENIED {} — missing caps: {:?}",
                call.name, missing
            );
            json!({
                "error": format!(
                    "Tool '{}' blocked by capability policy (missing: {})",
                    call.name,
                    missing.join(", ")
                ),
                "denied": true,
                "missing_caps": missing,
            })
        } else {
            self.dispatch(&call.name, arguments).await
        };

        let duration_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("_duration_ms".to_string(), json!(duration_ms));
        }
        let is_error = result.get("error").is_some();
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            name: call.name.clone(),
            output: serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
            is_error,
        })
    }

    /// Dispatch to the per-tool implementation.
    async fn dispatch(&self, tool_name: &str, arguments: &Value) -> Value {
        match tool_name {
            "shell_execute" => self.tool_shell_execute(arguments).await,
            "code_search" => self.tool_code_search(arguments).await,
            "get_traces" => self.tool_get_traces(arguments).await,
            "update_check" => self.tool_update_check(arguments).await,
            "install_package" => self.tool_install_package(arguments).await,
            "process_image" => self.tool_process_image(arguments).await,
            "send_message" => self.tool_send_message(arguments).await,
            "send_text_file" => self.tool_send_text_file(arguments).await,
            "get_bot_status" => self.tool_get_bot_status(arguments).await,
            "remember" => self.tool_remember(arguments).await,
            "web_search" => self.tool_web_search(arguments).await,
            "tg_route_send" => self.tool_tg_route_send(arguments).await,
            "tg_read" => self.tool_tg_read(arguments).await,
            "plugin_manager" => self.tool_plugin_manager(arguments).await,
            "raw_mtproto" => self.tool_raw_mtproto(arguments).await,
            "web_fetch" => self.tool_web_fetch(arguments).await,
            "think" => self.tool_think(arguments).await,
            "delegate" => self.tool_delegate(arguments).await,
            "ask_user" => self.tool_ask_user(arguments).await,
            "confirm_action" => self.tool_confirm_action(arguments).await,
            "collect_fields" => self.tool_collect_fields(arguments).await,
            _ => err(format!("Unknown tool: {tool_name}")),
        }
    }

    /// The OpenAI-style function schemas for every tool (name / description
    /// / parameters), matching `TOOL_SCHEMAS` + `INTERACTIVE_SCHEMAS` in the
    /// Python source.
    pub fn tool_definitions(&self) -> Vec<Value> {
        tool_schemas()
    }
}

/// Build a `ToolExecutor`. Pass custom caps via `ToolExecutor::new(..)
/// .with_caps(..)` to grant beyond (or restrict below) `DEFAULT_AGENT_CAPS`
/// — e.g. adding `update.apply`.
/// (`make_executor` in `_tools.py`.)
pub fn make_executor(data_dir: impl Into<PathBuf>) -> ToolExecutor {
    ToolExecutor::new(data_dir)
}

impl ToolExecutor {
    // ── shell_execute ──
    async fn tool_shell_execute(&self, args: &Value) -> Value {
        let Some(command) = arg_str(args, "command") else {
            return err("shell_execute requires a 'command' argument");
        };
        let timeout = clamp_shell_timeout(arg_f64(args, "timeout", AGENT_SHELL_DEFAULT_TIMEOUT));

        if let Some(reason) = shell_security_check(command) {
            return json!({
                "stdout": "",
                "stderr": format!("[BLOCKED] {reason}"),
                "exit_code": -1,
                "cwd": self.data_dir.to_string_lossy(),
                "ok": false,
            });
        }

        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.data_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return err(format!("failed to spawn shell: {e}")),
        };

        // Read the pipes concurrently with waiting: waiting alone can
        // deadlock if the child fills a pipe buffer. `wait()` borrows the
        // child, so on timeout the run future is dropped, the borrow
        // releases, and the child can be killed.
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let run = async {
            let wait_fut = child.wait();
            let read_out = async {
                let mut v = Vec::new();
                if let Some(mut p) = stdout_pipe.take() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(&mut p, &mut v).await;
                }
                v
            };
            let read_err = async {
                let mut v = Vec::new();
                if let Some(mut p) = stderr_pipe.take() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(&mut p, &mut v).await;
                }
                v
            };
            let (status, out, err) = tokio::join!(wait_fut, read_out, read_err);
            (status, out, err)
        };

        match tokio::time::timeout(Duration::from_secs_f64(timeout), run).await {
            Ok((Ok(status), out, err)) => {
                let stdout = String::from_utf8_lossy(&out);
                let stderr = String::from_utf8_lossy(&err);
                json!({
                    "stdout": truncate_chars(&stdout, 2000),
                    "stderr": truncate_chars(&stderr, 500),
                    "exit_code": status.code().unwrap_or(-1),
                    "cwd": self.data_dir.to_string_lossy(),
                    "ok": status.success(),
                })
            }
            Ok((Err(e), _, _)) => err(format!("shell execution failed: {e}")),
            Err(_) => {
                // Timeout: kill the child, exactly as the Python shell layer
                // kills a command past its clamped timeout.
                let _ = child.kill().await;
                let _ = child.wait().await;
                json!({
                    "stdout": "",
                    "stderr": format!("command timed out after {timeout}s"),
                    "exit_code": -1,
                    "cwd": self.data_dir.to_string_lossy(),
                    "ok": false,
                    "error": format!("command timed out after {timeout}s"),
                })
            }
        }
    }

    // ── code_search ──
    async fn tool_code_search(&self, _args: &Value) -> Value {
        err(
            "code_search is unavailable: it needs the tracer's code-search index \
             (pyUltroid.tracer.tr.search.nl_query), which has no Rust counterpart \
             in this crate yet",
        )
    }

    // ── get_traces ──
    async fn tool_get_traces(&self, _args: &Value) -> Value {
        err(
            "get_traces is unavailable: it needs the tracer's live trace store \
             (pyUltroid.tracer.tr.live), which has no Rust counterpart in this \
             crate yet",
        )
    }

    // ── update_check ──
    async fn tool_update_check(&self, _args: &Value) -> Value {
        err("update_check is unavailable: it needs the updater \
             (pyUltroid.updater.up.check/apply), which has no Rust counterpart \
             in this crate yet")
    }

    // ── install_package ──
    async fn tool_install_package(&self, _args: &Value) -> Value {
        err(
            "install_package is unavailable: it needs the resource extender \
             (pyUltroid.extender.ex.resources.install_dep), which has no Rust \
             counterpart in this crate yet",
        )
    }

    // ── process_image ──
    async fn tool_process_image(&self, args: &Value) -> Value {
        // The operation table is ported exactly; only the imaging engine
        // behind it is missing.
        const TRANSFORM_OPS: &[&str] = &[
            "grayscale",
            "blur",
            "sketch",
            "cartoon",
            "rotate",
            "caption",
            "meme",
            "glitch",
            "vintage",
            "invert",
            "pixelate",
        ];
        const GENERATION_OPS: &[&str] = &["error_card", "code_image", "text_image"];
        const ALL_OPS: &[&str] = &[
            "grayscale",
            "blur",
            "sketch",
            "cartoon",
            "rotate",
            "caption",
            "meme",
            "glitch",
            "vintage",
            "invert",
            "pixelate",
            "error_card",
            "code_image",
            "text_image",
            "collage",
        ];

        let Some(operation) = arg_str(args, "operation") else {
            return err("process_image requires an 'operation' argument");
        };
        if !ALL_OPS.contains(&operation) {
            return err(format!("Unknown operation: {operation}"));
        }
        // "collage" is in the schema enum but has no entry in the Python
        // op_map, so the source would raise KeyError inside execute and
        // return {"error": ...}; report it the same way.
        if operation == "collage" {
            return err("Unknown operation: collage");
        }
        let image_url = arg_str(args, "image_url");
        if !GENERATION_OPS.contains(&operation) && image_url.is_none() {
            return err("image_url required for this operation");
        }
        let _ = TRANSFORM_OPS;
        let _ = arg_obj(args, "params");
        err("process_image is unavailable: it needs the imaging engine \
             (pyUltroid.imaging.Imaging), and no image crate is among this \
             crate's dependencies")
    }

    // ── send_message ──
    async fn tool_send_message(&self, _args: &Value) -> Value {
        err("send_message is unavailable: it needs the Telegram bridge \
             (agent._tgbridge.send_via_master) and the master Telegram client, \
             which have no Rust counterpart in this crate yet")
    }

    // ── send_text_file ──
    async fn tool_send_text_file(&self, args: &Value) -> Value {
        // All local validation from the Python source, in the same order.
        let name = arg_str(args, "filename").unwrap_or("").trim().to_string();
        let filename_re = Regex::new(r"(?i)^[A-Za-z0-9][A-Za-z0-9._ \-]{0,95}\.(?:txt|md)$")
            .expect("hardcoded filename pattern must compile");
        if !filename_re.is_match(&name) {
            return err("Use a simple .txt or .md filename without a path.");
        }
        let Some(content) = arg_str(args, "content") else {
            return err("The attachment must contain non-empty plain text.");
        };
        if content.trim().is_empty() || content.contains('\x00') {
            return err("The attachment must contain non-empty plain text.");
        }
        if content.len() > 256 * 1024 {
            return err("Text attachments are limited to 256 KiB.");
        }
        if let Some(caption) = arg_str(args, "caption") {
            if caption.chars().count() > 900 {
                return err("The plain-text caption must be at most 900 characters.");
            }
        }
        if self.chat_id == 0 {
            return err("There is no current chat to receive the attachment.");
        }
        err(
            "send_text_file is unavailable: the validated attachment cannot be \
             delivered without the Telegram bridge \
             (agent._tgbridge.send_file_via_master), which has no Rust \
             counterpart in this crate yet",
        )
    }

    // ── get_bot_status ──
    async fn tool_get_bot_status(&self, _args: &Value) -> Value {
        err(
            "get_bot_status is unavailable: it needs the updater's git state \
             and the tracer index stats (pyUltroid.updater.up.git / \
             pyUltroid.tracer.tr.index.stats), which have no Rust counterpart \
             in this crate yet",
        )
    }

    // ── remember ──
    // The Python source stores in Redis under `agent:memory:<key>`. No
    // Redis dependency exists in this crate, so the store is a JSON file
    // under the executor's data dir with the same key prefix and the same
    // store/recall/forget semantics.
    fn memory_path(&self) -> PathBuf {
        self.data_dir.join("agent_memory.json")
    }

    fn memory_load(&self) -> Result<Map<String, Value>> {
        let path = self.memory_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(serde_json::from_str::<Map<String, Value>>(&text).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(Error::Other(format!("memory store read failed: {e}"))),
        }
    }

    fn memory_save(&self, store: &Map<String, Value>) -> Result<()> {
        let path = self.memory_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Other(format!("memory store mkdir failed: {e}")))?;
        }
        std::fs::write(
            &path,
            serde_json::to_string(store).unwrap_or_else(|_| "{}".into()),
        )
        .map_err(|e| Error::Other(format!("memory store write failed: {e}")))?;
        Ok(())
    }

    async fn tool_remember(&self, args: &Value) -> Value {
        let action = arg_str(args, "action").unwrap_or("");
        let key = arg_str(args, "key").unwrap_or("");
        let value = arg_str(args, "value").unwrap_or("");
        let r_key = format!("agent:memory:{key}");
        let mut store = match self.memory_load() {
            Ok(store) => store,
            Err(e) => return err(e.to_string()),
        };
        match action {
            "store" => {
                store.insert(r_key, Value::String(value.to_string()));
                if let Err(e) = self.memory_save(&store) {
                    return err(e.to_string());
                }
                json!({ "ok": true, "stored": key })
            }
            "recall" => {
                let val = store
                    .get(&r_key)
                    .and_then(Value::as_str)
                    .unwrap_or("(not found)");
                json!({ "ok": true, "key": key, "value": val })
            }
            "forget" => {
                store.remove(&r_key);
                if let Err(e) = self.memory_save(&store) {
                    return err(e.to_string());
                }
                json!({ "ok": true, "forgotten": key })
            }
            _ => err(format!("Unknown action: {action}")),
        }
    }

    // ── web_search ──
    async fn tool_web_search(&self, args: &Value) -> Value {
        let query = match arg_str(args, "query") {
            Some(q) => q.to_string(),
            None => return err("web_search requires a 'query' argument"),
        };
        let max_results = arg_i64(args, "max_results", 5).clamp(1, 10) as usize;
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return json!({ "error": e.to_string(), "query": query }),
        };
        let resp = match client
            .get("https://ddg-api.awam.repl.co/api/search")
            .query(&[
                ("query", query.as_str()),
                ("maxNumResults", &max_results.to_string()),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return json!({ "error": e.to_string(), "query": query }),
        };
        let data: Value = match resp.json().await {
            Ok(d) => d,
            Err(e) => return json!({ "error": e.to_string(), "query": query }),
        };
        let Some(list) = data.as_array() else {
            return json!({ "error": "unexpected search response", "query": query });
        };
        let results: Vec<Value> = list
            .iter()
            .take(max_results)
            .map(|d| {
                json!({
                    "title": d.get("Title").and_then(Value::as_str).unwrap_or(""),
                    "snippet": d.get("Snippet").and_then(Value::as_str).unwrap_or(""),
                    "url": d.get("Link").and_then(Value::as_str).unwrap_or(""),
                })
            })
            .collect();
        json!({ "query": query, "results": results })
    }

    // ── tg_route_send ──
    async fn tool_tg_route_send(&self, _args: &Value) -> Value {
        err(
            "tg_route_send is unavailable: it needs the Telegram bridge \
             (agent._tgbridge.send_via_master) and the master client router, \
             which have no Rust counterpart in this crate yet",
        )
    }

    // ── tg_read ──
    async fn tool_tg_read(&self, _args: &Value) -> Value {
        err("tg_read is unavailable: it needs the Telegram bridge \
             (agent._tgbridge.read_recent) and the master client router, \
             which have no Rust counterpart in this crate yet")
    }

    // ── plugin_manager ──
    async fn tool_plugin_manager(&self, args: &Value) -> Value {
        let action = arg_str(args, "action").unwrap_or("list").to_lowercase();
        match action.as_str() {
            "list" => err(
                "plugin_manager list is unavailable: it needs the plugin loader \
                 registry and tracer index stats \
                 (pyUltroid._core.loader.registry / pyUltroid.tracer.tr.index.stats), \
                 which have no Rust counterpart in this crate yet",
            ),
            "load" | "unload" | "reload" => {
                let plugin = arg_str(args, "plugin").unwrap_or("").trim();
                if plugin.is_empty() {
                    return err(format!("plugin name required for action='{action}'"));
                }
                err("plugin_manager is unavailable: it needs the plugin loader \
                     (pyUltroid._core.loader), which has no Rust counterpart in \
                     this crate yet")
            }
            _ => err(format!("Unknown action '{action}'")),
        }
    }

    // ── raw_mtproto ──
    async fn tool_raw_mtproto(&self, _args: &Value) -> Value {
        err(
            "raw_mtproto is unavailable: it needs the master Telegram client \
             (pyUltroid.master.route) and the raw-request allowlist invoker \
             (agent._tgbridge.invoke_raw), which have no Rust counterpart in \
             this crate yet",
        )
    }

    // ── web_fetch ──
    async fn tool_web_fetch(&self, args: &Value) -> Value {
        let Some(url) = arg_str(args, "url") else {
            return err("web_fetch requires a 'url' argument");
        };
        let url = url.to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return err("Only http/https URLs are fetched");
        }
        let max_chars = arg_i64(args, "max_chars", 4000).clamp(200, 20000) as usize;
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => return json!({ "error": e.to_string(), "url": url }),
        };
        let body = match client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (compatible; UltroidAgent)")
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().as_u16() >= 400 {
                    return json!({ "error": format!("HTTP {}", resp.status()), "url": url });
                }
                match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        return json!({ "error": format!("{}: {e}", "reqwest::Error"), "url": url });
                    }
                }
            }
            Err(e) => return json!({ "error": format!("{}: {e}", "reqwest::Error"), "url": url }),
        };
        // Crude HTML-to-text: drop script/style blocks, then tags, then
        // collapse whitespace. Good enough for the model to read content.
        // (regex has no backreferences, so script and style are stripped
        // by two separate patterns instead of the Python `</\1>` form.)
        let script_re = Regex::new(r"(?is)<script[^>]*>.*?</script>")
            .expect("hardcoded fetch pattern must compile");
        let style_re = Regex::new(r"(?is)<style[^>]*>.*?</style>")
            .expect("hardcoded fetch pattern must compile");
        let tag_re = Regex::new(r"(?s)<[^>]+>").expect("hardcoded fetch pattern must compile");
        let ws_re = Regex::new(r"\s+").expect("hardcoded fetch pattern must compile");
        let text = script_re.replace_all(&body, " ");
        let text = style_re.replace_all(&text, " ");
        let text = tag_re.replace_all(&text, " ");
        let text = ws_re.replace_all(&text, " ").trim().to_string();
        let chars = text.chars().count();
        json!({
            "ok": true,
            "url": url,
            "chars": chars,
            "text": truncate_chars(&text, max_chars),
        })
    }

    // ── think ──
    async fn tool_think(&self, args: &Value) -> Value {
        let thought = arg_str(args, "thought").unwrap_or("");
        json!({ "ok": true, "noted": truncate_chars(thought, 2000) })
    }

    // ── ask_user / confirm_action / collect_fields ──
    async fn tool_ask_user(&self, _args: &Value) -> Value {
        err("ask_user is unavailable: it needs the interactive bridge \
             (agent._interactive.InteractiveTools over the shared AskBridge \
             from _bridge.py), which has no Rust counterpart in this crate yet")
    }

    async fn tool_confirm_action(&self, _args: &Value) -> Value {
        err(
            "confirm_action is unavailable: it needs the interactive bridge \
             (agent._interactive.InteractiveTools over the shared AskBridge \
             from _bridge.py), which has no Rust counterpart in this crate yet",
        )
    }

    async fn tool_collect_fields(&self, _args: &Value) -> Value {
        err(
            "collect_fields is unavailable: it needs the interactive bridge \
             (agent._interactive.InteractiveTools over the shared AskBridge \
             from _bridge.py), which has no Rust counterpart in this crate yet",
        )
    }

    // ── delegate ──
    async fn tool_delegate(&self, args: &Value) -> Value {
        if self.depth >= MAX_AGENT_DEPTH {
            return json!({
                "error": format!(
                    "Delegation depth limit reached ({MAX_AGENT_DEPTH}); handle this task directly."
                ),
                "denied": true,
            });
        }
        let _ = arg_i64(args, "max_rounds", 3).clamp(1, 5);
        let _ = arg_str(args, "task").unwrap_or("");
        err("delegate is unavailable: it needs the agent loop itself \
             (agent._agent.UltroidAgent.respond), a sibling module being \
             ported in parallel — there is no sub-agent to spawn yet")
    }
}

/// `TOOL_SCHEMAS` + `INTERACTIVE_SCHEMAS` from `_tools.py` / `_interactive.py`.
fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "shell_execute",
                "description": "Execute a shell command on the Ultroid EC2 instance. Use for file operations, system inspection, git commands, running scripts, and one-off diagnostics. stdout/stderr are returned as text (truncated); the exit code tells you whether the command succeeded. Never use this to read .env or *.session files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The exact shell command to run, as a single string. Quote arguments that contain spaces."
                        },
                        "timeout": {
                            "type": "number",
                            "description": "Maximum seconds to wait before killing the command. Clamped to the range [1, 120]; junk values fall back to 60. Default 60."
                        }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "code_search",
                "description": "Search the live Ultroid plugin codebase. Map a command pattern to the plugin that handles it, extract a function's source, find every DB key a plugin reads/writes, or detect handler conflicts  -  all against the on-disk source, not training data.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural-language query or command pattern, e.g. 'who handles .play', 'show source of play_cmd', 'find all DB keys used by youtube'."
                        }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "get_traces",
                "description": "Read recent execution-trace history. See which handlers ran, who triggered them, and any failures or slow handlers. Useful for diagnosing 'why didn't X work' questions before guessing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filter": {
                            "type": "string",
                            "description": "Optional scope: 'recent' (default), 'failures', 'slow', or a prefix target  -  'plugin:<name>' or 'user:<id>'."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Number of traces to return, clamped to [1, 50]. Default 10."
                        }
                    },
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "update_check",
                "description": "Check whether Ultroid has upstream updates available. Returns commit count, changed files, a restart flag, and a short changelog. Applying the update requires an elevated capability grant and is denied by default.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "apply": {
                            "type": "boolean",
                            "description": "If true, apply the update after checking. Refused unless UPDATE_APPLY was explicitly granted to the executor."
                        }
                    },
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "install_package",
                "description": "Install a Python package via pip for on-demand plugin dependencies. Refused unless RES_INSTALL was explicitly granted  -  on-demand installs are supply-chain sensitive.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "package": {
                            "type": "string",
                            "description": "Package name with an optional version specifier, e.g. 'pillow', 'requests>=2.31', 'yt-dlp==2024.4.9'."
                        }
                    },
                    "required": ["package"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "process_image",
                "description": "Process or generate an image in a single operation. Transform operations need an input image_url; generation operations (error_card, code_image, text_image) build from their params. The produced image is returned for delivery.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["grayscale", "blur", "sketch", "cartoon", "rotate", "caption", "meme", "glitch", "vintage", "invert", "pixelate", "error_card", "code_image", "text_image", "collage"],
                            "description": "The image operation to perform."
                        },
                        "params": {
                            "type": "object",
                            "description": "Operation-specific parameters: text/author (caption, meme, text_image), radius (blur), degrees (rotate), intensity (glitch), size (pixelate), position (caption), code+language (code_image), title+message+traceback (error_card)."
                        },
                        "image_url": {
                            "type": "string",
                            "description": "URL of the input image, required for transform operations. Omit for pure generation operations."
                        }
                    },
                    "required": ["operation"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "send_message",
                "description": "Send an ordinary plain-text Telegram message as the userbot. This tool never sends a native RichMessage. Use tg_route_send with rich=true only when native formatting is needed and the selected account is a bot or confirmed Premium user.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "chat_id": {
                            "type": "integer",
                            "description": "Telegram chat ID to send the message to."
                        },
                        "text": {
                            "type": "string",
                            "description": "Plain-text message body."
                        }
                    },
                    "required": ["chat_id", "text"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "send_text_file",
                "description": "Create a UTF-8 .txt or .md attachment from the supplied text and send it only to the current chat. This uses in-memory content; filesystem paths and other destinations are not accepted.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filename": {
                            "type": "string",
                            "description": "A simple filename ending in .txt or .md; no path."
                        },
                        "content": {
                            "type": "string",
                            "description": "The UTF-8 text to put in the attachment."
                        },
                        "caption": {
                            "type": "string",
                            "description": "Optional plain-text caption, up to 900 characters."
                        }
                    },
                    "required": ["filename", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "get_bot_status",
                "description": "Get Ultroid's own running state: current version, git branch, loaded plugin/handler counts, and an uptime hint. No parameters.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "remember",
                "description": "Store, recall, or forget a fact that persists across conversations. The agent's own memory namespace (agent:memory:*); use to retain user preferences, project context, or cross-session facts.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["store", "recall", "forget"],
                            "description": "What to do with the memory entry."
                        },
                        "key": {
                            "type": "string",
                            "description": "Memory key name to store/recall/forget."
                        },
                        "value": {
                            "type": "string",
                            "description": "Value to store (only for action=store)."
                        }
                    },
                    "required": ["action", "key"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for current information via DuckDuckGo. Returns titled snippets with URLs. Use before answering time-sensitive or factual questions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query."
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum results to return, clamped to [1, 10]. Default 5."
                        }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "tg_route_send",
                "description": "Send a Telegram message through the master client router. The route chooses which account sends: 'auto' follows the ambient routing rules (business connection, then user, then bot), or pin 'user', 'bot', or 'business'. Sends ordinary plain text by default. Set rich=true to request a native RichMessage; bots may send these and user accounts require confirmed Telegram Premium. Unsupported rich sends are rejected without retry. Set rich=false whenever plain text is preferred. For plain sends, parse_mode only converts markup to readable text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "chat": {
                            "type": "string",
                            "description": "Destination chat: numeric id, @username, or t.me link."
                        },
                        "text": {
                            "type": "string",
                            "description": "Message text to send."
                        },
                        "route": {
                            "type": "string",
                            "enum": ["auto", "user", "bot", "business"],
                            "description": "Which account sends. Default 'auto'."
                        },
                        "reply_to": {
                            "type": "integer",
                            "description": "Message id to reply to, optional."
                        },
                        "parse_mode": {
                            "type": "string",
                            "description": "RichMessage format: 'html' or 'markdown' ('md' is accepted). Omit to detect the format from the text."
                        },
                        "rich": {
                            "type": "boolean",
                            "description": "Use native RichMessage formatting. Default false sends ordinary plain text and is always allowed."
                        },
                        "link_preview": {
                            "type": "boolean",
                            "description": "Allow link preview. Default true."
                        }
                    },
                    "required": ["chat", "text"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "tg_read",
                "description": "Read the most recent messages of a Telegram chat. Returns id, sender, date, text, and reply metadata per message. Use to ground answers in live conversation state instead of guessing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "chat": {
                            "type": "string",
                            "description": "Chat to read: numeric id, @username, or t.me link."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "How many messages, clamped to [1, 100]. Default 20."
                        },
                        "route": {
                            "type": "string",
                            "enum": ["user", "bot", "business"],
                            "description": "Which account reads. Defaults to the user account."
                        }
                    },
                    "required": ["chat"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "plugin_manager",
                "description": "Inspect and drive the live plugin registry. 'list' returns active plugin names with handler counts. 'load' loads a plugin file by name, 'unload' removes it from the running bot, 'reload' does both. Load/unload/reload are privileged and refused unless the matching capability was granted.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["list", "load", "unload", "reload"],
                            "description": "Registry operation. Default 'list'."
                        },
                        "plugin": {
                            "type": "string",
                            "description": "Plugin name for load/unload/reload (ignored for list)."
                        }
                    },
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "raw_mtproto",
                "description": "Invoke a read-only raw MTProto request through the active client. Only allowlisted request types are permitted (for example messages.GetHistoryRequest, channels.GetFullChannelRequest, users.GetUsersRequest); everything else, especially any write request, is refused. Arguments must match the Telethon constructor signature.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "function": {
                            "type": "string",
                            "description": "Request class name, e.g. 'messages.GetHistoryRequest'."
                        },
                        "arguments": {
                            "type": "object",
                            "description": "Keyword arguments for the request constructor."
                        }
                    },
                    "required": ["function"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a web page and return readable text (HTML stripped, truncated). Use after web_search to read a specific result. http/https only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Absolute http(s) URL to fetch."
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "Maximum characters of text returned, clamped to [200, 20000]. Default 4000."
                        }
                    },
                    "required": ["url"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "think",
                "description": "Private scratchpad. Record a short plan, an interim conclusion, or the next step between tool rounds. No side effects; the content is echoed back as a tool result so it stays in the transcript.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "thought": {
                            "type": "string",
                            "description": "The note to record."
                        }
                    },
                    "required": ["thought"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "delegate",
                "description": "Delegate a bounded subtask to a fresh sub-agent with its own context window. The sub-agent gets the same tool permissions minus delegation itself, a small round budget, and returns only its final answer. Use for parallelizable research or multi-step work that would bloat this conversation. Refused at the maximum nesting depth.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The complete, self-contained subtask for the sub-agent."
                        },
                        "max_rounds": {
                            "type": "integer",
                            "description": "Tool-round budget for the sub-agent, clamped to [1, 5]. Default 3."
                        }
                    },
                    "required": ["task"]
                }
            }
        }),
        // ── INTERACTIVE_SCHEMAS (from `_interactive.py`) ──
        json!({
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": "Ask the user a question and wait for their answer. Use this when you genuinely cannot continue without something only they know - their name, a preference, a missing path, which of two options they meant. This blocks the turn until they answer or the TTL expires, so ask only what you need. If you supply choices, they get a tap-to-answer keyboard; otherwise they can just type. On expiry you get timed_out=true and must decide how to proceed - do not retry the same question.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question, phrased for a human."
                        },
                        "choices": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional tap-to-answer options, at most 12. Omit to let them type freely."
                        },
                        "note": {
                            "type": "string",
                            "description": "Optional extra context under the question."
                        },
                        "ttl": {
                            "type": "number",
                            "description": "Seconds to wait, clamped to [5, 900]. Default 120."
                        }
                    },
                    "required": ["question"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "confirm_action",
                "description": "Ask the user to confirm something you are about to do that cannot be undone - deleting, force-updating, sending on their behalf. Returns their yes/no. Use it before a destructive shell command or a bulk send, not for routine work.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "what": {
                            "type": "string",
                            "description": "What you are about to do, concretely."
                        },
                        "yes_label": { "type": "string", "description": "Confirm button text." },
                        "no_label": { "type": "string", "description": "Cancel button text." },
                        "ttl": { "type": "number", "description": "Seconds to wait." }
                    },
                    "required": ["what"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "collect_fields",
                "description": "Collect several answers in one go - a short form. Each field is asked in turn with its own keyboard. Use it instead of repeating ask_user when you need more than one thing, so the user is interrupted once rather than five times.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "fields": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "Field label." },
                                    "question": { "type": "string" },
                                    "choices": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    }
                                },
                                "required": ["name", "question"]
                            },
                            "description": "Between 1 and 5 fields, asked in order."
                        },
                        "ttl": { "type": "number", "description": "Seconds per field." }
                    },
                    "required": ["fields"]
                }
            }
        }),
    ]
}

// ── ToolPipeline ───────────────────────────────────────────────────────────

/// One step in a `ToolPipeline`: the tool, its declared arguments, and the
/// feed map (`result_key -> argument_name`) that copies values out of the
/// previous step's result into this step's arguments.
struct PipelineStep {
    tool: String,
    arguments: Value,
    feed: HashMap<String, String>,
}

/// One executed pipeline step: the tool, the arguments actually dispatched
/// (declared args plus any fed-forward values), and the result.
pub struct PipelineOutcome {
    pub tool: String,
    pub arguments: Value,
    pub result: Value,
}

/// Chain tool calls through a single `ToolExecutor`, optionally feeding each
/// step's output into the next step's arguments.
///
/// (`ToolPipeline` in `_tools.py`.)
pub struct ToolPipeline {
    executor: ToolExecutor,
    steps: Vec<PipelineStep>,
}

impl ToolPipeline {
    pub fn new(executor: ToolExecutor) -> Self {
        Self {
            executor,
            steps: Vec::new(),
        }
    }

    /// Append a step. `feed` maps a key in the previous step's result to an
    /// argument name for this step. Returns `self` for chaining.
    /// (`ToolPipeline.add` in `_tools.py`.)
    pub fn add(
        &mut self,
        tool: impl Into<String>,
        arguments: Option<Value>,
        feed: Option<HashMap<String, String>>,
    ) -> &mut Self {
        self.steps.push(PipelineStep {
            tool: tool.into(),
            arguments: arguments.unwrap_or_else(|| json!({})),
            feed: feed.unwrap_or_default(),
        });
        self
    }

    /// Execute every added step in order, returning per-step outcomes.
    /// (`ToolPipeline.run` in `_tools.py`.)
    pub async fn run_steps(&self) -> Vec<PipelineOutcome> {
        let mut outcomes: Vec<PipelineOutcome> = Vec::new();
        let mut prev: Option<Value> = None;
        for (i, step) in self.steps.iter().enumerate() {
            let mut args = step.arguments.clone();
            if let Some(prev_result) = &prev {
                if let Some(obj) = args.as_object_mut() {
                    for (result_key, arg_name) in &step.feed {
                        if let Some(fed) = prev_result.get(result_key) {
                            obj.insert(arg_name.clone(), fed.clone());
                        }
                    }
                }
            }
            let call = ToolCall {
                id: format!("pipeline-{i}"),
                name: step.tool.clone(),
                arguments: args.clone(),
            };
            let executed = self.executor.execute(&call).await;
            let result = match executed {
                Ok(r) => serde_json::from_str::<Value>(&r.output)
                    .unwrap_or_else(|_| json!({ "error": r.output })),
                Err(e) => err(e.to_string()),
            };
            outcomes.push(PipelineOutcome {
                tool: step.tool.clone(),
                arguments: args,
                result: result.clone(),
            });
            prev = Some(result);
        }
        outcomes
    }

    /// Run a flat list of tool calls in order through the executor.
    /// (Contract API for the agent loop; the Python `ToolPipeline` has no
    /// direct equivalent — it chains via `add`/`run_steps` above.)
    pub async fn run(&self, calls: &[ToolCall]) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            match self.executor.execute(call).await {
                Ok(r) => results.push(r),
                Err(e) => results.push(ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    output: serde_json::to_string(&err(e.to_string()))
                        .unwrap_or_else(|_| "{}".to_string()),
                    is_error: true,
                }),
            }
        }
        results
    }
}
