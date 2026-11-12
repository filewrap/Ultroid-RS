//! UI config router — port of `plugins/_rebrand.py` (`.ui` command group).
//!
//! Owns three udB keys and nothing else:
//! * `UI_THEME` — `"dark"` / `"default"` (default `"default"`).
//! * `UI_CARDS` — rich cards on/off (default off = legacy plain text).
//! * `UI_BUTTONS` — `"classic"` / `"styled"` (default `"styled"`).
//!
//! The module-level getters (`ui_theme` / `ui_cards_enabled` /
//! `ui_buttons_style`) are the public API other plugins import — in Rust
//! they take `&Ctx` because there is no global `udB`. Values are stored as
//! strings in the database (`"true"`/`"false"` for cards), since the Rust
//! `Database` trait is string-typed.
//!
//! The Plugin Manifest Framework pieces (`@toggle` / `@action` /
//! `@manifest` / `@handle_manifest`) are callback-framework registrations,
//! not commands; they have no equivalent on the plugin `Registry` surface,
//! so the state flips they perform are covered by the `.ui` command itself
//! (theme cycling and button-style cycling are one-line `udB` writes).

use crate::util::{db_get, db_set, reg_add};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

pub const THEMES: [&str; 2] = ["default", "dark"];
pub const DEFAULT_THEME: &str = "default";
pub const BUTTON_STYLES: [&str; 2] = ["classic", "styled"];
pub const DEFAULT_BUTTON_STYLE: &str = "styled";
pub const DEFAULT_CARDS: bool = false;

/// Port of `ui_theme()` — the configured UI theme.
pub async fn ui_theme(ctx: &Ctx) -> String {
    match db_get(ctx, "UI_THEME").await.ok().flatten() {
        Some(v) if THEMES.contains(&v.as_str()) => v,
        _ => DEFAULT_THEME.to_string(),
    }
}

/// Port of `ui_cards_enabled()` — whether PIL/rich cards are enabled.
pub async fn ui_cards_enabled(ctx: &Ctx) -> bool {
    match db_get(ctx, "UI_CARDS").await.ok().flatten() {
        Some(v) => matches!(v.as_str(), "true" | "1" | "True"),
        None => DEFAULT_CARDS,
    }
}

/// Port of `ui_buttons_style()` — the configured inline-button style.
pub async fn ui_buttons_style(ctx: &Ctx) -> String {
    match db_get(ctx, "UI_BUTTONS").await.ok().flatten() {
        Some(v) if BUTTON_STYLES.contains(&v.as_str()) => v,
        _ => DEFAULT_BUTTON_STYLE.to_string(),
    }
}

async fn status_text(ctx: &Ctx) -> String {
    let cards = if ui_cards_enabled(ctx).await {
        "on"
    } else {
        "off"
    };
    format!(
        "**UI Configuration**\n\n• theme: `{}`\n• cards: `{cards}`\n• buttons: `{}`\n\n\
         `.ui theme <dark|default>` · `.ui cards <on|off>` · `.ui buttons <classic|styled>`",
        ui_theme(ctx).await,
        ui_buttons_style(ctx).await,
    )
}

/// `.ui [theme|cards|buttons <value>]` — port of `_rebrand.py::ui_router`.
async fn ui_router(ctx: &Ctx) -> Result<()> {
    // captures[1] = subcommand, captures[2] = value (pattern groups).
    let sub = ctx
        .captures
        .get(1)
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let value = ctx
        .captures
        .get(2)
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if sub.is_empty() {
        let text = status_text(ctx).await;
        return ctx.reply(&text).await.map(|_| ());
    }

    match sub.as_str() {
        "theme" => {
            if !THEMES.contains(&value.as_str()) {
                return ctx
                    .reply(&format!("Usage: `.ui theme <{}>`", THEMES.join("|")))
                    .await
                    .map(|_| ());
            }
            db_set(ctx, "UI_THEME", &value).await?;
            ctx.reply(&format!("UI theme set to **{value}**."))
                .await
                .map(|_| ())
        }
        "cards" => {
            if value != "on" && value != "off" {
                return ctx.reply("Usage: `.ui cards <on|off>`").await.map(|_| ());
            }
            let enabled = value == "on";
            db_set(ctx, "UI_CARDS", if enabled { "true" } else { "false" }).await?;
            ctx.reply(if enabled {
                "PIL cards enabled."
            } else {
                "PIL cards disabled — plain rich messages."
            })
            .await
            .map(|_| ())
        }
        "buttons" => {
            if !BUTTON_STYLES.contains(&value.as_str()) {
                return ctx
                    .reply(&format!(
                        "Usage: `.ui buttons <{}>`",
                        BUTTON_STYLES.join("|")
                    ))
                    .await
                    .map(|_| ());
            }
            db_set(ctx, "UI_BUTTONS", &value).await?;
            ctx.reply(&format!("Button style set to **{value}**."))
                .await
                .map(|_| ())
        }
        _ => {
            let text = status_text(ctx).await;
            ctx.reply(&text).await.map(|_| ())
        }
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "ui",
            pattern: r"ui(?:\s+(\S+)(?:\s+(\S+))?)?$",
            help: "Show/set the UI theme, cards, and button style.",
            owner_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(ui_router(ctx)),
    );
}
