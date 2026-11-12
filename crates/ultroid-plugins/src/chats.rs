//! Chat management — port of `plugins/chats.py`
//! (`.delchat` / `.getlink` / `.create` / `.setgpic` / `.delgpic` /
//! `.unbanall` / `.rmusers`).
//!
//! Every command here bottoms out in a raw MTProto call
//! (`channels.DeleteChannelRequest`, `messages.ExportChatInviteRequest`,
//! `messages.CreateChatRequest`, `channels.CreateChannelRequest`,
//! `channels.UpdateUsernameRequest`, `channels.EditPhotoRequest`,
//! `messages.GetFullChatRequest` / `channels.GetFullChannelRequest`,
//! `iter_participants` + `kick_participant` / `edit_permissions`).
//! None of those exists on the frozen `Ctx`/`Responder` surface, so the
//! argument parsing and validation of each command is ported faithfully
//! and the Telegram call itself is an honest `TODO(responder-op)` error
//! at the exact call site.
//!
//! NOTE: Python's `require="change_info"` / `require="ban_users"` flags
//! (admin-rights requirements on `.setgpic` / `.delgpic` / `.unbanall`)
//! have no field on `ultroid_core::CommandDef`; that requirement is
//! recorded here and in the wave report, not silently dropped into a
//! different gate.

use crate::util::{def, reg_add};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

fn todo_op(op: &str, pyref: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {pyref})"
    ))
}

/// `.delchat [id]` — port of `chats.py` delchat handler.
async fn delchat(ctx: &Ctx) -> Result<()> {
    let _x = ctx.reply(&ctx.get_str("com_1")).await?;
    let _target = if ctx.args.trim().is_empty() {
        ctx.chat_id
    } else {
        // Python: `e.client.parse_id(match)` — resolves a username/id to
        // an entity id.
        ctx.args.trim().parse::<i64>().unwrap_or(ctx.chat_id)
    };
    // TODO(responder-op): channels.DeleteChannelRequest(chat)
    // (Python: chats.py:delchat).
    Err(todo_op("channels.DeleteChannelRequest", "chats.py:delchat"))
}

/// Parsed `.getlink` options: `(request_needed, usage_limit, title)`.
/// Port of the parsing block in `chats.py` getlink handler.
pub fn parse_getlink_args(input: &str) -> (bool, Option<u32>, Option<String>) {
    let mut request = false;
    let mut usage: Option<u32> = None;
    let mut title: Option<String> = None;
    let input = input.trim();
    if input.is_empty() {
        return (request, usage, title);
    }
    let mut split = input.splitn(2, char::is_whitespace);
    let first = split.next().unwrap_or("");
    let rest = split.next().unwrap_or("");
    request = first == "r" || first == "request";
    title = Some("Created by Ultroid".to_string());
    if !rest.is_empty() {
        let mut spli = rest.splitn(2, char::is_whitespace);
        let s0 = spli.next().unwrap_or("");
        let s1 = spli.next().unwrap_or("");
        if s0.chars().all(|c| c.is_ascii_digit()) && !s0.is_empty() {
            usage = s0.parse::<u32>().ok();
        }
        if !s1.is_empty() {
            title = Some(s1.to_string());
        }
    } else if !request {
        if input.chars().all(|c| c.is_ascii_digit()) {
            usage = input.parse::<u32>().ok();
        } else {
            title = Some(input.to_string());
        }
    }
    if request && usage.is_some() {
        usage = Some(0);
    }
    (request, usage, title)
}

/// `.getlink [r|usage|title]` — port of `chats.py` getlink handler.
async fn getlink(ctx: &Ctx) -> Result<()> {
    let _x = ctx.reply(&ctx.get_str("com_1")).await?;
    if ctx.reply_to_id.is_some() {
        // Python resolves the replied-to sender when it is not a User;
        // that needs a replied-message fetch.
        tracing::debug!("getlink: replied-message sender resolution needs a Responder op");
    }
    let (request, usage, title) = parse_getlink_args(&ctx.args);
    let _ = (request, usage, title);
    // TODO(responder-op): messages.ExportChatInviteRequest (when a new
    // link is requested) or messages.GetFullChatRequest /
    // channels.GetFullChannelRequest for the existing exported invite
    // (Python: chats.py:getlink).
    Err(todo_op(
        "messages.ExportChatInviteRequest / messages.GetFullChatRequest / channels.GetFullChannelRequest",
        "chats.py:getlink",
    ))
}

