//! Setup-guide / repo commands — port of `plugins/_ultroid.py`
//! (`.repo`, `.ultroid`).
//!
//! Both commands prefer delivering through the assistant bot's inline
//! results (`.repo`) or file send (`.ultroid`), wired via
//! `Responder::inline_query_click` / the assistant responder's
//! `send_file_to`; the plain-text fallbacks Python already has are kept
//! for when the assistant is not connected or the inline send fails.
//! The `.ultroid` "Start" inline button cannot be attached — the trait
//! has no file-send-with-markup op — so the guide goes out without it.

use crate::util::{def, reg_add};
use ultroid_core::{Ctx, Error, Registry, Result};

pub const REPOMSG: &str = "
• **ULTROID USERBOT** •\n
• Repo - [Click Here](https://github.com/TeamUltroid/Ultroid)
• Addons - [Click Here](https://github.com/TeamUltroid/UltroidAddons)
• Support - @UltroidSupportChat
";

pub const ULTSTRING: &str = "<b>Ultroid is ready</b>\n\n\
Open this guide for the first steps, useful commands, and support links.";

pub const DEPLOYMENT_FALLBACK: &str = "<b>Ultroid is ready</b>\n\n\
Start with <code>.help</code> to browse commands, or <code>.cmds</code> for the complete command index.\n\n\
<a href=\"https://t.me/UltroidSupportChat\">Support</a>";

pub const GUIDE_IMAGE_URL: &str = "https://graph.org/file/54a917cc9dbb94733ea5f.jpg";

/// Send the usable text guide — port of `_ultroid.py::_send_fallback_guide`.
/// Downloads the guide image and sends it with the fallback caption; on
/// any failure sends the plain-text guide instead.
async fn send_fallback_guide(ctx: &Ctx, _target_chat_id: i64) -> Result<()> {
    // `Responder::send_file` targets the current chat (there is no
    // send-to-arbitrary-chat op), so the guide goes to this chat.
    let resp = ctx.http.get(GUIDE_IMAGE_URL).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let bytes = r.bytes().await.map_err(Error::from)?;
            let path = std::env::temp_dir().join("ultroid-setup-guide.jpg");
            tokio::fs::write(&path, &bytes).await?;
            let sent = ctx.send_file(&path, DEPLOYMENT_FALLBACK).await;
            let _ = tokio::fs::remove_file(&path).await;
            sent?;
            Ok(())
        }
        _ => ctx.reply_html(DEPLOYMENT_FALLBACK).await.map(|_| ()),
    }
}

/// `.repo` — port of `_ultroid.py::repify`. Prefers the assistant inline
/// result; falls back to the plain repo message (Python's own fallback —
/// the inline query is attempted first and any failure falls through).
async fn repo(ctx: &Ctx) -> Result<()> {
    if let Some(asst_id) = ctx.asst_dialog_id {
        if ctx
            .responder
            .inline_query_click(asst_id, "", ctx.chat_id, None)
            .await
            .is_ok()
        {
            let _ = ctx.delete(ctx.message_id).await;
            return Ok(());
        }
        tracing::debug!("repo: assistant inline unavailable; using REPOMSG fallback");
    }
    ctx.reply(REPOMSG).await.map(|_| ())
}

/// `.ultroid` — port of `_ultroid.py::useUltroid`. The assistant-bot file
/// send is the primary path; the user-client fallback guide is used when
/// the assistant is not connected or its send fails.
async fn use_ultroid(ctx: &Ctx) -> Result<()> {
    let target = if ctx.config.log_channel != 0 {
        ctx.config.log_channel
    } else {
        ctx.chat_id
    };
    // Primary path (Python: `asst.send_file(target, guide image,
    // caption=ULTSTRING, buttons=[Start])`): the "Start" inline button
    // cannot be attached — the trait has no file-send-with-markup op —
    // so the file goes out without it.
    if let Some(asst) = &ctx.asst {
        let resp = ctx.http.get(GUIDE_IMAGE_URL).send().await;
        if let Ok(r) = resp {
            if r.status().is_success() {
                if let Ok(bytes) = r.bytes().await {
                    let path = std::env::temp_dir().join("ultroid-setup-guide.jpg");
                    if tokio::fs::write(&path, &bytes).await.is_ok() {
                        let sent = asst.send_file_to(target, &path, ULTSTRING).await;
                        let _ = tokio::fs::remove_file(&path).await;
                        if sent.is_ok() {
                            if target == ctx.chat_id {
                                let _ = ctx.delete(ctx.message_id).await;
                            }
                            return Ok(());
                        }
                        tracing::debug!(
                            "ultroid: assistant file send failed; using fallback guide"
                        );
                    }
                }
            }
        }
    }
    match send_fallback_guide(ctx, target).await {
        Ok(()) => {
            if target == ctx.chat_id {
                let _ = ctx.delete(ctx.message_id).await;
                Ok(())
            } else {
                // No message link is available from the fallback send; say
                // where the guide went, as Python does when it has no link.
                ctx.reply("Setup guide sent to the configured log channel.")
                    .await
                    .map(|_| ())
            }
        }
        Err(e) => {
            tracing::error!("ultroid: fallback guide send failed: {e}");
            ctx.reply("Could not send the setup guide. Check chat access and try again.")
                .await
                .map(|_| ())
        }
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("repo", "repo$", "Ultroid repo, addons and support links."),
        |ctx| Box::pin(repo(ctx)),
    );
    reg_add(
        registry,
        def("ultroid", "ultroid$", "Open the Ultroid setup guide."),
        |ctx| Box::pin(use_ultroid(ctx)),
    );
}
