//! Assistant help menu for group administration and utility commands.
//!
//! Port of `assistant/manager/_help.py`.
//!
//! The help data, the page builders and the `help` command are ported in
//! full. What is NOT portable through the current core surface:
//!
//! * Inline buttons cannot be attached to a send: neither
//!   [`ultroid_core::Responder`] nor `ultroid_rich::RichSender::send_rich`
//!   accepts reply markup, so the `help` command sends the rich page alone.
//!   [`get_buttons`] still builds the exact button rows (category buttons
//!   plus the "Add assistant to a group" URL button) as data, for the
//!   binary to attach once a send path accepts markup.
//! * The "Add assistant to a group" / "Open help" URL buttons need the
//!   assistant bot's username (`asst.me.username`); [`Ctx`] and
//!   [`ultroid_core::Config`] carry no bot username, so those URL buttons
//!   are omitted when the username is unavailable.
//! * The callback handlers (`manager_help_button`, `home_aja`,
//!   `_category_page`) cannot be registered: `Registry` dispatches message
//!   commands only, there is no CallbackQuery dispatch surface. The
//!   callback-data builders ([`home_callback_data`],
//!   [`category_callback_data`]) and the page builders are ported so the
//!   binary can wire the callbacks once such a surface exists.

use ultroid_core::error::Result;
use ultroid_core::fns::escape_html;
use ultroid_core::rich::RichDoc;
use ultroid_core::router::{CommandDef, Registry};
use ultroid_core::tg::{ButtonAction, ButtonStyle, InlineButton};
use ultroid_core::{ChatType, Ctx};

use crate::misc_assistant::fn_handler;

/// Help sections: (command, purpose) pairs, exactly as in Python's
/// `HELP_SECTIONS`, in the same order.
pub const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Admintools",
        &[
            ("/pin <reply>", "Pin the replied-to message."),
            ("/pinned", "Show the current pinned message."),
            ("/unpin <reply>", "Unpin the replied-to message."),
            ("/unpin all", "Remove all pinned messages."),
            (
                "/ban <user> [reason]",
                "Ban a user by reply, ID, or username.",
            ),
            ("/unban <user>", "Unban a user."),
            ("/mute <user>", "Mute a user."),
            ("/unmute <user>", "Unmute a user."),
            ("/tban <user> <time>", "Temporarily ban a user."),
            ("/tmute <user> <time>", "Temporarily mute a user."),
            (
                "/purge <reply>",
                "Delete messages starting at the replied-to message.",
            ),
            (
                "/setgpic <reply to photo>",
                "Set the group's profile photo.",
            ),
            ("/delgpic", "Remove the current group photo."),
        ],
    ),
    (
        "locks",
        &[
            (
                "/lock <item>",
                "Restrict a type of content in the current chat.",
            ),
            (
                "/unlock <item>",
                "Allow a previously restricted type of content.",
            ),
            ("msgs", "Messages."),
            ("inlines", "Inline queries."),
            ("media", "Photos, files, and other media."),
            ("games", "Games."),
            ("sticker", "Stickers."),
            ("polls", "Polls."),
            ("gif", "GIFs."),
            ("pin", "Pinning messages."),
            ("changeinfo", "Changing chat information."),
        ],
    ),
    (
        "Utils",
        &[
            (
                "/info <user>",
                "Show details for a user by reply, ID, or username.",
            ),
            ("/id", "Show the current chat and sender IDs."),
            ("/tr <text>", "Translate text."),
            ("/q <reply>", "Create a quote from a message."),
            (
                "/paste <reply or text>",
                "Upload text or a file to Spacebin.",
            ),
            ("/meaning <word>", "Look up a word's meaning."),
            ("/google <query>", "Search the web."),
            ("/suggest <question>", "Create a yes/no poll."),
        ],
    ),
    (
        "Search",
        &[
            ("/search <query>", "Search the web for a query."),
            (
                "/deepresearch <query>",
                "Run a deeper, multi-step research search.",
            ),
        ],
    ),
    (
        "Misc",
        &[
            ("/joke", "Get a random joke."),
            ("/decide <options>", "Choose between options."),
            ("/kang <reply to sticker>", "Add a sticker to your pack."),
            ("/listpack", "List your sticker packs."),
        ],
    ),
    (
        "Assistant",
        &[
            ("/agent", "Configure an AI provider."),
            ("/akinator", "Play Akinator in a private chat."),
            ("/startgame", "Start a group game."),
            ("/afk [reason]", "Set or clear your away status."),
            ("/eph <reply>", "Send an ephemeral message."),
            ("/whisper <reply>", "Send a private whisper."),
            ("@botusername <query>", "Open an inline assistant tool."),
        ],
    ),
];