/// `.create (b|g|c) <name> [ ; <username>]` — port of `chats.py` create.
async fn create(ctx: &Ctx) -> Result<()> {
    let kind = ctx.captures.get(1).cloned().unwrap_or_default();
    let mut group_name = ctx.captures.get(2).cloned().unwrap_or_default();
    let mut username: Option<String> = None;
    if let Some(idx) = group_name.find(" ; ") {
        username = Some(group_name[idx + 3..].to_string());
        group_name.truncate(idx);
    }
    let _x = ctx.reply(&ctx.get_str("com_1")).await?;
    match kind.as_str() {
        "b" => {
            // TODO(responder-op): messages.CreateChatRequest(users=[asst],
            // title) + messages.ExportChatInviteRequest (Python:
            // chats.py:create, "b" branch).
            let _ = username;
            Err(todo_op(
                "messages.CreateChatRequest + messages.ExportChatInviteRequest",
                "chats.py:create",
            ))
        }
        "g" | "c" => {
            // TODO(responder-op): channels.CreateChannelRequest(title,
            // about, megagroup) + optional channels.UpdateUsernameRequest /
            // messages.ExportChatInviteRequest (Python: chats.py:create,
            // "g"/"c" branch).
            let _ = (group_name, username);
            Err(todo_op(
                "channels.CreateChannelRequest + channels.UpdateUsernameRequest / messages.ExportChatInviteRequest",
                "chats.py:create",
            ))
        }
        _ => ctx
            .reply("Usage: `.create (b|g|c) <group_name> ; <optional-username>`")
            .await
            .map(|_| ()),
    }
}

/// `.setgpic [chat]` (reply to media) — port of `chats.py` setgpic.
/// Requires the `change_info` admin right (no `CommandDef` field for it).
async fn setgpic(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        return ctx.reply("`Reply to a Media..`").await.map(|_| ());
    }
    // TODO(responder-op): download replied media, convert to jpg/png/mp4,
    // upload, channels.EditPhotoRequest(chat, photo)
    // (Python: chats.py:setgpic).
    Err(todo_op(
        "download replied media + channels.EditPhotoRequest",
        "chats.py:setgpic",
    ))
}

/// `.delgpic [chat]` — port of `chats.py` delgpic.
/// Requires the `change_info` admin right (no `CommandDef` field for it).
async fn delgpic(ctx: &Ctx) -> Result<()> {
    let _target = ctx.args.trim();
    // TODO(responder-op): channels.EditPhotoRequest(chat,
    // types.InputChatPhotoEmpty()) (Python: chats.py:delgpic).
    Err(todo_op(
        "channels.EditPhotoRequest with InputChatPhotoEmpty",
        "chats.py:delgpic",
    ))
}

/// `.unbanall` — port of `chats.py` unbanall.
/// Requires the `ban_users` admin right (no `CommandDef` field for it).
async fn unbanall(ctx: &Ctx) -> Result<()> {
    let _x = ctx.reply("Searching Participant Lists.").await?;
    // TODO(responder-op): iter_participants(filter=
    // ChannelParticipantsKicked, aggressive=True) + edit_permissions(view_messages=True)
    // (Python: chats.py:unbanall).
    Err(todo_op(
        "iter_participants(ChannelParticipantsKicked) + edit_permissions unban",
        "chats.py:unbanall",
    ))
}

/// `.rmusers [deleted|empty|month|week|offline|online|recently|bot|none]`
/// — port of `chats.py` rmusers. Without a participant list the counts
/// cannot be computed, so the iteration itself is the TODO site.
async fn rmusers(ctx: &Ctx) -> Result<()> {
    let _x = ctx.reply(&ctx.get_str("com_1")).await?;
    let _input = ctx.args.trim();
    // TODO(responder-op): iter_participants(chat) with per-user status
    // classification + kick_participant for the requested classes
    // (Python: chats.py:rmusers).
    Err(todo_op(
        "iter_participants + kick_participant by status class",
        "chats.py:rmusers",
    ))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "delchat",
            pattern: "delchat",
            help: "Delete the group this command is used in.",
            groups_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(delchat(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "getlink",
            pattern: "getlink( (.*)|$)",
            help: "Get the invite link of the group.",
            groups_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(getlink(ctx)),
    );
    reg_add(
        registry,
        def(
            "create",
            "create (b|g|c)(?: |$)(.*)",
            "Create a group (b/g) or channel (c).",
        ),
        |ctx| Box::pin(create(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "setgpic",
            pattern: "setgpic( (.*)|$)",
            help: "Set the group photo (reply to a photo/video). Requires change_info.",
            admins_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(setgpic(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "delgpic",
            pattern: "delgpic( (.*)|$)",
            help: "Remove the group photo. Requires change_info.",
            admins_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(delgpic(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "unbanall",
            pattern: "unbanall$",
            help: "Unban all members of a group. Requires ban_users.",
            admins_only: true,
            ..Default::default()
        },
        |ctx| Box::pin(unbanall(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "rmusers",
            pattern: "rmusers( (.*)|$)",
            help: "Remove users by status class (deleted/empty/month/week/offline/online/recently/bot/none).",
            groups_only: true,
            admins_only: true,
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(rmusers(ctx)),
    );
}
