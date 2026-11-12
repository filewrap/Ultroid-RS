//! Port of `plugins/fakeaction.py` — fake chat actions.
//!
//! `.ftyping [time]`, `.faudio`, `.fcontact`, `.fdocument`, `.fgame`,
//! `.flocation`, `.fsticker`, `.fphoto`, `.fround`, `.fvideo` — show a
//! fake typing / recording / uploading action in the chat for ``time``
//! (seconds, or `1h`/`30m`/`2d`-style durations; default 60s).
//!
//! The duration parsing and the status notice are real. Showing the
//! action itself needs a client-level "send chat action" call that the
//! `Ctx`/`Responder` surface does not expose yet (see the
//! `TODO(responder-op)` below).

use crate::util::{def, reg_add};
use std::time::Duration;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "fakeaction",
            "f(typing|audio|contact|document|game|location|sticker|photo|round|video)( (.*)|$)",
            "Show a fake chat action (typing, recording, uploading...) for a given time.",
        ),
        |ctx| Box::pin(fakeaction(ctx)),
    );
}

/// Parse a duration like `90`, `30s`, `5m`, `2h`, `1d` (or `1h30m`) into seconds.
fn parse_duration(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(n) = raw.parse::<u64>() {
        return Some(n);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: u64 = num.parse().ok()?;
            let mult = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return None,
            };
            total = total.checked_add(n.checked_mul(mult)?)?;
            num.clear();
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(total)
}

async fn fakeaction(ctx: &Ctx) -> Result<()> {
    // captures[1] = action word, captures[3] = optional duration text.
    let mut act = ctx.captures.get(1).cloned().unwrap_or_default();
    let raw_t = ctx.captures.get(3).cloned().unwrap_or_default();
    if matches!(act.as_str(), "audio" | "round" | "video") {
        act = format!("record-{act}");
    }
    let t = parse_duration(raw_t.trim()).unwrap_or(60);
    let notice = ctx.get_str("fka_1").replacen("{}", &t.to_string(), 1);
    ctx.reply(&notice).await?;
    // TODO(responder-op): client.send chat action `act` in this chat for `t`
    // seconds, then stop it (Python: fakeaction.py, `e.client.action(...)`).
    let _ = Duration::from_secs(t);
    Err(Error::Other(format!(
        "send chat action '{act}' requires a binary Responder op (Python: fakeaction.py:_)"
    )))
}