/// Display labels for the sections (Python `CATEGORY_LABELS`).
pub fn category_label(category: &str) -> &'static str {
    match category {
        "Admintools" => "Admin tools",
        "locks" => "Chat locks",
        "Utils" => "Utilities",
        "Search" => "Search",
        "Misc" => "More tools",
        "Assistant" => "Assistant",
        _ => "",
    }
}

/// Section ids in order (Python `CATEGORIES`).
pub const CATEGORIES: &[&str] = &[
    "Admintools",
    "locks",
    "Utils",
    "Search",
    "Misc",
    "Assistant",
];

/// Items per help page (Python `HELP_PAGE_SIZE`).
pub const HELP_PAGE_SIZE: usize = 6;

/// Callback data for the help home page: `cb("help", "manager", "home")`.
pub fn home_callback_data() -> Vec<u8> {
    b"help:manager:home".to_vec()
}

/// Callback data for the manager-help button: `cb_exact_pattern("help",
/// "manager", "mngbtn")`.
pub fn manager_button_callback_data() -> Vec<u8> {
    b"help:manager:mngbtn".to_vec()
}

/// Callback data for one category page: `cb("help", cat, page)`.
pub fn category_callback_data(category: &str, page: usize) -> Vec<u8> {
    format!("help:{category}:{page}").into_bytes()
}

/// Shared page footer (Python `rich_pages.page_footer`):
/// `page {page} of {pages} • total {total}`, 1-based and clamped.
pub fn page_footer(page: usize, pages: usize, total: usize) -> String {
    let pages = pages.max(1);
    let page = page.clamp(1, pages);
    format!("page {page} of {pages} • total {total}")
}

/// Build a native rich page: large heading, optional description, divider,
/// bordered table, divider, italic page index (Python
/// `rich_pages.rich_page`).
pub fn rich_page_html(
    title: &str,
    rows: &[(&str, &str)],
    description: Option<&str>,
    columns: (&str, &str),
    page: usize,
    pages: usize,
    total: usize,
) -> String {
    let mut parts = vec![format!("<h1>{}</h1>", escape_html(title))];
    if let Some(desc) = description {
        parts.push(format!("<p>{}</p>", escape_html(desc)));
    }
    parts.push("<hr>".to_string());
    parts.push("<table class=\"bordered compact\">".to_string());
    parts.push(format!(
        "<tr><th>{}</th><th>{}</th></tr>",
        escape_html(columns.0),
        escape_html(columns.1)
    ));
    for (a, b) in rows {
        parts.push(format!(
            "<tr><td>{}</td><td>{}</td></tr>",
            escape_html(a),
            escape_html(b)
        ));
    }
    parts.push("</table><hr>".to_string());
    parts.push(format!(
        "<p><i>{}</i></p>",
        escape_html(&page_footer(page, pages, total))
    ));
    parts.join("")
}

