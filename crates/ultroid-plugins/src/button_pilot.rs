//! Button pilot — port of `plugins/button_pilot.py` (`.buttonpilot`).
//!
//! A demo command that sends an inline keyboard with colorized buttons
//! (primary/danger/success) through the assistant bot. Sending a message
//! with an inline keyboard through the assistant needs an assistant-send
//! op that does not exist on `Responder`, so the command is an honest
//! `TODO(responder-op)` error.
//!
//! The `pilot_<color>` callback is an assistant-bot callback handler, not
//! a command; there is no callback registration surface in the plugin
//! crate, so it is not portable in this wave. The mapping it implements
//! is preserved here for the follow-up pass:
//! `blue` -> "BLUE (style: primary, icon: 1) 🔵",
//! `red` -> "RED (style: danger, icon: 2) 🔴",
//! `green` -> "GREEN (style: success, icon: 3) 🟢".

use crate::util::reg_add;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

/// The color answer the `pilot_<color>` callback would edit into the
/// message (Python: `button_pilot.py::pilot_button_callback`).
pub fn pilot_answer(color: &str) -> String {
    let selected = match color {
        "blue" => "BLUE (style: primary, icon: 1) 🔵".to_string(),
        "red" => "RED (style: danger, icon: 2) 🔴".to_string(),
        "green" => "GREEN (style: success, icon: 3) 🟢".to_string(),
        other => other.to_uppercase(),
    };
    format!("🎉 You successfully clicked the **{selected}** button!")
}

/// `.buttonpilot` — port of `button_pilot.py::button_pilot_cmd`.
async fn button_pilot(ctx: &Ctx) -> Result<()> {
    tracing::info!("buttonpilot command triggered.");
    // TODO(responder-op): assistant-bot send_message with the colorized
    // inline keyboard (Blue/primary, Red/danger, Green/success)
    // (Python: button_pilot.py:button_pilot_cmd -> asst.send_message).
    let _ = ctx;
    Err(Error::Other(
        "assistant send with inline keyboard (button pilot) requires a \
         binary Responder op (Python: button_pilot.py:button_pilot_cmd)"
            .to_string(),
    ))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "buttonpilot",
            pattern: "buttonpilot$",
            help: "Send an inline keyboard with custom color buttons.",
            allow_guest: true,
            ..Default::default()
        },
        |ctx| Box::pin(button_pilot(ctx)),
    );
}
