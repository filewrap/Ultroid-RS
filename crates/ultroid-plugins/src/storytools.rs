//! Port of `plugins/storytools.py` — Telegram Stories.
//!
//! `.story [caption] [-p 6|12|24|48] [-priv everyone|contacts|close_friends|nobody] [-pin]`
//! (reply to, or send with, media), `.storydel <id> [id2 ...]`,
//! `.stealth` (hide recent story views; Premium-only).
//!
//! Argument parsing (shlex-style tokeniser, period/privacy validation)
//! is real and faithful. Posting / deleting a story and enabling stealth
//! mode are `stories.*` RPCs plus a media download — none on the
//! `Responder` surface yet, so each handler validates first and returns
//! an honest `TODO(responder-op)` error at the exact point the Telegram
//! op is needed.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub const PERIOD_CHOICES: [u32; 4] = [6, 12, 24, 48];
pub const PRIVACY_CHOICES: [&str; 4] = ["everyone", "contacts", "close_friends", "nobody"];

pub fn register(registry: &mut Registry) {
    let mut story = def(
        "story",
        "story( (.*)|$)",
        "Post a Story: `.story [caption] [-p 24] [-priv everyone] [-pin]` (reply to media).",
    );
    story.fullsudo = true;
    reg_add(registry, story, |ctx| Box::pin(send_story(ctx)));

    let mut del = def(
        "storydel",
        "storydel( (.*)|$)",
        "Delete your stories: `.storydel <id> [id2 ...]`.",
    );
    del.fullsudo = true;
    reg_add(registry, del, |ctx| Box::pin(delete_story(ctx)));

    let mut stealth = def(
        "stealth",
        "stealth$",
        "Activate Telegram stealth mode (hide recent story views). Premium-only.",
    );
    stealth.fullsudo = true;
    reg_add(registry, stealth, |ctx| Box::pin(stealth_mode(ctx)));
}

/// Parsed `.story` arguments: (caption, period hours, privacy, pinned).
pub struct StoryArgs {
    pub caption: Option<String>,
    pub period: u32,
    pub privacy: Option<String>,
    pub pinned: bool,
}

/// shlex-style tokeniser: splits on whitespace, honouring single/double quotes.
fn shlex_split(arg: &str) -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut in_token = false;
    for ch in arg.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(ch);
                in_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if quote.is_some() {
        return Err("Bad quotes: unterminated quote".to_string());
    }
    if in_token {
        out.push(cur);
    }
    Ok(out)
}

pub fn parse_story_args(arg: &str) -> std::result::Result<StoryArgs, String> {
    let tokens = shlex_split(arg)?;
    let mut period = 24u32;
    let mut privacy = None;
    let mut pinned = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "-p" && i + 1 < tokens.len() {
            let val: u32 = tokens[i + 1]
                .parse()
                .map_err(|_| format!("`-p` must be one of {:?}", PERIOD_CHOICES))?;
            if !PERIOD_CHOICES.contains(&val) {
                return Err(format!("`-p` must be one of {:?}", PERIOD_CHOICES));
            }
            period = val;
            i += 1;
        } else if tok == "-priv" && i + 1 < tokens.len() {
            let val = tokens[i + 1].to_lowercase();
            if !PRIVACY_CHOICES.contains(&val.as_str()) {
                return Err(format!("`-priv` must be one of {:?}", PRIVACY_CHOICES));
            }
            privacy = Some(val);
            i += 1;
        } else if tok == "-pin" {
            pinned = true;
        } else if tok.starts_with('-') && tok.len() > 1 {
            return Err(format!("Unknown flag: {tok}"));
        } else {
            positional.push(tok.clone());
        }
        i += 1;
    }
    Ok(StoryArgs {
        caption: if positional.is_empty() {
            None
        } else {
            Some(positional.join(" "))
        },
        period,
        privacy,
        pinned,
    })
}

async fn send_story(ctx: &Ctx) -> Result<()> {
    let args = match parse_story_args(ctx.args.trim()) {
        Ok(a) => a,
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
            return Ok(());
        }
    };
    if ctx.reply_to_id.is_none() && ctx.reply_to_text.is_none() {
        // No replied media and no attached media visible on `Ctx`.
        // (The Python version also accepts media attached to the command
        // message itself; neither is reachable here.)
        ctx.reply(&ctx.get_str("story_reply_media_required"))
            .await?;
        return Ok(());
    }
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): download the replied media and post it as a
    // story (stories.SendStoryRequest: caption, period, privacy, pinned)
    // (Python: storytools.py:_send_story, via `pyUltroid._core.stories.send_story`).
    let _ = args;
    Err(Error::Other(
        "send story requires a binary Responder op (Python: storytools.py:_send_story)".to_string(),
    ))
}

async fn delete_story(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    if raw.is_empty() {
        ctx.reply("`Give the story id(s) to delete.`").await?;
        return Ok(());
    }
    let mut ids: Vec<i64> = Vec::new();
    for tok in raw.split_whitespace() {
        match tok.parse::<i64>() {
            Ok(n) => ids.push(n),
            Err(_) => {
                ctx.reply(&format!("`Invalid story id: {tok}`")).await?;
                return Ok(());
            }
        }
    }
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): stories.DeleteStoriesRequest(ids)
    // (Python: storytools.py:_delete_story).
    let _ = ids;
    Err(Error::Other(
        "delete stories requires a binary Responder op (Python: storytools.py:_delete_story)"
            .to_string(),
    ))
}

async fn stealth_mode(ctx: &Ctx) -> Result<()> {
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): stories.ToggleStealthMode / send_stealth_mode
    // (Python: storytools.py:_stealth_mode, via
    // `pyUltroid._core.stories.send_stealth_mode`).
    Err(Error::Other(
        "enable stealth mode requires a binary Responder op (Python: storytools.py:_stealth_mode)"
            .to_string(),
    ))
}
