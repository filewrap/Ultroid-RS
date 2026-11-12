//! Loop guards: the layer that stops a turn from spinning — port of
//! `pyUltroid/agent/_loop.py`.
//!
//! The agentic loop is correct about *what* to do each round and has no
//! opinion about *how many* rounds are sensible. A tool that fails
//! deterministically, a model that keeps re-issuing the same call, or a
//! turn that makes no progress between rounds all burn rounds until the
//! cap. This is the same idea as Hermes' `ToolCallGuardrailController`:
//! a decision per call, made *before* dispatch, that either allows,
//! warns, blocks or halts the turn.
//!
//! A blocked call is not dropped. It comes back to the model as a
//! synthetic tool result carrying the reason, so the model gets to
//! choose what to do instead of silently losing the result it asked for.
//!
//! Hashing note: Python hashes signatures with SHA-256; this crate has no
//! SHA-256 dependency, so signatures use FNV-1a-64 rendered as 16 hex
//! digits. The shape (`call_` + 16 hex chars for tool-call ids, 16 hex
//! chars for signatures) and the stability properties match — signatures
//! are per-turn, in-memory only, never persisted or compared across
//! processes.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{Value, json};

/// What to do about one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    Allow,
    Warn,
    Block,
    Halt,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Warn => "warn",
            Decision::Block => "block",
            Decision::Halt => "halt",
        }
    }
}

/// Identical failing signature: past this the repetition is the bug.
pub const EXACT_FAIL_WARN: u32 = 2;
pub const EXACT_FAIL_BLOCK: u32 = 5;
pub const SAME_TOOL_WARN: u32 = 3;
pub const SAME_TOOL_HALT: u32 = 8;
pub const NO_PROGRESS_WARN: u32 = 2;
pub const NO_PROGRESS_BLOCK: u32 = 5;

/// The decision for one call, plus the reason the model is told.
#[derive(Debug, Clone)]
pub struct GuardVerdict {
    pub decision: Decision,
    pub tool_name: String,
    pub code: String,
    pub message: String,
    pub count: u32,
    pub signature: String,
}

impl GuardVerdict {
    pub fn new(decision: Decision, tool_name: impl Into<String>) -> Self {
        GuardVerdict {
            decision,
            tool_name: tool_name.into(),
            code: String::new(),
            message: String::new(),
            count: 0,
            signature: String::new(),
        }
    }

    pub fn blocked(&self) -> bool {
        matches!(self.decision, Decision::Block | Decision::Halt)
    }

    pub fn halt(&self) -> bool {
        self.decision == Decision::Halt
    }

    /// The synthetic tool result a blocked call produces.
    ///
    /// Shaped like a real tool result so the provider serializers treat
    /// it as one; the model is told plainly not to retry the same call,
    /// because the whole point is that repeating it is what got blocked.
    pub fn as_result(&self) -> Value {
        json!({
            "error": if self.message.is_empty() { "blocked by loop guard".to_string() } else { self.message.clone() },
            "guardrail": self.code,
            "blocked": true,
            "tool": self.tool_name,
            "occurrences": self.count,
        })
    }
}

