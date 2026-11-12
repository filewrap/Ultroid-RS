//! Interactive tools: the agent can ask, confirm and collect — port of
//! `pyUltroid/agent/_interactive.py`.
//!
//! These are the Hermes `clarify` idea in SarahUB terms. The loop does
//! not exit for a question — asking is an ordinary tool call whose
//! handler blocks on a future until the user taps a styled button or
//! types a reply, and the answer arrives as a normal tool result. That
//! is what makes "ask my name" work: the model calls `ask_user`, the
//! turn continues, and the name is in context by the time it produces
//! its final answer.
//!
//! Two rules keep this from becoming a foot-gun:
//!
//! * A question that is never answered resolves as `timed_out` with a
//!   reason, never an exception. A silent user is a normal outcome the
//!   model can reason about, not a crash.
//! * Every ask needs `agent.ask` cap, so a privileged grant is what buys
//!   the ability to interrupt a human.

use std::sync::Arc;

use serde_json::{Value, json};
use ultroid_core::error::Result;

use crate::bridge::{AskBridge, AskOutcome};

/// A keyboard wider than this stops being usable on a phone.
pub const MAX_CHOICES: usize = 12;
/// One turn may not become an interrogation.
pub const MAX_QUESTIONS: usize = 5;

const TTL_MIN: f64 = 5.0;
const TTL_MAX: f64 = 900.0;
const TTL_DEFAULT: f64 = 120.0;

/// Coerce an LLM-supplied TTL into a sane range; junk becomes the default.
pub fn clamp_ttl(value: f64) -> f64 {
    clamp_ttl_default(value, TTL_DEFAULT)
}

pub fn clamp_ttl_default(value: f64, default: f64) -> f64 {
    if value.is_nan() || value.is_infinite() {
        return default;
    }
    value.clamp(TTL_MIN, TTL_MAX)
}

/// The three interactive tool schemas, mirroring `INTERACTIVE_SCHEMAS`.
/// Built at runtime (schema values interpolate the constants above).
pub fn interactive_schemas() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": concat!(
                    "Ask the user a question and wait for their answer. Use this ",
                    "when you genuinely cannot continue without something only ",
                    "they know - their name, a preference, a missing path, which ",
                    "of two options they meant. This blocks the turn until they ",
                    "answer or the TTL expires, so ask only what you need. If you ",
                    "supply choices, they get a tap-to-answer keyboard; otherwise ",
                    "they can just type. On expiry you get timed_out=true and must ",
                    "decide how to proceed - do not retry the same question."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question, phrased for a human.",
                        },
                        "choices": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": format!(
                                "Optional tap-to-answer options, at most {MAX_CHOICES}. Omit to let them type freely."
                            ),
                        },
                        "note": {
                            "type": "string",
                            "description": "Optional extra context under the question.",
                        },
                        "ttl": {
                            "type": "number",
                            "description": format!(
                                "Seconds to wait, clamped to [{}, {}]. Default {}.",
                                TTL_MIN as i64, TTL_MAX as i64, TTL_DEFAULT as i64
                            ),
                        },
                    },
                    "required": ["question"],
                },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "confirm_action",
                "description": concat!(
                    "Ask the user to confirm something you are about to do that ",
                    "cannot be undone - deleting, force-updating, sending on their ",
                    "behalf. Returns their yes/no. Use it before a destructive ",
                    "shell command or a bulk send, not for routine work."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "what": {
                            "type": "string",
                            "description": "What you are about to do, concretely.",
                        },
                        "yes_label": {"type": "string", "description": "Confirm button text."},
                        "no_label": {"type": "string", "description": "Cancel button text."},
                        "ttl": {"type": "number", "description": "Seconds to wait."},
                    },
                    "required": ["what"],
                },
            },
        }),
        json!({
            "type": "function",
            "function": {
                "name": "collect_fields",
                "description": concat!(
                    "Collect several answers in one go - a short form. Each field ",
                    "is asked in turn with its own keyboard. Use it instead of ",
                    "repeating ask_user when you need more than one thing, so the ",
                    "user is interrupted once rather than five times."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "fields": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": {"type": "string", "description": "Field label."},
                                    "question": {"type": "string"},
                                    "choices": {
                                        "type": "array",
                                        "items": {"type": "string"},
                                    },
                                },
                                "required": ["name", "question"],
                            },
                            "description": format!(
                                "Between 1 and {MAX_QUESTIONS} fields, asked in order."
                            ),
                        },
                        "ttl": {"type": "number", "description": "Seconds per field."},
                    },
                    "required": ["fields"],
                },
            },
        }),
    ]
}

/// One cleaned form field for [`InteractiveTools::collect_fields`].
#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub name: String,
    pub question: String,
    pub choices: Vec<String>,
}

/// The three interactive tools, over one shared [`AskBridge`].
///
/// The bridge is injected rather than created, so a chat that already
/// has one keeps a single TTL story and the pending-question map stays
/// inspectable from one place.
pub struct InteractiveTools {
    pub bridge: Arc<AskBridge>,
    pub chat_id: i64,
}

