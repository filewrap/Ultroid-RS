//! Forum topics — port of `plugins/topics.py`.
//!
//! `.topics` / `.topic` / `.topicedit` / `.sendtopic` — argument parsing,
//! validation and friendly errors are fully ported. Every step needs raw
//! MTProto (`messages.GetForumTopicsRequest`, `CreateForumTopicRequest`,
//! `EditForumTopicRequest`, sending into a topic, and the `forum` flag on
//! the chat), none of which the `Ctx`/`Responder` surface exposes, so
//! each handler returns the honest `TODO(responder-op)` error at the exact
//! point the RPC would run.

use crate::util::{def, reg_add};
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

pub const HELP: &str = "✘ Commands Available -\n\n• `{i}topics`\n    List forum topics of the current group (only works in forum-enabled\n    supergroups).\n\n• `{i}topic <name>`\n    Create a new forum topic titled `<name>` in the current group.\n    Needs \"Manage Topics\" admin rights.\n\n• `{i}topicedit <id> <name>`\n    Rename forum topic `<id>` to `<name>`.\n\n• `{i}sendtopic <id> <text>`\n    Send `<text>` into forum topic `<id>` of the current group.\n";

pub const NOT_FORUM: &str =
    "`This chat isn't a forum-enabled supergroup — topics aren't available here.`";
pub const NEED_ADMIN: &str = "`I need \"Manage Topics\" admin rights in this chat for that.`";

pub fn register(registry: &mut Registry) {
    let group_fullsudo = |name: &'static str, pattern: &'static str| CommandDef {
        name,
        pattern,
        help: HELP,
        groups_only: true,
        fullsudo: true,
        ..def(name, pattern, HELP)
    };
    reg_add(registry, group_fullsudo("topics", "topics$"), |ctx| {
        Box::pin(list_topics(ctx))
    });
    reg_add(registry, group_fullsudo("topic", "topic( (.*)|$)"), |ctx| {
        Box::pin(create_topic(ctx))
    });
    reg_add(
        registry,
        group_fullsudo("topicedit", "topicedit( (.*)|$)"),
        |ctx| Box::pin(edit_topic(ctx)),
    );
    reg_add(
        registry,
        group_fullsudo("sendtopic", "sendtopic( (.*)|$)"),
        |ctx| Box::pin(send_to_topic(ctx)),
    );
}

fn todo_op(op: &str, py: &str) -> Error {
    Error::Other(format!(
        "{op} requires a binary Responder op (Python: {py})"
    ))
}

/// Friendly error text for topic RPC failures (Python
/// `_friendly_topic_error`): forum-missing and admin-required get their
/// own messages; everything else is a short error block.
pub fn friendly_topic_error(err: &str) -> String {
    let upper = err.to_uppercase();
    if upper.contains("FORUM") {
        return NOT_FORUM.to_string();
    }
    if upper.contains("ADMIN") {
        return NEED_ADMIN.to_string();
    }
    format!("❌ **Error.**\n`{err}`")
}

/// One forum topic (Python `messages.GetForumTopicsRequest` result item).
#[derive(Debug, Clone)]
pub struct ForumTopic {
    pub id: i64,
    pub title: String,
    pub pinned: bool,
    pub closed: bool,
}

/// The topic list text (Python `_list_topics` rendering).
pub fn topics_list_text(topics: &[ForumTopic]) -> String {
    if topics.is_empty() {
        return "`No forum topics found in this chat.`".to_string();
    }
    let mut lines = vec![format!("**Forum topics** ({}):", topics.len())];
    for topic in topics {
        let mut flags = String::new();
        if topic.pinned {
            flags.push_str("📌");
        }
        if topic.closed {
            flags.push_str("🔒");
        }
        lines.push(
            format!("`{}` — **{}** {flags}", topic.id, topic.title)
                .trim_end()
                .to_string(),
        );
    }
    lines.join("\n")
}

async fn list_topics(ctx: &Ctx) -> Result<()> {
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): get chat (forum flag) + messages.GetForumTopicsRequest.
    // Python: plugins/topics.py:_list_forum_topics / _list_topics.
    Err(todo_op(
        "list forum topics (GetForumTopicsRequest)",
        "plugins/topics.py:_list_forum_topics",
    ))
}

async fn create_topic(ctx: &Ctx) -> Result<()> {
    let title = ctx.args.trim().to_string();
    if title.is_empty() {
        ctx.reply("`Usage: .topic <name>`").await?;
        return Ok(());
    }
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): get chat (forum flag) + messages.CreateForumTopicRequest.
    // Python: plugins/topics.py:_create_topic.
    Err(todo_op(
        "create forum topic (CreateForumTopicRequest)",
        "plugins/topics.py:_create_topic",
    ))
}

async fn edit_topic(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let mut parts = raw.splitn(2, ' ');
    let (id_part, new_title) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("").trim(),
    );
    let Ok(topic_id) = id_part.parse::<i64>() else {
        ctx.reply("`Usage: .topicedit <id> <name>`").await?;
        return Ok(());
    };
    if new_title.is_empty() {
        ctx.reply("`Usage: .topicedit <id> <name>`").await?;
        return Ok(());
    }
    let _ = ctx.reply(&ctx.get_str("com_1")).await?;
    let _ = topic_id;
    // TODO(responder-op): get chat (forum flag) + messages.EditForumTopicRequest.
    // Python: plugins/topics.py:_edit_topic.
    Err(todo_op(
        "rename forum topic (EditForumTopicRequest)",
        "plugins/topics.py:_edit_topic",
    ))
}

async fn send_to_topic(ctx: &Ctx) -> Result<()> {
    let raw = ctx.args.trim();
    let mut parts = raw.splitn(2, ' ');
    let (id_part, text) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let Ok(topic_id) = id_part.parse::<i64>() else {
        ctx.reply("`Usage: .sendtopic <id> <text>`").await?;
        return Ok(());
    };
    if text.trim().is_empty() {
        ctx.reply("`Usage: .sendtopic <id> <text>`").await?;
        return Ok(());
    }
    let _ = topic_id;
    // TODO(responder-op): get chat (forum flag) + send_message(chat, text, reply_to=topic_id).
    // Python: plugins/topics.py:_send_to_topic.
    Err(todo_op(
        "send message into forum topic",
        "plugins/topics.py:_send_to_topic",
    ))
}
