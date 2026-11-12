//! Per-chat agentic controls: tool use, tool rounds, temperature, model
//! — port of `pyUltroid/agent/_controls.py`.
//!
//! Stored as one database mapping so a chat's settings survive restarts,
//! with safe defaults that match the behaviour before this module
//! existed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ultroid_core::db::{Database, db_get_json, db_set_json};
use ultroid_core::error::{Error, Result};

pub const CHAT_SETTINGS_KEY: &str = "AGENT_CHAT_SETTINGS";

const CONTROL_FIELDS: [&str; 4] = ["tools", "rounds", "temp", "model"];

/// Effective controls for one chat.
///
/// Matches the pre-controls behaviour exactly: tools available to
/// `.agent`, the built-in round cap, the default sampling temperature, no
/// model pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatControls {
    pub tools: bool,
    /// `None` → the agent's own `MAX_TOOL_ROUNDS`.
    pub rounds: Option<u32>,
    pub temp: f64,
    /// `None` → the provider's configured default.
    pub model: Option<String>,
}

impl Default for ChatControls {
    fn default() -> Self {
        ChatControls {
            tools: true,
            rounds: None,
            temp: 0.7,
            model: None,
        }
    }
}

async fn all_settings(db: Option<&Arc<dyn Database>>) -> HashMap<String, Value> {
    let Some(db) = db else {
        return HashMap::new();
    };
    db_get_json(db.as_ref(), CHAT_SETTINGS_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn save_settings(db: Option<&Arc<dyn Database>>, settings: &HashMap<String, Value>) -> bool {
    let Some(db) = db else {
        return false;
    };
    db_set_json(db.as_ref(), CHAT_SETTINGS_KEY, settings)
        .await
        .is_ok()
}

/// Effective controls for *chat_id*, defaults merged in.
pub async fn get_controls(db: Option<&Arc<dyn Database>>, chat_id: &str) -> ChatControls {
    let mut controls = ChatControls::default();
    if let Some(stored) = all_settings(db).await.get(chat_id).and_then(|v| v.as_object()) {
        if let Some(v) = stored.get("tools").and_then(|v| v.as_bool()) {
            controls.tools = v;
        }
        if let Some(v) = stored.get("rounds").and_then(|v| v.as_u64()) {
            controls.rounds = Some(v as u32);
        }
        if let Some(v) = stored.get("temp").and_then(|v| v.as_f64()) {
            controls.temp = v;
        }
        if let Some(v) = stored.get("model").and_then(|v| v.as_str()) {
            controls.model = Some(v.to_string());
        }
    }
    controls
}

/// Set one control for *chat_id*. `value=None` clears it to the default.
pub async fn set_control(
    db: Option<&Arc<dyn Database>>,
    chat_id: &str,
    field: &str,
    value: Option<Value>,
) -> Result<bool> {
    if !CONTROL_FIELDS.contains(&field) {
        return Err(Error::Other(format!(
            "Unknown control {field:?}; expected one of: {}",
            CONTROL_FIELDS.join(", ")
        )));
    }
    let mut settings = all_settings(db).await;
    let mut chat: HashMap<String, Value> = settings
        .get(chat_id)
        .and_then(|v| serde_json::from_value::<HashMap<String, Value>>(v.clone()).ok())
        .unwrap_or_default();
    match value {
        None => {
            chat.remove(field);
        }
        Some(v) => {
            chat.insert(field.to_string(), v);
        }
    }
    if chat.is_empty() {
        settings.remove(chat_id);
    } else {
        settings.insert(
            chat_id.to_string(),
            serde_json::to_value(chat).unwrap_or(Value::Null),
        );
    }
    Ok(save_settings(db, &settings).await)
}

pub async fn reset_controls(db: Option<&Arc<dyn Database>>, chat_id: &str) -> Result<bool> {
    let mut settings = all_settings(db).await;
    if settings.remove(chat_id).is_some() {
        return Ok(save_settings(db, &settings).await);
    }
    Ok(true)
}

/// One-line human summary, for `.aiconf`.
pub async fn describe_controls(db: Option<&Arc<dyn Database>>, chat_id: &str) -> String {
    let controls = get_controls(db, chat_id).await;
    let rounds = match controls.rounds {
        Some(r) => r.to_string(),
        None => "default".to_string(),
    };
    let model = controls
        .model
        .clone()
        .unwrap_or_else(|| "default".to_string());
    format!(
        "tools={} · rounds={rounds} · temp={} · model={model}",
        if controls.tools { "on" } else { "off" },
        controls.temp
    )
}

/// Whether *user_id* may run tools in a DM session.
///
/// DM sessions are reachable by anyone who can message the bot, and the
/// tool set includes shell execution and file access, so tools are
/// restricted to the owner and sudos no matter what the chat controls
/// say. This is a hard boundary, not a default: the controls alone can
/// never grant it.
///
/// `privileged` is the owner + sudo id set, supplied by the caller: the
/// Python source reads it from `pyUltroid._misc.owner_and_sudos`, which
/// has no counterpart inside this crate.
pub async fn dm_tools_enabled(
    db: Option<&Arc<dyn Database>>,
    user_id: i64,
    controls: Option<ChatControls>,
    privileged: &HashSet<i64>,
) -> bool {
    let controls = match controls {
        Some(c) => c,
        None => get_controls(db, &format!("dm:{user_id}")).await,
    };
    if !controls.tools {
        return false;
    }
    privileged.contains(&user_id)
}