impl InteractiveTools {
    pub fn new(bridge: Option<Arc<AskBridge>>, chat_id: i64) -> Self {
        InteractiveTools {
            bridge: bridge.unwrap_or_else(|| Arc::new(AskBridge::new())),
            chat_id,
        }
    }

    /// Point the tools at another chat (the runtime is per-request).
    pub fn bind(&mut self, chat_id: i64) {
        self.chat_id = chat_id;
    }

    // ── ask_user ──

    /// Put one question to the user and return their answer.
    ///
    /// A model-supplied `ttl` overrides the bridge default, but only
    /// after clamping. Supplying nothing leaves the bridge's own TTL in
    /// charge, so a bridge configured short (tests, a chat that should
    /// not nag) is not silently overridden by a hardcoded default.
    pub async fn ask_user(
        &self,
        question: &str,
        choices: Option<Vec<String>>,
        note: &str,
        ttl: Option<f64>,
    ) -> Result<Value> {
        let options = clean_choices(choices);
        let outcome = self
            .bridge
            .ask(
                self.chat_id,
                question,
                options.clone(),
                note,
                ttl.map(|t| std::time::Duration::from_secs_f64(clamp_ttl(t))),
                None,
                None,
                2,
                true,
            )
            .await?;
        let mut payload = outcome.as_dict();
        if !options.is_empty() {
            payload["choices_offered"] = json!(options);
        }
        Ok(payload)
    }

    // ── confirm_action ──

    /// Ask for a yes/no before something irreversible.
    pub async fn confirm_action(
        &self,
        what: &str,
        yes_label: &str,
        no_label: &str,
        ttl: Option<f64>,
    ) -> Result<Value> {
        let yes = if yes_label.is_empty() {
            "Yes"
        } else {
            yes_label
        };
        let no = if no_label.is_empty() { "No" } else { no_label };
        let outcome = self
            .bridge
            .ask(
                self.chat_id,
                &format!("{what}\n\nProceed?"),
                vec![yes.to_string(), no.to_string()],
                "",
                ttl.map(|t| std::time::Duration::from_secs_f64(clamp_ttl(t))),
                Some("primary"),
                None,
                2,
                true,
            )
            .await?;
        let mut payload = outcome.as_dict();
        let confirmed = if outcome.answered() {
            outcome.value.as_deref() == Some(yes)
        } else {
            false
        };
        payload["confirmed"] = Value::Bool(confirmed);
        Ok(payload)
    }

    // ── collect_fields ──

    /// Ask a short form of questions, one keyboard each.
    pub async fn collect_fields(
        &self,
        fields: Option<Vec<Value>>,
        ttl: Option<f64>,
    ) -> Result<Value> {
        let specs = clean_fields(fields);
        if specs.is_empty() {
            return Ok(json!({"error": "collect_fields needs at least one field"}));
        }

        let mut answers = serde_json::Map::new();
        let mut skipped: Vec<String> = Vec::new();
        let per_field = ttl.map(|t| std::time::Duration::from_secs_f64(clamp_ttl(t)));

        for spec in &specs {
            let outcome = self
                .bridge
                .ask(
                    self.chat_id,
                    &spec.question,
                    spec.choices.clone(),
                    "",
                    per_field,
                    None,
                    None,
                    2,
                    true,
                )
                .await?;
            if outcome.answered() {
                answers.insert(
                    spec.name.clone(),
                    outcome.value.map(Value::String).unwrap_or(Value::Null),
                );
            } else {
                // One unanswered field ends the form: continuing would nag.
                skipped.push(spec.name.clone());
                answers.insert(spec.name.clone(), Value::Null);
                break;
            }
        }

        Ok(json!({
            "answers": answers,
            "skipped": skipped,
            "complete": skipped.is_empty(),
            "timed_out": !skipped.is_empty(),
        }))
    }

    // ── diagnostics ──

    /// Outstanding questions. A non-zero value after a turn is a bug.
    pub async fn pending(&self) -> usize {
        self.bridge.pending().await
    }
}

/// Normalise and bound a model-supplied choice list.
pub fn clean_choices(choices: Option<Vec<String>>) -> Vec<String> {
    let Some(choices) = choices else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<String> = Vec::new();
    for choice in choices {
        let cleaned = choice.trim().to_string();
        if cleaned.is_empty() {
            continue;
        }
        // Duplicates would produce two identical buttons resolving one future.
        if seen.insert(cleaned.to_lowercase()) {
            unique.push(cleaned);
        }
    }
    unique.truncate(MAX_CHOICES);
    unique
}

/// Normalise and bound a model-supplied form.
pub fn clean_fields(fields: Option<Vec<Value>>) -> Vec<FieldSpec> {
    let Some(fields) = fields else {
        return Vec::new();
    };
    let mut specs: Vec<FieldSpec> = Vec::new();
    for entry in fields.iter().take(MAX_QUESTIONS) {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let question = obj
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() || question.is_empty() {
            continue;
        }
        let choices = obj.get("choices").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<String>>()
        });
        specs.push(FieldSpec {
            name,
            question,
            choices: clean_choices(choices),
        });
    }
    specs
}

/// Re-export for callers that mirror the Python `AskOutcome` import.
pub type Outcome = AskOutcome;
