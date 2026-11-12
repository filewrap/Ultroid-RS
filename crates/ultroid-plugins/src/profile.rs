//! Port of `plugins/profile.py` — profile editing and photo fetching.
//!
//! `.setname <first // last>`, `.setbio <bio>`, `.setpic` (reply to a
//! pic/video), `.delpfp [n|all]`, `.poto <user>/reply [-dl] [n|all]`.
//!
//! Argument parsing is real. Every one of these ends in an
//! `account.*` / `photos.*` RPC or a media download, none of which the
//! `Ctx`/`Responder` surface exposes — each handler validates its input
//! and then returns an honest `TODO(responder-op)` error at the exact
//! point the Telegram op is needed.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

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

fn responder_op(op: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: profile.py)"
    ))
}

async fn setbio(ctx: &Ctx) -> Result<()> {
    let bio = ctx.args.trim();
    let _id = ctx.reply("...").await?;
    // TODO(responder-op): account.UpdateProfileRequest(about=bio)
    // (Python: profile.py:_).
    Err(responder_op(&format!(
        "account.UpdateProfileRequest(about='{bio}')"
    )))
}

async fn setname(ctx: &Ctx) -> Result<()> {
    let names = ctx.args.trim();
    let _id = ctx.reply("...").await?;
    let (first, last) = match names.split_once("//") {
        Some((f, l)) => (f.trim(), l.trim()),
        None => (names, ""),
    };
    // TODO(responder-op): account.UpdateProfileRequest(first_name, last_name)
    // (Python: profile.py:_).
    Err(responder_op(&format!(
        "account.UpdateProfileRequest(first_name='{first}', last_name='{last}')"
    )))
}

async fn setpic(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a Media..`").await?;
        return Ok(());
    }
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): download the replied media, upload it, then
    // photos.UploadProfilePhotoRequest(file/video) (Python: profile.py:_).
    Err(responder_op(
        "download replied media + photos.UploadProfilePhotoRequest",
    ))
}

async fn delpfp(ctx: &Ctx) -> Result<()> {
    let _id = ctx.reply("`...`").await?;
    let arg = ctx.args.trim();
    let limit: Option<i64> = match arg {
        "all" => Some(0),
        "" => Some(1),
        _ => arg.parse::<i64>().ok().or(Some(1)),
    };
    let _ = limit;
    // TODO(responder-op): get_profile_photos("me", limit) then
    // photos.DeletePhotosRequest (Python: profile.py:remove_profilepic).
    Err(responder_op(
        "get_profile_photos + photos.DeletePhotosRequest",
    ))
}

async fn poto(ctx: &Ctx) -> Result<()> {
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    let arg = ctx.args.trim();
    let just_dl = arg == "-dl" || arg == "--dl";
    let _ = just_dl;
    // Resolving a username / a replied sender's id and downloading their
    // profile photo(s) are both client ops.
    // TODO(responder-op): resolve the target user and download their
    // profile photo(s) (Python: profile.py:gpoto,
    // `download_profile_photo` / `iter_profile_photos`).
    Err(responder_op(
        "resolve user + download_profile_photo / iter_profile_photos",
    ))
}
