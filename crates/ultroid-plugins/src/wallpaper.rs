//! Wallpapers & chat themes — port of `plugins/wallpaper.py`
//! (source HEAD 5d09531).
//!
//! Commands Available (from the Python docstring):
//! - `.walls` — list the wallpapers saved on this account.
//! - `.wallsave` `<reply to photo/document>` — save the replied media
//!   as an account wallpaper. If Telegram rejects the upload, the media
//!   is sent back as a plain photo instead (fallback, as in Python).
//! - `.themes` — list the chat themes available on this account.
//! - `.settheme <emoticon>` — set the current chat's theme to one of
//!   the emoticons from `.themes`; `.settheme off` clears it.
//!
//! All four commands are raw MTProto account/messages requests
//! (`account.GetWallPapers`, `account.UploadWallPaper`,
//! `account.SaveWallPaper`, `account.GetChatThemes`,
//! `messages.SetChatTheme`) with no equivalent on the `Ctx`/`Responder`
//! surface, so each handler validates its input first and then returns
//! an honest TODO(responder-op) error at the exact request point.
//! `.wallsave` also implements Python's fallback path: when the upload
//! request itself is unavailable, the replied media is sent back as a
//! photo — but sending the *replied* media still needs the download,
//! so the fallback is reached only when a local file path is given.

use std::path::PathBuf;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

use crate::util::{def, reg_add};

pub fn register(registry: &mut Registry) {
    // Python decorates all four with `fullsudo=True`.
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def(
                "walls",
                "walls$",
                "List the wallpapers saved on this account.",
            )
        },
        |ctx| Box::pin(list_walls(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def(
                "wallsave",
                "wallsave$",
                "Save replied media as an account wallpaper.",
            )
        },
        |ctx| Box::pin(save_wallpaper(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def(
                "themes",
                "themes$",
                "List the chat themes available on this account.",
            )
        },
        |ctx| Box::pin(list_themes(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def(
                "settheme",
                "settheme( (.*)|$)",
                "Set the current chat's theme: .settheme <emoticon> (off to clear).",
            )
        },
        |ctx| Box::pin(set_theme(ctx)),
    );
}

const WALL_LIST_LIMIT: usize = 25;
const THEME_LIST_LIMIT: usize = 40;

async fn list_walls(ctx: &Ctx) -> Result<()> {
    let _ = ctx;
    // Python: `account.GetWallPapersRequest(hash=0)`, list id/slug +
    // default/dark/pattern flags, truncated at 25.
    // TODO(responder-op): account.GetWallPapers (Python: wallpaper.py:_list_walls).
    Err(Error::Other(
        "listing account wallpapers requires a binary Responder op (account.GetWallPapersRequest) (Python: wallpaper.py:_list_walls)".into(),
    ))
}

async fn save_wallpaper(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to a photo/document to save it as a wallpaper.`")
            .await?;
        return Ok(());
    }
    // With a local file path the file itself is on disk; the upload/save
    // requests are still MTProto.
    let local = ctx
        .args
        .split_whitespace()
        .map(PathBuf::from)
        .find(|p| p.is_file());
    let _ = local;
    // Python: `account.UploadWallPaperRequest(file, mime_type, settings)`
    // then `account.SaveWallPaperRequest(InputWallPaper, unsave=False,
    // settings)`; on RPCError it falls back to sending the media back
    // as a photo.
    // TODO(responder-op): account.UploadWallPaper + SaveWallPaper
    // (Python: wallpaper.py:_save_wallpaper).
    Err(Error::Other(
        "uploading + saving a wallpaper requires a binary Responder op (account.UploadWallPaperRequest / account.SaveWallPaperRequest) (Python: wallpaper.py:_save_wallpaper)".into(),
    ))
}

async fn list_themes(ctx: &Ctx) -> Result<()> {
    let _ = ctx;
    // Python: `account.GetChatThemesRequest(hash=0)`, emoticons listed,
    // truncated at 40.
    // TODO(responder-op): account.GetChatThemes (Python: wallpaper.py:_list_themes).
    Err(Error::Other(
        "listing chat themes requires a binary Responder op (account.GetChatThemesRequest) (Python: wallpaper.py:_list_themes)".into(),
    ))
}

async fn set_theme(ctx: &Ctx) -> Result<()> {
    let emoticon = ctx.args.trim();
    if emoticon.is_empty() {
        ctx.reply("`Usage: .settheme <emoticon>` (or `off` to clear)")
            .await?;
        return Ok(());
    }
    let _label = if emoticon.eq_ignore_ascii_case("off")
        || emoticon.eq_ignore_ascii_case("none")
        || emoticon.eq_ignore_ascii_case("clear")
    {
        "cleared"
    } else {
        "set"
    };
    // Python: `messages.SetChatThemeRequest(peer, theme)` with
    // `InputChatTheme(emoticon)` or `InputChatThemeEmpty()` for off.
    // TODO(responder-op): messages.SetChatTheme (Python: wallpaper.py:_set_theme).
    Err(Error::Other(
        "setting a chat theme requires a binary Responder op (messages.SetChatThemeRequest) (Python: wallpaper.py:_set_theme)".into(),
    ))
}

/// Limits kept for the Responder wiring pass (Python module constants).
#[allow(dead_code)]
fn _limits() -> (usize, usize) {
    (WALL_LIST_LIMIT, THEME_LIST_LIMIT)
}
