//! Raw-update listener controls — port of `plugins/listeners.py`.
//!
//! `.listeners` lists registered raw-update listeners and persistently
//! enables/disables them (state in udB under `LISTENER_TOGGLES`).
//!
//! Genuinely unportable through the current plugin surface: the live
//! enumeration (`iter_dispatchers()` + `dispatcher.listener_states()`)
//! reads the Python dispatchers' in-memory listener tables, which have no
//! Rust counterpart in `ultroid-core` — there is no dispatcher object a
//! plugin can introspect. What IS ported, as real behaviour:
//!
//! * the `LISTENER_TOGGLES` persistence (`listener_enabled` /
//!   `set_listener_enabled` / `list_toggles`),
//! * the command's argument parsing, key matching (exact, then trailing
//!   qualname fragment) and replies,
//! * listing of the stored toggles when no live states are available.
//!
//! When the core gains a listener registry, `list_live_states` is the
//! single seam to fill in (marked below).

use crate::util::{def, reg_add};
use serde_json::Value;
use std::collections::HashMap;
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Registry, Result};

pub const HELP: &str = "\n\n- `{i}listeners`\n    List all registered raw-update listeners with their on/off state.\n\n- `{i}listeners disable <key>`\n    Persistently disable a listener (survives restarts).\n\n- `{i}listeners enable <key>`\n    Re-enable a previously disabled listener.\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "listeners",
            pattern: "listeners?( (.*)|$)",
            help: HELP,
            owner_only: true,
            ..def("listeners", "listeners?( (.*)|$)", HELP)
        },
        |ctx| Box::pin(listener_controls(ctx)),
    );
}

/// Replace `{}` placeholders in a localised string in order
/// (Python `str.format(*args)`).
pub fn fmt_args(template: &str, args: &[&str]) -> String {
    let mut out = template.to_string();
    for a in args {
        match out.find("{}") {
            Some(pos) => out.replace_range(pos..pos + 2, a),
            None => break,
        }
    }
    out
}

/// One listener's persisted toggle (Python `LISTENER_TOGGLES` udB key).
#[derive(Debug, Clone)]
pub struct ListenerState {
    pub key: String,
    pub enabled: bool,
}

/// The persisted toggle store (Python `dispatcher.set_listener_enabled`
/// writes through to the shared udB).
pub async fn list_toggles(ctx: &Ctx) -> Result<HashMap<String, bool>> {
    let raw: HashMap<String, Value> = db_get_json(&*ctx.db, "LISTENER_TOGGLES")
        .await?
        .unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|(k, v)| v.as_bool().map(|b| (k, b)))
        .collect())
}

pub async fn set_listener_enabled(ctx: &Ctx, key: &str, enabled: bool) -> Result<()> {
    let mut map = list_toggles(ctx).await?;
    map.insert(key.to_string(), enabled);
    db_set_json(&*ctx.db, "LISTENER_TOGGLES", &map).await
}

pub async fn listener_enabled(ctx: &Ctx, key: &str) -> Result<bool> {
    Ok(list_toggles(ctx).await?.get(key).copied().unwrap_or(true))
}

/// Live listener states (Python `_listener_states()`).
/// UNPORTABLE through the current surface: there is no dispatcher object
/// to introspect from a plugin. Returns the persisted toggles as the only
/// states known, so `.listeners` still reports real data instead of
/// inventing live states.
pub async fn list_live_states(ctx: &Ctx) -> Result<Vec<ListenerState>> {
    let toggles = list_toggles(ctx).await?;
    Ok(toggles
        .into_iter()
        .map(|(key, enabled)| ListenerState { key, enabled })
        .collect())
}

/// Match a target against known listener keys: exact first, then a
/// trailing qualname fragment (Python `listener_controls`).
pub fn match_key<'a>(states: &'a [ListenerState], target: &str) -> Option<&'a ListenerState> {
    if let Some(s) = states.iter().find(|s| s.key == target) {
        return Some(s);
    }
    states
        .iter()
        .find(|s| s.key.ends_with(&format!(".{target}")) || s.key.split('.').last() == Some(target))
}

async fn listener_controls(ctx: &Ctx) -> Result<()> {
    let args = ctx.args.trim().to_string();
    let states = list_live_states(ctx).await?;

    if args.is_empty() {
        if states.is_empty() {
            ctx.reply(&ctx.get_str("lstnr_6")).await?;
            return Ok(());
        }
        let mut lines = vec![fmt_args(
            &ctx.get_str("lstnr_1"),
            &[&states.len().to_string()],
        )];
        for state in &states {
            let mark = if state.enabled { "✅" } else { "❌" };
            lines.push(format!("{mark} `{}`", state.key));
        }
        ctx.reply(&lines.join("\n")).await?;
        return Ok(());
    }

    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    let action = parts[0].to_lowercase();
    if (action != "enable" && action != "disable") || parts.len() != 2 {
        ctx.reply(&ctx.get_str("lstnr_5")).await?;
        return Ok(());
    }
    let target = parts[1].trim();
    let Some(matched) = match_key(&states, target).map(|s| s.key.clone()) else {
        ctx.reply(&fmt_args(&ctx.get_str("lstnr_4"), &[target]))
            .await?;
        return Ok(());
    };
    let enabled = action == "enable";
    set_listener_enabled(ctx, &matched, enabled).await?;
    let key = if enabled { "lstnr_2" } else { "lstnr_3" };
    ctx.reply(&fmt_args(&ctx.get_str(key), &[&matched])).await?;
    Ok(())
}