/// The help home page: one row per section (Python `_help_home_page`).
pub fn help_home_page() -> Result<RichDoc> {
    let rows: Vec<(String, String)> = CATEGORIES
        .iter()
        .map(|cat| {
            (
                category_label(cat).to_string(),
                "Browse commands and controls in this section.".to_string(),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    RichDoc::html(rich_page_html(
        "Group Help",
        &refs,
        Some("Choose a section to browse commands and their purpose."),
        ("Section", "What it contains"),
        1,
        1,
        CATEGORIES.len(),
    ))
}

/// The "Open Help" page sent in groups (Python `helpish`, non-private
/// branch): a single row pointing at the private-chat help.
pub fn open_help_page() -> Result<RichDoc> {
    RichDoc::html(rich_page_html(
        "Open Help",
        &[("Open the assistant", "Browse help in a private chat.")],
        None,
        ("Action", "What it does"),
        1,
        1,
        1,
    ))
}

/// The category buttons, two per row, plus — when the assistant bot's
/// username is known — the "Add assistant to a group" URL button
/// (Python `get_buttons`).
pub fn get_buttons(bot_username: Option<&str>) -> Vec<Vec<InlineButton>> {
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let mut rest: &[&str] = CATEGORIES;
    while !rest.is_empty() {
        let take = rest.len().min(2);
        rows.push(
            rest[..take]
                .iter()
                .map(|cat| InlineButton {
                    text: category_label(cat).to_string(),
                    action: ButtonAction::Data(category_callback_data(cat, 0)),
                    style: ButtonStyle::Primary,
                })
                .collect(),
        );
        rest = &rest[take..];
    }
    if let Some(username) = bot_username {
        rows.push(vec![InlineButton::url(
            "Add assistant to a group",
            format!("https://t.me/{username}?startgroup=true"),
        )]);
    }
    rows
}

/// One category page: the rich page plus its navigation buttons (Python
/// `_help_page`). Returns `None` for an unknown category. Page is clamped
/// to the available range, exactly as in Python.
pub fn help_page(category: &str, page: usize) -> Option<(RichDoc, Vec<Vec<InlineButton>>)> {
    let items = HELP_SECTIONS.iter().find(|(cat, _)| *cat == category)?.1;
    let total_pages = items.len().div_ceil(HELP_PAGE_SIZE).max(1);
    let page = page.min(total_pages - 1);
    let start = page * HELP_PAGE_SIZE;
    let selected = &items[start..(start + HELP_PAGE_SIZE).min(items.len())];
    let content = RichDoc::html(rich_page_html(
        category_label(category),
        selected,
        Some("Commands and what they do."),
        ("Command", "Purpose"),
        page + 1,
        total_pages,
        items.len(),
    ))
    .ok()?;
    let mut nav_row: Vec<InlineButton> = Vec::new();
    if page > 0 {
        nav_row.push(InlineButton {
            text: "Previous".to_string(),
            action: ButtonAction::Data(category_callback_data(category, page - 1)),
            style: ButtonStyle::Primary,
        });
    }
    if total_pages > 1 {
        nav_row.push(InlineButton {
            text: format!("{} / {}", page + 1, total_pages),
            action: ButtonAction::Data(category_callback_data(category, page)),
            style: ButtonStyle::Default,
        });
    }
    if page < total_pages - 1 {
        nav_row.push(InlineButton {
            text: "Next".to_string(),
            action: ButtonAction::Data(category_callback_data(category, page + 1)),
            style: ButtonStyle::Primary,
        });
    }
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    if !nav_row.is_empty() {
        rows.push(nav_row);
    }
    rows.push(vec![InlineButton {
        text: "Back to sections".to_string(),
        action: ButtonAction::Data(home_callback_data()),
        style: ButtonStyle::Default,
    }]);
    Some((content, rows))
}

/// `DUAL_MODE` DB key, parsed the way the Python check needs it: truthy
/// when the stored value reads as enabled.
async fn dual_mode_on(ctx: &Ctx) -> bool {
    match ctx.db.get("DUAL_MODE").await {
        Ok(Some(v)) => {
            let v = v.trim().trim_matches('"').to_lowercase();
            matches!(v.as_str(), "true" | "1" | "yes" | "on")
        }
        _ => false,
    }
}

/// The `help` command (Python `helpish`, `@asst_cmd(pattern="help")`).
///
/// In a group: send the "Open Help" rich page (the "Open help" URL button
/// needs the bot username, which is unavailable in this crate — see the
/// module docs). In a private chat: owner/sudo senders are ignored when
/// dual mode is on with `/` as the dual handler, exactly as in Python;
/// everyone else gets the home page.
pub async fn help_command(ctx: &Ctx) -> Result<()> {
    if ctx.chat_type != ChatType::Private {
        let doc = open_help_page()?;
        ctx.send_rich_or_plain(&doc).await?;
        return Ok(());
    }
    if (ctx.is_owner || ctx.is_sudo) && dual_mode_on(ctx).await && ctx.config.dual_hndlr == "/" {
        return Ok(());
    }
    let doc = help_home_page()?;
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

/// Register the `help` command. The callback handlers from `_help.py`
/// (`help:manager:mngbtn`, `help:manager:home`, `help:<category>:<page>`)
/// have no CallbackQuery dispatch surface to register on — see the module
/// docs.
pub fn register(registry: &mut Registry) {
    registry.add(
        CommandDef {
            name: "help",
            pattern: "help",
            help: "Show the group-manager help menu.",
            ..Default::default()
        },
        fn_handler(|ctx| Box::pin(help_command(ctx))),
    );
}
