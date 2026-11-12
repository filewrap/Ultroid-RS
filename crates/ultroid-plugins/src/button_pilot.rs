//! Button pilot — port of `plugins/button_pilot.py` (`.buttonpilot`).
//!
//! A demo command that sends an inline keyboard with colorized buttons
//! (primary/danger/success) through the assistant bot, wired via the
//! assistant responder's `send_markup_to` (the callback payloads match
//! Python's `pilot_<color>` data exactly; the callback handler itself
//! lives on the assistant crate).
//!
//! The `pilot_<color>` callback is an assistant-bot callback handler, not
//! a command; there is no callback registration surface in the plugin
//! crate, so it is not portable in this wave. The mapping it implements
//! is preserved here for the follow-up pass:
//! `blue` -> "BLUE (style: primary, icon: 1) 🔵",
//! `red` -> "RED (style: danger, icon: 2) 🔴",
//! `green` -> "GREEN (style: success, icon: 3) 🟢".

use crate::util::reg_add;
use ultroid_core::tg::{ButtonStyle, InlineButton};
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
    let Some(asst) = &ctx.asst else {
        return Err(Error::Other(
            "sending the button pilot keyboard requires the assistant bot to be connected".into(),
        ));
    };
    let rows = vec![
        vec![
            InlineButton::data("Blue Button", b"pilot_blue".to_vec()).styled(ButtonStyle::Primary),
        ],
        vec![InlineButton::data("Red Button", b"pilot_red".to_vec()).styled(ButtonStyle::Danger)],
        vec![
            InlineButton::data("Green Button", b"pilot_green".to_vec())
                .styled(ButtonStyle::Success),
        ],
    ];
    asst.send_markup_to(ctx.chat_id, "🎨 **Select a custom color button:**", rows)
        .await?;
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
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
