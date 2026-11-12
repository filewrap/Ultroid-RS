//! Port of `plugins/profile.py` — profile editing and photo fetching.
//!
//! `.setname <first // last>`, `.setbio <bio>`, `.setpic` (reply to a
//! pic/video), `.delpfp [n|all]`, `.poto <user>/reply [-dl] [n|all]`.
//!
//! Argument parsing is real, and the handlers are wired to the
//! `Responder` ops (`update_profile`, `download_media` +
//! `upload_profile_photo`, `delete_profile_photos`,
//! `resolve_user` + `download_profile_photos`).

use crate::util::{def, reg_add};
use std::path::PathBuf;
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    let mut bio = def(
        "setbio",
        "setbio( (.*)|$)",
        "Change your profile bio: `.setbio <bio>`.",
    );
    bio.fullsudo = true;
    reg_add(registry, bio, |ctx| Box::pin(setbio(ctx)));

    let mut name = def(
        "setname",
        "setname ?((.|//)*)",
        "Change your profile name: `.setname <first // last>`.",
    );
    name.fullsudo = true;
    reg_add(registry, name, |ctx| Box::pin(setname(ctx)));

    let mut pic = def(
        "setpic",
        "setpic$",
        "Change your profile pic: `.setpic` as a reply to a photo/video.",
    );
    pic.fullsudo = true;
    reg_add(registry, pic, |ctx| Box::pin(setpic(ctx)));

    let mut del = def(
        "delpfp",
        "delpfp( (.*)|$)",
        "Delete profile pic(s): `.delpfp [n|all]`.",
    );
    del.fullsudo = true;
    reg_add(registry, del, |ctx| Box::pin(delpfp(ctx)));

    reg_add(
        registry,
        def(
            "poto",
            "poto( (.*)|$)",
            "Upload a user's profile photo(s): `.poto <user> [n|all|-dl]`.",
        ),
        |ctx| Box::pin(poto(ctx)),
    );
}

async fn setbio(ctx: &Ctx) -> Result<()> {
    let bio = ctx.args.trim();
    let id = ctx.reply("...").await?;
    // account.UpdateProfileRequest(about=bio) (Python: profile.py:_).
    ctx.responder.update_profile(None, None, Some(bio)).await?;
    ctx.edit(id, "`Bio updated.`").await
}

async fn setname(ctx: &Ctx) -> Result<()> {
    let names = ctx.args.trim();
    let id = ctx.reply("...").await?;
    let (first, last) = match names.split_once("//") {
        Some((f, l)) => (f.trim(), l.trim()),
        None => (names, ""),
    };
    // account.UpdateProfileRequest(first_name, last_name)
    // (Python: profile.py:_).
    ctx.responder
        .update_profile(Some(first), Some(last), None)
        .await?;
    ctx.edit(id, "`Name updated.`").await
}

async fn setpic(ctx: &Ctx) -> Result<()> {
    let reply_id = match ctx.reply_to_id {
        Some(id) => id,
        None => {
            ctx.reply("`Reply to a Media..`").await?;
            return Ok(());
        }
    };
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    // Download the replied media, then upload it as the profile photo
    // (Python: profile.py:_).
    let dest = std::env::temp_dir().join(format!("ultroid-setpic-{}", uuid::Uuid::new_v4()));
    let path = ctx
        .responder
        .download_media(ctx.chat_id, reply_id as i32, &dest)
        .await?;
    let res = ctx.responder.upload_profile_photo(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    res?;
    ctx.edit(id, "`Profile photo updated.`").await
}

async fn delpfp(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`...`").await?;
    let arg = ctx.args.trim();
    // Python: "all" -> 0 (delete all), a number -> that many,
    // anything else (incl. empty) -> 1. The Responder op treats
    // limit <= 0 as "all", matching Telethon's limit=0.
    let limit: i32 = match arg {
        "all" => 0,
        "" => 1,
        _ => arg.parse::<i32>().ok().unwrap_or(1),
    };
    let deleted = ctx.responder.delete_profile_photos(limit).await?;
    ctx.edit(
        id,
        &format!("`Successfully deleted {deleted} profile picture(s).`"),
    )
    .await
}

/// Parsed `.poto` arguments: (user token, limit, just download).
/// Limit: `None` = the single most recent photo, `Some(0)` = all,
/// `Some(n)` = the `n` most recent.
fn parse_poto_args(arg: &str) -> (Option<String>, Option<i32>, bool) {
    let mut user = None;
    let mut limit = None;
    let mut just_dl = false;
    for tok in arg.split_whitespace() {
        match tok {
            "-dl" | "--dl" => just_dl = true,
            "all" => limit = Some(0),
            _ => {
                if user.is_none() {
                    user = Some(tok.to_string());
                } else if let Ok(n) = tok.parse::<i32>() {
                    limit = Some(n);
                }
            }
        }
    }
    (user, limit, just_dl)
}

async fn poto(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim();
    let (user_tok, limit, just_dl) = parse_poto_args(arg);

    // Resolve the target user (Python: profile.py:gpoto).
    let dialog_id: i64 = if ctx.reply_to_id.is_some() {
        match ctx.reply_to_sender_id {
            Some(sender) => sender,
            None => {
                // Fall back to the replied message's own sender record.
                let reply_id = ctx.reply_to_id.unwrap();
                match ctx
                    .responder
                    .message_summary(ctx.chat_id, reply_id as i32)
                    .await?
                {
                    Some(summary) => match summary.sender_id {
                        Some(sender) => sender,
                        None => {
                            ctx.edit(id, "`Couldn't tell who sent the replied message.`")
                                .await?;
                            return Ok(());
                        }
                    },
                    None => {
                        ctx.edit(id, "`The replied message was deleted.`").await?;
                        return Ok(());
                    }
                }
            }
        }
    } else if let Some(tok) = user_tok {
        ctx.responder.resolve_user(&tok).await?.id
    } else {
        // No reply and no user: the current chat (a DM peer, as in Python).
        ctx.chat_id
    };

    let dir: PathBuf = std::env::temp_dir().join(format!("ultroid-poto-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await?;
    // The Responder op clamps the limit to 1..=100, so "all" (0) is
    // passed as 100 (the most the server returns in one call).
    let fetch_limit = match limit {
        None => 1,
        Some(0) => 100,
        Some(n) => n,
    };
    let photos = ctx
        .responder
        .download_profile_photos(dialog_id, &dir, fetch_limit)
        .await?;
    if photos.is_empty() {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        ctx.edit(id, "`Pfp Not Found...`").await?;
        return Ok(());
    }
    if just_dl {
        let list = photos
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        ctx.edit(id, &format!("Downloaded pfp to [ `{list}` ]."))
            .await?;
        return Ok(());
    }
    ctx.delete(id).await?;
    for file in &photos {
        ctx.send_file(file, "").await?;
        let _ = tokio::fs::remove_file(file).await;
    }
    let _ = tokio::fs::remove_dir_all(&dir).await;
    Ok(())
}
