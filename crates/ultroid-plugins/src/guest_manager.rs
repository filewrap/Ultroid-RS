//! Guest-session management — port of `plugins/guest_manager.py`.
//!
//! DB key (raw udB key, same as Python `pyUltroid/dB/guest_db.py`):
//! `GUEST_SESSIONS` maps user id -> {scope, start, expiry} (epoch seconds;
//! `expiry` is null for indefinite sessions).
//!
//! `.guest add|remove|list` is fully ported over the DB. The reply path
//! needs the replied message's sender, which the `Ctx` surface does not
//! expose — that path is a `TODO(responder-op)` site. Duration formatting
//! mirrors Python `custom.fmt.fmtsec(..., part=1, human=True)`.

use crate::util::{def, reg_add};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use ultroid_core::db::{db_get_json, db_set_json};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "Guest session management.\n\n• `.guest add <user_id> [expiry_seconds]`\n• `.guest remove <user_id>`\n• `.guest list`\n";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "guest",
            pattern: r"guest (add|remove|list)(?:\s+(.*))?$",
            help: HELP,
            ..def("guest", r"guest (add|remove|list)(?:\s+(.*))?$", HELP)
        },
        |ctx| Box::pin(guest_manager(ctx)),
    );
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// One guest session (Python `GuestDB` record).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuestSession {
    pub scope: String,
    pub start: f64,
    pub expiry: Option<f64>,
}

/// All guest sessions (Python `GuestDB.get_guests()`).
pub async fn get_guests(ctx: &Ctx) -> Result<HashMap<String, GuestSession>> {
    Ok(db_get_json(&*ctx.db, "GUEST_SESSIONS")
        .await?
        .unwrap_or_default())
}

async fn set_guests(ctx: &Ctx, map: &HashMap<String, GuestSession>) -> Result<()> {
    db_set_json(&*ctx.db, "GUEST_SESSIONS", map).await
}

pub async fn add_guest(
    ctx: &Ctx,
    user_id: i64,
    expiry_seconds: Option<i64>,
    scope: &str,
) -> Result<()> {
    let mut map = get_guests(ctx).await?;
    let now = now_secs();
    map.insert(
        user_id.to_string(),
        GuestSession {
            scope: scope.to_string(),
            start: now,
            expiry: expiry_seconds.map(|s| now + s as f64),
        },
    );
    set_guests(ctx, &map).await
}

pub async fn remove_guest(ctx: &Ctx, user_id: i64) -> Result<bool> {
    let mut map = get_guests(ctx).await?;
    let removed = map.remove(&user_id.to_string()).is_some();
    if removed {
        set_guests(ctx, &map).await?;
    }
    Ok(removed)
}

/// Is this user currently a guest? Expired sessions are removed on read
/// (Python `GuestDB.is_guest`).
pub async fn is_guest(ctx: &Ctx, user_id: i64) -> Result<bool> {
    let mut map = get_guests(ctx).await?;
    let Some(sess) = map.get(&user_id.to_string()) else {
        return Ok(false);
    };
    if let Some(exp) = sess.expiry {
        if now_secs() > exp {
            map.remove(&user_id.to_string());
            set_guests(ctx, &map).await?;
            return Ok(false);
        }
    }
    Ok(true)
}

/// Human duration, mirroring Python `fmtsec(seconds, part=1, human=True)`:
/// the single largest unit, e.g. `2 days`, `3 hours`, `45 seconds`.
pub fn fmt_seconds_human(seconds: f64) -> String {
    let secs = seconds.max(0.0);
    let (value, unit) = if secs >= 86400.0 {
        (secs / 86400.0, "days")
    } else if secs >= 3600.0 {
        (secs / 3600.0, "hours")
    } else if secs >= 60.0 {
        (secs / 60.0, "minutes")
    } else {
        (secs, "seconds")
    };
    let v = value.round() as i64;
    format!("{v} {unit}")
}

async fn guest_manager(ctx: &Ctx) -> Result<()> {
    // Pattern groups: 1 = add|remove|list, 2 = rest.
    let action = ctx
        .captures
        .get(1)
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let args_str = ctx.captures.get(2).cloned().unwrap_or_default();
    let args: Vec<&str> = args_str.split_whitespace().collect();

    if action == "list" {
        let guests = get_guests(ctx).await?;
        if guests.is_empty() {
            ctx.reply("📝 **Active Guest Sessions**: None").await?;
            return Ok(());
        }
        let mut lines = vec!["<b>Active Guest Sessions</b>".to_string()];
        for (uid, details) in &guests {
            let expiry_str = match details.expiry {
                Some(exp) => {
                    let rem = exp - now_secs();
                    if rem > 0.0 {
                        format!("expires in {}", fmt_seconds_human(rem))
                    } else {
                        "expired".to_string()
                    }
                }
                None => "never expires".to_string(),
            };
            let elapsed = now_secs() - details.start;
            lines.push(format!(
                "<code>{uid}</code>: scope: {} | added: {} ago | status: {expiry_str}",
                details.scope,
                fmt_seconds_human(elapsed)
            ));
        }
        ctx.reply(&lines.join("\n")).await?;
        return Ok(());
    }

    // add / remove need a user id.
    let (user_id, expiry_seconds) = if ctx.reply_to_id.is_some() {
        // TODO(responder-op): get replied message -> sender id.
        // Python: plugins/guest_manager.py:guest_manager_cmd.
        return Err(todo_op(
            "get replied message sender",
            "plugins/guest_manager.py:guest_manager_cmd",
        ));
    } else {
        if args.is_empty() {
            ctx.reply("❌ **Error**: Please specify a User ID or reply to a user's message.")
                .await?;
            return Ok(());
        }
        let Ok(uid) = args[0].parse::<i64>() else {
            ctx.reply(&format!(
                "❌ **Error**: Invalid User ID `{}`. Must be an integer.",
                args[0]
            ))
            .await?;
            return Ok(());
        };
        let expiry = args.get(1).and_then(|a| a.parse::<i64>().ok());
        (uid, expiry)
    };

    if action == "add" {
        add_guest(ctx, user_id, expiry_seconds, "all").await?;
        let expiry_text = match expiry_seconds {
            Some(s) => format!("{s} seconds"),
            None => "indefinite".to_string(),
        };
        ctx.reply(&format!(
            "✅ **Guest Added**: User `{user_id}` is now registered as guest (duration: {expiry_text})."
        ))
        .await?;
    } else if action == "remove" {
        if remove_guest(ctx, user_id).await? {
            ctx.reply(&format!(
                "✅ **Guest Removed**: Revoked access for user `{user_id}`."
            ))
            .await?;
        } else {
            ctx.reply(&format!(
                "❌ **Guest Not Found**: User `{user_id}` has no active guest session."
            ))
            .await?;
        }
    }
    Ok(())
}
