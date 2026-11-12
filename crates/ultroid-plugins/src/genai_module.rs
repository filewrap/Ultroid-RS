//! Port of `plugins/genai_module.py` — lifecycle adapter for the shared
//! GenAI agent runtime.
//!
//! The Python file has NO commands: it is a `BoundModule` whose handlers
//! fire on outgoing messages matching the `!?` trigger (`TRIGGER`) and
//! on `!clear` (`CLEAR`). This module ports it faithfully as helpers
//! plus a `register` that registers nothing — exactly like the Python
//! file, which registers no `ultroid_cmd` commands.
//!
//! The binary's outgoing-message path should call `handle_outgoing`
//! (trigger) and `handle_clear` (`!clear`); both delegate to the turn
//! runner in `crate::genai`, sharing its history key and runtime.

use crate::genai;
use ultroid_core::{Ctx, Registry, Result};

/// The `!?` trigger (Python `genai_module.py:TRIGGER`).
pub const TRIGGER_PATTERN: &str = r"(?s)^(?:(.+?)\s)?\!\?(?:\s-i)?$";

/// The `!clear` trigger (Python `genai_module.py:CLEAR`).
pub const CLEAR_PATTERN: &str = r"^!clear$";

/// Lifecycle-bound GenAI module (Python `GenAIModule` / `GeminiModule`).
pub struct GenAIModule;

impl GenAIModule {
    /// The same caller-scoped key as the decorator adapter.
    pub fn history_key(chat_id: i64, user_id: i64) -> String {
        genai::history_key(chat_id, user_id)
    }
}

/// Historical public class alias retained for import compatibility
/// (Python `GeminiModule = GenAIModule`).
pub type GeminiModule = GenAIModule;

/// Close-button callback data for the GenAI card (Python `CLOSE_DATA`).
pub const CLOSE_DATA: &[u8] = b"genai-close";

/// Handle one outgoing `!?` trigger message (Python
/// `GenAIModule.on_message_out`). Returns `Ok(true)` when the message
/// matched and was answered.
pub async fn handle_outgoing(ctx: &Ctx) -> Result<bool> {
    genai::check_trigger(ctx).await
}

/// Handle one outgoing `!clear` message (Python
/// `GenAIModule.on_cleared`). Returns `Ok(true)` when matched.
pub async fn handle_clear(ctx: &Ctx) -> Result<bool> {
    if ctx.text.trim() != "!clear" {
        return Ok(false);
    }
    genai::clear_history(ctx).await?;
    ctx.reply(&ctx.get_str("genai_cleared")).await?;
    Ok(true)
}

/// Registers nothing: this file has no commands (Python
/// `genai_module.py` defines no `ultroid_cmd` handlers — its handlers
/// are lifecycle-bound; see `handle_outgoing` / `handle_clear`).
pub fn register(_registry: &mut Registry) {}
