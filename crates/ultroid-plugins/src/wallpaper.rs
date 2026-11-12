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
//! All four commands are wired to the `Responder` wallpaper/theme ops
//! (`list_wallpapers`, `upload_wallpaper`, `list_themes`,
//! `set_chat_theme`). `.wallsave` keeps Python's fallback path: when
//! the upload request itself fails, the replied media is sent back as
//! a photo. `.settheme off` still has no trait op (clearing a theme
//! needs `InputChatThemeEmpty`, which `set_chat_theme` cannot express)
//! and returns an honest error for that case only.

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
    // Python: `account.GetWallPapersRequest(hash=0)`, list id/slug +
    // default/dark/pattern flags, truncated at 25.
    let walls = ctx.responder.list_wallpapers().await?;
    if walls.is_empty() {
        ctx.reply("`No wallpapers saved on this account.`").await?;
        return Ok(());
    }
    let mut lines = vec![format!("**Wallpapers** ({}):", walls.len())];
    for w in walls.iter().take(WALL_LIST_LIMIT) {
        let slug = w.title.as_deref().unwrap_or("?");
        let mut flags = Vec::new();
        if w.dark {
            flags.push("dark");
        }
        if w.pattern {
            flags.push("pattern");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" ({})", flags.join(", "))
        };
        lines.push(format!("• `{slug}`{flags}"));
    }
    if walls.len() > WALL_LIST_LIMIT {
        lines.push(format!(
            "`… +{} more (list truncated)`",
            walls.len() - WALL_LIST_LIMIT
        ));
    }
    ctx.reply(&lines.join("\n")).await?;
    Ok(())
}

async fn save_wallpaper(ctx: &Ctx) -> Result<()> {
    let reply_id = match ctx.reply_to_id {
        Some(id) => id,
        None => {
            ctx.reply("`Reply to a photo/document to save it as a wallpaper.`")
                .await?;
            return Ok(());
        }
    };
    // With a local file path the file itself is on disk; otherwise the
    // replied media is downloaded first.
    let local = ctx
        .args
        .split_whitespace()
        .map(PathBuf::from)
        .find(|p| p.is_file());
    let (path, is_temp) = match local {
        Some(p) => (p, false),
        None => {
            let dest = std::env::temp_dir().join(format!("ultroid-wall-{}", uuid::Uuid::new_v4()));
            let downloaded = ctx
                .responder
                .download_media(ctx.chat_id, reply_id as i32, &dest)
                .await?;
            (downloaded, true)
        }
    };
    // Python: `account.UploadWallPaperRequest(file, mime_type, settings)`
    // then `account.SaveWallPaperRequest(...)`; on RPCError it falls back
    // to sending the media back as a photo.
    match ctx.responder.upload_wallpaper(&path).await {
        Ok(()) => {
            if is_temp {
                let _ = tokio::fs::remove_file(&path).await;
            }
            ctx.reply("`Wallpaper saved.`").await?;
        }
        Err(_) => {
            ctx.send_file(
                &path,
                "`Couldn't save this as a wallpaper — sending it back as a photo instead.`",
            )
            .await?;
            if is_temp {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
    Ok(())
}

async fn list_themes(ctx: &Ctx) -> Result<()> {
    // Python: `account.GetChatThemesRequest(hash=0)`, emoticons listed,
    // truncated at 40.
    let themes = ctx.responder.list_themes().await?;
    if themes.is_empty() {
        ctx.reply("`No chat themes available on this account.`")
            .await?;
        return Ok(());
    }
    let emoticons: Vec<&str> = themes
        .iter()
        .take(THEME_LIST_LIMIT)
        .filter_map(|t| t.emoticon.as_deref())
        .collect();
    let mut lines = vec![
        format!("**Chat themes** ({}):", themes.len()),
        emoticons.join(" "),
    ];
    if themes.len() > THEME_LIST_LIMIT {
        lines.push(format!(
            "`… +{} more (list truncated)`",
            themes.len() - THEME_LIST_LIMIT
        ));
    }
    lines.push(String::new());
    lines.push("Use `.settheme <emoticon>` to apply one to this chat.".to_string());
    ctx.reply(&lines.join("\n")).await?;
    Ok(())
}

async fn set_theme(ctx: &Ctx) -> Result<()> {
    let emoticon = ctx.args.trim();
    if emoticon.is_empty() {
        ctx.reply("`Usage: .settheme <emoticon>` (or `off` to clear)")
            .await?;
        return Ok(());
    }
    let is_off = emoticon.eq_ignore_ascii_case("off")
        || emoticon.eq_ignore_ascii_case("none")
        || emoticon.eq_ignore_ascii_case("clear");
    if is_off {
        // Clearing a theme needs `InputChatThemeEmpty`, which the
        // trait's `set_chat_theme` (slug-based) cannot express.
        return Err(Error::Other(
            "clearing a chat theme requires a binary Responder op (messages.SetChatThemeRequest with InputChatThemeEmpty) (Python: wallpaper.py:_set_theme)".into(),
        ));
    }
    // Python: `messages.SetChatThemeRequest(peer, theme)` with
    // `InputChatTheme(emoticon)`. The trait applies a theme by slug
    // and resolves the emoticon itself, so look the emoticon up first.
    let themes = ctx.responder.list_themes().await?;
    let Some(theme) = themes
        .iter()
        .find(|t| t.emoticon.as_deref() == Some(emoticon))
    else {
        ctx.reply(&format!(
            "`No chat theme with emoticon {emoticon} — see .themes for the list.`"
        ))
        .await?;
        return Ok(());
    };
    ctx.responder
        .set_chat_theme(ctx.chat_id, &theme.slug)
        .await?;
    ctx.reply(&format!("`Chat theme set to {emoticon}.`"))
        .await?;
    Ok(())
}

/// Limits kept for the Responder wiring pass (Python module constants).
#[allow(dead_code)]
fn _limits() -> (usize, usize) {
    (WALL_LIST_LIMIT, THEME_LIST_LIMIT)
}