/// FNV-1a-64, rendered as 16 lowercase hex digits.
fn fnv_hex(data: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Render a JSON value with object keys sorted recursively, so key order
/// in the arguments never changes the signature.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(*k).unwrap(),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// A stable identity for one call, so repeats are recognisable.
///
/// Key order in the arguments must not change the signature, so the
/// payload is sorted before hashing.
pub fn signature_of(tool_name: &str, arguments: &Value) -> String {
    let payload = canonical_json(arguments);
    fnv_hex(&format!("{tool_name}:{payload}"))
}

/// Whether a tool result counts as a failure.
///
/// Anything the executor marked as an error, a denial, a guardrail
/// block, or a bare exception string. A result that merely *looks*
/// alarming is not a failure — only an explicit signal is.
pub fn is_failure(result: &Value) -> bool {
    match result {
        Value::Object(map) => {
            if map
                .get("error")
                .map(|v| {
                    !v.is_null() && *v != Value::Bool(false) && *v != Value::String(String::new())
                })
                .unwrap_or(false)
            {
                return true;
            }
            if map.get("denied").and_then(|v| v.as_bool()).unwrap_or(false)
                || map
                    .get("blocked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                return true;
            }
            if let Some(ok) = map.get("ok").and_then(|v| v.as_bool()) {
                return !ok;
            }
            if let Some(code) = map.get("exit_code").and_then(|v| v.as_i64()) {
                return code != 0;
            }
            false
        }
        Value::String(s) => s.starts_with("Error"),
        _ => false,
    }
}

/// Whether a sibling `ToolResult` (its `is_error` flag plus its JSON
/// output) counts as a failure.
pub fn is_failure_result(is_error: bool, output: &Value) -> bool {
    is_error || is_failure(output)
}

/// Per-turn stall detection.
///
/// One instance per turn. It is not thread-safe and does not need to be:
/// the loop drives it from a single coroutine, and a shared instance
/// across turns would leak one turn's failures into the next.
pub struct LoopGuard {
    pub exact_fail_warn: u32,
    pub exact_fail_block: u32,
    pub same_tool_warn: u32,
    pub same_tool_halt: u32,
    pub no_progress_warn: u32,
    pub no_progress_block: u32,
    fail_counts: HashMap<String, u32>,
    tool_fail_counts: HashMap<String, u32>,
    tool_calls: HashMap<String, u32>,
    pending_signatures: HashSet<String>,
    last_progress: Instant,
    stale_rounds: u32,
    round_repeated: bool,
    pub halted_reason: String,
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopGuard {
    pub fn new() -> Self {
        LoopGuard {
            exact_fail_warn: EXACT_FAIL_WARN,
            exact_fail_block: EXACT_FAIL_BLOCK,
            same_tool_warn: SAME_TOOL_WARN,
            same_tool_halt: SAME_TOOL_HALT,
            no_progress_warn: NO_PROGRESS_WARN,
            no_progress_block: NO_PROGRESS_BLOCK,
            fail_counts: HashMap::new(),
            tool_fail_counts: HashMap::new(),
            tool_calls: HashMap::new(),
            pending_signatures: HashSet::new(),
            last_progress: Instant::now(),
            stale_rounds: 0,
            round_repeated: false,
            halted_reason: String::new(),
        }
    }

    /// Decide whether `tool_name` may run at all.
    ///
    /// The no-progress check lives here rather than after the fact,
    /// because it is the only way to stop the round that would
    /// otherwise be the sixth identical one.
    pub fn before_call(&mut self, tool_name: &str, arguments: &Value) -> GuardVerdict {
        let signature = signature_of(tool_name, arguments);
        if self.pending_signatures.contains(&signature) {
            return GuardVerdict {
                decision: Decision::Block,
                tool_name: tool_name.to_string(),
                code: "duplicate_in_flight".to_string(),
                message: "An identical call is already running; wait for its result.".to_string(),
                count: 1,
                signature,
            };
        }
        let failures = self.fail_counts.get(&signature).copied().unwrap_or(0);
        if failures >= self.exact_fail_block {
            self.note_repeat();
            return GuardVerdict {
                decision: Decision::Block,
                tool_name: tool_name.to_string(),
                code: "repeated_failure".to_string(),
                message: format!(
                    "This exact call has failed {failures} times. Repeating it will not help - change the arguments, use a different tool, or report the failure."
                ),
                count: failures,
                signature,
            };
        }
        if failures >= self.exact_fail_warn {
            self.note_repeat();
            self.pending_signatures.insert(signature.clone());
            return GuardVerdict {
                decision: Decision::Warn,
                tool_name: tool_name.to_string(),
                code: "repeated_failure".to_string(),
                message: format!("This exact call has already failed {failures} times."),
                count: failures,
                signature,
            };
        }

        if self.stalled() {
            self.note_repeat();
            return GuardVerdict {
                decision: Decision::Block,
                tool_name: tool_name.to_string(),
                code: "no_progress".to_string(),
                message: format!(
                    "{} consecutive rounds re-ran a call that had already failed. Stop calling tools and answer with what you already know, or say what is missing.",
                    self.no_progress_block
                ),
                count: self.stale_rounds,
                signature,
            };
        }

        let tool_failures = self.tool_fail_counts.get(tool_name).copied().unwrap_or(0);
        if tool_failures >= self.same_tool_halt {
            return GuardVerdict {
                decision: Decision::Halt,
                tool_name: tool_name.to_string(),
                code: "tool_misbehaving".to_string(),
                message: format!(
                    "'{tool_name}' has failed {tool_failures} times. This turn is being stopped."
                ),
                count: tool_failures,
                signature,
            };
        }
        if tool_failures >= self.same_tool_warn {
            self.pending_signatures.insert(signature.clone());
            return GuardVerdict {
                decision: Decision::Warn,
                tool_name: tool_name.to_string(),
                code: "tool_failing".to_string(),
                message: format!("'{tool_name}' has failed {tool_failures} times so far."),
                count: tool_failures,
                signature,
            };
        }
        self.pending_signatures.insert(signature.clone());
        GuardVerdict {
            decision: Decision::Allow,
            tool_name: tool_name.to_string(),
            code: String::new(),
            message: String::new(),
            count: 0,
            signature,
        }
    }

    /// Record one completed call. A success resets the stall timer.
    pub fn observe(&mut self, tool_name: &str, arguments: &Value, result: &Value) {
        let signature = signature_of(tool_name, arguments);
        self.pending_signatures.remove(&signature);
        *self.tool_calls.entry(tool_name.to_string()).or_insert(0) += 1;
        if is_failure(result) {
            *self.fail_counts.entry(signature).or_insert(0) += 1;
            *self
                .tool_fail_counts
                .entry(tool_name.to_string())
                .or_insert(0) += 1;
            return;
        }
        // Any success is progress, whatever else happened this round.
        self.last_progress = Instant::now();
    }

    /// Reset the stall timer explicitly (e.g. after a successful round).
    pub fn note_progress(&mut self) {
        self.last_progress = Instant::now();
    }

    /// Mark the in-flight round as re-running something already failed.
    fn note_repeat(&mut self) {
        self.round_repeated = true;
    }

    /// Whether the turn is re-running a call that already failed.
    ///
    /// Deliberately not "rounds elapsed": a model that tries five
    /// different commands and has them all fail is investigating, not
    /// looping, and blocking it would stop it from finding the one that
    /// works. Only a repeat of a known-bad call counts.
    pub fn stalled(&self) -> bool {
        self.stale_rounds >= self.no_progress_block
    }

    /// Open a round, tracking consecutive rounds that re-ran a bad call.
    ///
    /// A success clears the tally. A round that only tried *new* calls
    /// also clears it — nothing has been proven wrong twice yet.
    pub fn begin_round(&mut self, progressed: bool) {
        if progressed || !self.round_repeated {
            self.stale_rounds = 0;
            if progressed {
                self.note_progress();
            }
            return;
        }
        self.stale_rounds += 1;
        self.round_repeated = false;
    }

    /// State for telemetry and for asserting on in tests.
    pub fn snapshot(&self) -> Value {
        json!({
            "fail_counts": self.fail_counts,
            "tool_fail_counts": self.tool_fail_counts,
            "tool_calls": self.tool_calls,
            "stale_rounds": self.stale_rounds,
            "halted_reason": self.halted_reason,
        })
    }

    /// Stop the turn after the current round.
    pub fn halt(&mut self, reason: &str) {
        self.halted_reason = reason.to_string();
    }

    /// Whether the turn was stopped outright.
    pub fn halted(&self) -> bool {
        !self.halted_reason.is_empty()
    }
}
