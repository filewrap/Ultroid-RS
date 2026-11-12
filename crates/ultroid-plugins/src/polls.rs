//! Port of `plugins/polls.py` — create polls.
//!
//! `.poll <question> ; <option> ; <option> [| public|multiple|quiz[_N]]`
//! — anonymous by default; `public` shows voters, `multiple` allows
//! multiple choices, `quiz`/`quiz_N` marks the poll as a quiz with the
//! Nth option correct. All parsing and validation is real; the poll is
//! sent with the `Responder` `send_poll` op.
//!
//! Note: `send_poll` takes no public/anonymous flag, so a `public`
//! poll is sent the same way as an anonymous one; the `quiz_N`
//! correct answer *is* marked server-side (`correct` indices).

use crate::util::{def, reg_add};
use ultroid_core::{ChatType, ClientKind, Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "poll",
            "poll( (.*)|$)",
            "Create a poll: `.poll question ; option ; option | quiz_1`.",
        ),
        |ctx| Box::pin(poll(ctx)),
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollKind {
    Anonymous,
    Public,
    Multiple,
    Quiz,
}

async fn poll(ctx: &Ctx) -> Result<()> {
    if ctx.client_kind == ClientKind::User && ctx.chat_type == ChatType::Private {
        ctx.reply("`Use this in Group/Channel.`").await?;
        return Ok(());
    }
    let m = ctx.args.trim();
    if m.is_empty() {
        ctx.reply("`Give Proper Input...`").await?;
        return Ok(());
    }
    if !m.contains(';') {
        ctx.reply("`Unable to Determine Options.`.").await?;
        return Ok(());
    }
    // Split off the optional `| type` suffix first.
    let (main, ptype) = match m.split_once('|') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (m, None),
    };
    let mut parts = main.split(';');
    let question = parts.next().unwrap_or("").trim().to_string();
    let options: Vec<String> = parts.map(|p| p.trim().to_string()).collect();
    let mut kind = PollKind::Anonymous;
    let mut correct: Option<usize> = None;
    if let Some(pt) = ptype {
        let (name, num) = match pt.split_once('_') {
            Some((n, v)) => (n, v.parse::<usize>().ok()),
            None => (pt, None),
        };
        match name {
            "public" => kind = PollKind::Public,
            "multiple" => kind = PollKind::Multiple,
            "quiz" => {
                kind = PollKind::Quiz;
                correct = num.map(|n| n.saturating_sub(1));
            }
            _ => {
                ctx.reply("`Invalid Poll Type...`").await?;
                return Ok(());
            }
        }
    }
    if options.len() <= 1 {
        ctx.reply("`Options Should be More than 1..`").await?;
        return Ok(());
    }
    if let Some(c) = correct {
        if c >= options.len() {
            ctx.reply(&format!(
                "`Correct option must be between 1 and {} (got {}).`",
                options.len(),
                c + 1
            ))
            .await?;
            return Ok(());
        }
    }
    // `public` has no flag on `send_poll` (see the module docs), so it
    // is sent like an anonymous poll; the quiz correct answer is marked.
    ctx.responder
        .send_poll(
            ctx.chat_id,
            &question,
            options,
            kind == PollKind::Multiple,
            kind == PollKind::Quiz,
            correct.map(|c| vec![c]),
            None,
        )
        .await?;
    Ok(())
}
