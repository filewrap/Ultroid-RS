//! Port of `plugins/schedulemsg.py` — schedule a message.
//!
//! `.schedule <text> <time>` or `.schedule <time>` as a reply: deliver
//! the text (or the replied message's text) after ``time`` — seconds as
//! a bare number, or `1h`/`30m`/`2d`-style durations. The time parsing
//! is real; the delayed send itself needs a scheduled-send op on the
//! `Responder` surface (see the `TODO(responder-op)` below).

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut d = def(
        "schedule",
        "schedule( (.*)|$)",
        "Schedule a message: `.schedule Hello 100` (100s) or `.schedule Hello 1h`.",
    );
    d.fullsudo = true;
    reg_add(registry, d, |ctx| Box::pin(schedule(ctx)));
}

/// Parse `90`, `30s`, `5m`, `2h`, `1d` (or `1h30m`) into seconds.
fn parse_delay(raw: &str) -> Option<u64> {
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

async fn schedule(ctx: &Ctx) -> Result<()> {
    let x = ctx.args.trim().to_string();
    let replied = ctx.reply_to_text.clone();
    let (text, delay) = match (x.is_empty(), replied) {
        (false, None) => {
            // `.schedule <text> <time>` — last token is the delay.
            let last = x.rsplit(' ').next().unwrap_or("").to_string();
            match parse_delay(&last) {
                Some(d) => (x.trim_end_matches(&last).trim().to_string(), d),
                None => {
                    ctx.reply(&ctx.get_str("schdl_2")).await?;
                    return Ok(());
                }
            }
        }
        (false, Some(rep)) => match parse_delay(&x) {
            Some(d) => (rep, d),
            None => {
                ctx.reply(&ctx.get_str("schdl_2")).await?;
                return Ok(());
            }
        },
        _ => {
            ctx.reply(&ctx.get_str("schdl_2")).await?;
            return Ok(());
        }
    };
    if text.is_empty() {
        ctx.reply(&ctx.get_str("schdl_2")).await?;
        return Ok(());
    }
    // TODO(responder-op): send `text` to this chat scheduled `delay`
    // seconds from now (Python: schedulemsg.py, `send_message(...,
    // schedule=timedelta(seconds=...))`).
    ctx.reply(&ctx.get_str("schdl_1")).await?;
    Err(Error::Other(format!(
        "scheduled send in {delay}s requires a binary Responder op (Python: schedulemsg.py:_)"
    )))
}
