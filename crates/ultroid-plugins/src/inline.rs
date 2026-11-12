//! Inline-mode engine — port of `plugins/_inline.py` (1,369 LOC).
//!
//! This module owns **no** `.`-commands, so `register` registers nothing;
//! the binary crate owns the inline-query dispatcher, the callback-query
//! router, and the `InlineResultBuilder` (grammers) calls. Everything in
//! this file is the pure query-parsing + result-building logic the
//! dispatcher needs:
//!
//! * query routing — empty → alive card, `ultd` → help menu,
//!   `pasta-<id>` → Spacebin links, `stf<n>` → the `something()` store;
//! * the alive card HTML (`_alive_rich_markup`) and the deploy guide
//!   (`_deploy_guide_blocks`, rendered as Markdown — `RichDoc` cannot
//!   carry native `PageBlock` trees, so the binary sends it as
//!   `RichDoc::markdown`);
//! * the help catalog pagination math + plugin-card HTML
//!   (`_catalog_help_blocks`, `_plugin_card_markup`, `page_num`);
//! * the `STUFF` store behind `something()` and its media-type detection;
//! * callback-data builders (`cb(...)` joins with `:`) and the manifest
//!   placeholder-button logic (`_placeholder_button`, `_rebuild_buttons`).
//!
//! Deliberately **not** ported here (owned by other layers):
//! `_help_blocks` / `main_help_menu_buttons` / `_page_footer`'s callers in
//! `_help.py` (the help plugin — the binary passes their output in),
//! `HELP`/`LIST` registries (the loader), `core.manifest` PMF state (the
//! manifest layer), and the updater engine behind `doupdate`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use ultroid_core::Registry;
use ultroid_core::fns::time_formatter;

/// The inline result id of the alive/deploy article (Python
/// `INLINE_ALIVE_RESULT_ID`). The binary's `UpdateBotInlineSend` handler
/// matches on it — see [`is_deploy_inline_send`].
pub const INLINE_ALIVE_RESULT_ID: &str = "alive-deploy";

/// Settle delay before editing the freshly-sent alive article (Python
/// `INLINE_DEPLOY_SEND_DELAY_SECONDS`).
pub const INLINE_DEPLOY_SEND_DELAY_SECONDS: f64 = 3.5;

/// This module owns no commands: the binary owns the inline dispatcher.
/// Kept so the coordinator can call every plugin's `register` uniformly.
pub fn register(_registry: &mut Registry) {}

// ── inline buttons (plain data; the binary builds real buttons) ──────────

/// One inline button (label + exactly one of callback data / URL).
#[derive(Debug, Clone, PartialEq)]
pub struct InlineButton {
    pub label: String,
    pub data: Option<String>,
    pub url: Option<String>,
}

impl InlineButton {
    pub fn callback(label: impl Into<String>, data: impl Into<String>) -> Self {
        InlineButton {
            label: label.into(),
            data: Some(data.into()),
            url: None,
        }
    }
    pub fn url(label: impl Into<String>, url: impl Into<String>) -> Self {
        InlineButton {
            label: label.into(),
            data: None,
            url: Some(url.into()),
        }
    }
}

/// Callback-data builder (Python `pyUltroid._core._cbs.cb`: parts joined
/// with `:`).
pub fn cb(parts: &[&str]) -> String {
    parts.join(":")
}

/// Support buttons: repo + support-chat links (Python `SUP_BUTTONS`).
pub fn sup_buttons(repo_label: &str, support_label: &str) -> Vec<Vec<InlineButton>> {
    vec![vec![
        InlineButton::url(repo_label, "https://github.com/TeamUltroid/Ultroid"),
        InlineButton::url(support_label, "t.me/UltroidSupportChat"),
    ]]
}

// ── query routing ─────────────────────────────────────────────────────────

/// What an inline query asks for (Python's `@in_pattern` handlers).
#[derive(Debug, Clone, PartialEq)]
pub enum InlineQueryKind {
    /// Empty query → the alive card.
    Alive,
    /// `ultd` → the bot-help menu.
    Help,
    /// `alive` → the status card from `plugins/bot.py`.
    AliveStatus,
    /// `pasta-<id>` → Spacebin paste links.
    Pasta(String),
    /// `stf<n>` → a `something()` stored result.
    Stuff(usize),
    /// Anything else (other plugins own their patterns).
    Other(String),
}

/// Route an inline query string to its handler (Python's `in_pattern`
/// registrations in `_inline.py`).
pub fn parse_inline_query(query: &str) -> InlineQueryKind {
    let q = query.trim();
    if q.is_empty() {
        return InlineQueryKind::Alive;
    }
    if q == "ultd" {
        return InlineQueryKind::Help;
    }
    if q == "alive" {
        return InlineQueryKind::AliveStatus;
    }
    if let Some(ok) = q.strip_prefix("pasta-") {
        if !ok.is_empty() {
            return InlineQueryKind::Pasta(ok.to_string());
        }
    }
    if let Some(num) = q.strip_prefix("stf") {
        if let Ok(n) = num.trim().parse::<usize>() {
            return InlineQueryKind::Stuff(n);
        }
    }
    InlineQueryKind::Other(q.to_string())
}

/// Spacebin links for a `pasta-<id>` query (Python `pasta` handler).
pub fn pasta_links(ok: &str) -> (String, String) {
    (
        format!("https://spaceb.in/{ok}"),
        format!("https://spaceb.in/api/{ok}/raw"),
    )
}

/// Accept only the empty-query send of the alive/deploy inline result
/// (Python `_is_deploy_inline_send`).
pub fn is_deploy_inline_send(query: &str, result_id: &str, is_inline_msg_id: bool) -> bool {
    query.trim().is_empty() && result_id == INLINE_ALIVE_RESULT_ID && is_inline_msg_id
}

// ── alive card + deploy guide ─────────────────────────────────────────────

/// Footer line used across the help surfaces (Python `_help._page_footer`,
/// as observed in `_alive_rich_markup`: `page 1 of 1 • total 4`).
pub fn page_footer(page: usize, page_count: usize, total: usize) -> String {
    format!("page {page} of {page_count} • total {total}")
}

/// Build the Ultroid status/deploy card as HTML (Python
/// `_alive_rich_markup`). `uptime_ms` is milliseconds since `start_time`.
pub fn alive_rich_markup(
    owner_name: &str,
    uptime_ms: u64,
    official_count: usize,
    addon_count: usize,
    invoker_name: Option<&str>,
) -> String {
    let uptime = time_formatter(uptime_ms);
    let rows: [(&str, String); 4] = [
        ("Owner", owner_name.to_string()),
        ("Uptime", uptime),
        ("Plugins", official_count.to_string()),
        ("Addons", addon_count.to_string()),
    ];
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut parts = vec!["<h1>Ultroid · Alive</h1>".to_string(), "<hr>".to_string()];
    if let Some(name) = invoker_name {
        parts.push(format!(
            "<p>Hey {}, want to deploy your own? Tap <b>Hi</b> to continue.</p>",
            esc(name)
        ));
    }
    parts.push(
        "<table class=\"bordered compact\"><tr><th>Item</th><th>Details</th></tr>".to_string(),
    );
    for (label, value) in rows.iter() {
        parts.push(format!(
            "<tr><td>{}</td><td>{}</td></tr>",
            esc(label),
            esc(value)
        ));
    }
    parts.push(format!(
        "</table><hr><p><i>{}</i></p>",
        page_footer(1, 1, rows.len())
    ));
    parts.join("")
}

/// The deploy guide (Python `_deploy_guide_blocks`) rendered as Markdown:
/// the same 7 steps, commands, value sources and warnings. The Python
/// original builds native `PageBlock` trees (collapsible `Details`,
/// copy-button rows, tables); `RichDoc` cannot carry those, so the binary
/// sends this via `RichDoc::markdown` and the plain variant below is the
/// `blocks_to_plain_text` fallback.
pub fn deploy_guide_markdown(invoker_name: &str) -> String {
    let repo = "https://github.com/Mahesh953-hub/SarahUB";
    let docs = format!("{repo}/blob/main/docs/SETUP.md");
    format!(
        "# Deploy your own Ultroid\n\n\
         Hey {invoker_name}! Browse these steps to deploy your own Ultroid.\n\n\
         ---\n\n\
         ## Step 1 · Clone the repository\n\n\
         Use Git and Python 3.10+ (3.12 recommended). Private repositories \
         require GitHub access; authenticate securely, never paste a token \
         into the clone URL.\n\n\
         ```bash\n\
         git clone https://github.com/Mahesh953-hub/SarahUB.git Ultroid\n\
         cd Ultroid\n\
         ```\n\n\
         [Repository]({repo}) · [Setup guide]({docs})\n\n\
         ## Step 2 · Install requirements\n\n\
         Choose pip OR uv, not both. Run commands from the Ultroid folder \
         on Linux/macOS.\n\n\
         ### 2a · pip\n\n\
         ```bash\n\
         python3 -m venv .venv\n\
         source .venv/bin/activate\n\
         python -m pip install --upgrade pip\n\
         python -m pip install -r requirements.txt\n\
         ```\n\n\
         ### 2b · uv\n\n\
         Install uv first if needed, then create the environment and install \
         the project.\n\n\
         ```bash\n\
         uv venv --python 3.12\n\
         uv pip install -e .\n\
         ```\n\n\
         [Install uv](https://docs.astral.sh/uv/getting-started/installation/)\n\n\
         ## Step 3 · Fill in your variables\n\n\
         Create a private .env file in the repository root, then replace \
         YOUR_API_ID and YOUR_API_HASH. SESSION may stay blank for first-run \
         setup. Optional values may stay blank.\n\n\
         ```bash\n\
         umask 077\n\
         touch .env\n\
         chmod 600 .env\n\
         nano .env\n\
         ```\n\n\
         ```dotenv\n\
         API_ID=YOUR_API_ID\n\
         API_HASH=YOUR_API_HASH\n\
         SESSION=\n\
         #-- OPTIONAL\n\
         LOG_CHANNEL=\n\
         BOT_TOKEN=\n\
         MONGO_URI=\n\
         ```\n\n\
         ### 3a · Where to obtain every value\n\n\
         | Variable | Where to obtain it | Open |\n\
         |---|---|---|\n\
         | API_ID | Create an app in Telegram API development tools. | [Telegram apps](https://my.telegram.org/apps) |\n\
         | API_HASH | Use the hash from that same Telegram app; keep it private. | [Telegram apps](https://my.telegram.org/apps) |\n\
         | SESSION | Leave blank for first-run private OTP/QR setup; it saves your session locally. | [Private setup guide]({docs}#web-setup-server) |\n\
         | LOG_CHANNEL | Optional numeric ID of a group/channel you control; allow the bot to post. | [Telegram IDs](https://core.telegram.org/api/peers#peer-id) |\n\
         | BOT_TOKEN | Optional assistant token: create your own bot with BotFather. | [BotFather](https://t.me/BotFather) |\n\
         | MONGO_URI | Optional connection URI from your own MongoDB deployment; includes credentials. | [MongoDB Atlas](https://www.mongodb.com/docs/atlas/driver-connection/) |\n\n\
         ## Step 4 · Start Ultroid\n\n\
         Choose the launch method matching your install. Only run one \
         instance per session.\n\n\
         ### 4a · Run as a Python module\n\n\
         ```bash\n\
         source .venv/bin/activate\n\
         python3 -m pyUltroid\n\
         ```\n\n\
         ### 4b · Run with uv\n\n\
         ```bash\n\
         uv run --no-sync python3 -m pyUltroid\n\
         ```\n\n\
         ## Step 5 · Complete private first-run login\n\n\
         If SESSION is blank, open the private setup URL printed in your \
         terminal. Use the setup token shown there and complete phone OTP \
         or QR login. The setup saves the session in your local .env. Keep \
         the URL/token private; do not expose an unprotected setup server \
         to the Internet.\n\n\
         Wait for startup to finish, then test your configured commands in \
         Saved Messages. If switching to a background launch, stop this \
         foreground instance first with Ctrl+C.\n\n\
         [First-run setup]({docs}#web-setup-server)\n\n\
         ## Step 6 · Keep it running\n\n\
         Choose ONE method after successful setup and after stopping the \
         foreground worker. These keep it alive after SSH logout, not after \
         a server reboot or crash; use a properly configured systemd service \
         for automatic restart. Protect log files because they can contain \
         private setup details.\n\n\
         ### 6a · nohup\n\n\
         ```bash\n\
         umask 077\n\
         nohup .venv/bin/python -m pyUltroid > ultroid.log 2>&1 < /dev/null &\n\
         ```\n\n\
         ### 6b · screen\n\n\
         Requires screen installed. Detach with Ctrl+A, then D; reattach \
         with the command below. Do not launch a second worker when \
         reconnecting.\n\n\
         ```bash\n\
         screen -S ultroid .venv/bin/python -m pyUltroid\n\
         ```\n\n\
         ```bash\n\
         screen -r ultroid\n\
         ```\n\n\
         ## Step 7 · Use custom addons\n\n\
         Only load code you trust: addons execute with your account's \
         permissions. Telegram command examples use the default dot prefix; \
         replace it if you changed yours.\n\n\
         ### 7a · Enable addons\n\n\
         Add this setting to .env, then restart your existing worker once. \
         An existing database ADDONS setting can override the environment.\n\n\
         ```dotenv\n\
         ADDONS=true\n\
         ```\n\n\
         [Default addon repository](https://github.com/Mahesh953-hub/MYADDONS)\n\n\
         ### 7b · Use a channel\n\n\
         Join your trusted channel with the user account and upload addon \
         .py files there. Run this in Telegram, replacing the example \
         channel. PLUGIN_CHANNEL is a database list, not an .env variable. \
         Channel loading requires a user account; it is unavailable in \
         bot-only mode.\n\n\
         ```text\n\
         .setdb PLUGIN_CHANNEL [\"@your_addon_channel\"]\n\
         .restart\n\
         ```\n\n\
         ### 7c · Use a repository\n\n\
         Before the first addon clone, set ADDONS_URL in Telegram to a \
         trusted repository. Replace OWNER/ADDONS_REPO. This is a database \
         key, not an .env variable. If addons/ already exists, this setting \
         does not replace it: preserve local work and migrate deliberately. \
         For private access configure GIT_TOKEN securely in your environment, \
         never in a Telegram command or repository URL.\n\n\
         ```text\n\
         .setdb ADDONS_URL https://github.com/OWNER/ADDONS_REPO.git\n\
         .restart\n\
         ```\n\n\
         ---\n\n\
         **NEVER share your SESSION string or MongoDB MONGO_URI. They can \
         grant access to your account and database. Also keep API_HASH, \
         BOT_TOKEN, OTPs and .env private; never commit them to Git.**\n\n\
         _page 1 of 1 • total 7_\n"
    )
}

/// Plain-text fallback for the deploy guide (Python `_show_deploy_guide`'s
/// `plain_fallback`, same sections, no collapsibles/tables/buttons).
pub fn deploy_guide_plain(invoker_name: &str) -> String {
    let md = deploy_guide_markdown(invoker_name);
    let mut out = String::from(
        "Plain-text guide: collapsibles, tables and in-section copy/URL buttons \
         are unavailable in this inline destination. The commands below are examples; \
         choose one install/start method, replace placeholders, and do not start duplicate workers. \
         If SESSION is blank, complete OTP/QR login using the private setup URL/token in your terminal.\n\n",
    );
    // Keep the commands readable: strip markdown decoration conservatively.
    for line in md.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with('#') || line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(
        "\nFor full explanations and value sources, open Repository → docs/SETUP.md. \
         Never share SESSION, MONGO_URI, API_HASH, BOT_TOKEN, OTPs or .env.",
    );
    out
}

// ── help catalog / plugin card ────────────────────────────────────────────

/// Display name for a help-registry key: `plugins.afk` -> `Afk` (Python
/// `_plugin_label`).
pub fn plugin_label(key: &str) -> String {
    let stem = key
        .rsplit('.')
        .next()
        .unwrap_or(key)
        .replace('_', " ")
        .trim()
        .to_string();
    let mut out = String::new();
    let mut cap = true;
    for c in stem.chars() {
        if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
        if c == ' ' {
            cap = true;
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

/// Page count for a catalog of `total` plugins at `rows`×`cols` per page
/// (Python `_catalog_help_blocks`: `HELP_ROWS`/`HELP_COLUMNS`, defaults 5/2).
pub fn catalog_page_count(total: usize, rows: usize, cols: usize) -> usize {
    let page_size = rows.max(1) * cols.max(1);
    ((total + page_size - 1) / page_size).max(1)
}

/// Clamp a requested page index (Python: `max(0, min(index, page_count-1))`).
pub fn clamp_page(index: i64, page_count: usize) -> usize {
    index.clamp(0, page_count.saturating_sub(1) as i64).max(0) as usize
}

/// The catalog overview HTML for a library page (Python
/// `_catalog_help_blocks`): title header + overview table + footer. The
/// plugin-button grid itself is built by the binary from the HELP
/// registry via [`menu_grid_pages`].
pub fn catalog_overview_html(
    library_title: &str,
    total_handlers: usize,
    plugin_count: usize,
    page: usize,
    page_count: usize,
) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<h2>{title}</h2><p>Browse this library to open a plugin's handlers, \
         documentation, and controls.</p><h2>Overview</h2>\
         <table><tr><th>Section name</th><th>Total handlers</th><th>Contains</th></tr>\
         <tr><td>{title}</td><td>{handlers}</td><td>{count} plugin(s)</td></tr></table>\
         <hr><p><i>{footer}</i></p>",
        title = esc(library_title),
        handlers = total_handlers,
        count = plugin_count,
        footer = page_footer(page + 1, page_count, plugin_count),
    )
}

/// The plugin reference card HTML (Python `_plugin_card_markup`): header +
/// overview table + Commands / PMF collapsibles + Docs blockquote + footer.
/// `commands` are the rendered command lines (deduped by the caller),
/// `pmf_guide` is the PMF guide text when the plugin has PMF entries.
pub fn plugin_card_html(
    file: &str,
    category: &str,
    commands: &[String],
    pmf_guide: Option<&str>,
    docs_html: &str,
    total_handlers: usize,
) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let label = plugin_label(file);
    let mut contains = format!("{} command(s)", commands.len());
    if pmf_guide.is_some() {
        contains += "; PMF control(s)";
    }
    let mut out = format!(
        "<h2>{label}</h2><p>Browse this plugin's command handlers, documentation, \
         and controls.</p><h2>Overview</h2>\
         <table><tr><th>Section name</th><th>Total handlers</th><th>Contains</th></tr>\
         <tr><td>{category}</td><td>{total}</td><td>{contains}</td></tr></table>",
        label = esc(&label),
        category = esc(category),
        total = total_handlers,
        contains = esc(&contains),
    );
    if !commands.is_empty() {
        out.push_str(&format!(
            "<blockquote expandable><b>Commands</b>\n{}</blockquote>",
            esc(&commands.join("\n"))
        ));
    }
    if let Some(guide) = pmf_guide {
        out.push_str(&format!(
            "<blockquote expandable><b>PMF guide</b>\n{}</blockquote>",
            esc(guide)
        ));
    }
    let docs = if docs_html.is_empty() {
        "No documentation is registered.".to_string()
    } else {
        docs_html.to_string()
    };
    out.push_str(&format!(
        "<hr><blockquote expandable><b>Docs</b>\n{docs}</blockquote>\
         <hr><p><i>{footer}</i></p>",
        footer = page_footer(1, 1, total_handlers),
    ));
    out
}

/// Arrange items into rows of `columns`, then chunk rows into pages of
/// `rows` rows each (Python `build_menu_grid` + `fns.split_list` in
/// `page_num`). Returns the pages; the binary picks `clamp_page(index, …)`.
pub fn menu_grid_pages<T: Clone>(items: &[T], cols: usize, rows: usize) -> Vec<Vec<Vec<T>>> {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let mut grid: Vec<Vec<T>> = Vec::new();
    let mut row: Vec<T> = Vec::new();
    for it in items {
        row.push(it.clone());
        if row.len() >= cols {
            grid.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        grid.push(row);
    }
    grid.chunks(rows).map(|c| c.to_vec()).collect()
}

// ── callback data ─────────────────────────────────────────────────────────

/// Callback data for the help-menu page (`assistant:inline:help_menu:<key>:<count>`).
pub fn help_menu_data(key: &str, count: i64) -> String {
    cb(&["assistant", "inline", "help_menu", key, &count.to_string()])
}

/// Callback data for a plugin page (`assistant:inline:plugin:<key>:<file>:<index>`).
pub fn plugin_data(key: &str, file: &str, index: &str) -> String {
    cb(&["assistant", "inline", "plugin", key, file, index])
}

/// Callback data for the help main menu (`assistant:inline:main`).
pub fn main_menu_data() -> String {
    cb(&["assistant", "inline", "main"])
}

/// Callback data to send a plugin's help to chat
/// (`assistant:inline:send_plugin:<key>:<file>:<index>`).
pub fn send_plugin_data(key: &str, file: &str, index: &str) -> String {
    cb(&["assistant", "inline", "send_plugin", key, file, index])
}

/// Parse a help-menu callback's captured tail (`<key>:<count>`, Python
/// `help_func`'s `data_match.group(1)` split).
pub fn parse_help_menu_tail(tail: &str) -> Option<(String, i64)> {
    let (key, count) = tail.split_once(':')?;
    let count: i64 = if count.is_empty() {
        0
    } else {
        count.parse().ok()?
    };
    Some((key.to_string(), count))
}

/// Parse a plugin callback's captured tail (`<key>:<file>:<index>`, Python
/// `uptd_plugin`).
pub fn parse_plugin_tail(tail: &str) -> Option<(String, String, Option<String>)> {
    let mut parts = tail.splitn(3, ':');
    let key = parts.next()?.to_string();
    let file = parts.next()?.to_string();
    let index = parts
        .next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    Some((key, file, index))
}

// ── manifest placeholder buttons ──────────────────────────────────────────

/// The manifest placeholder state for a plugin (Python
/// `core.manifest.get_placeholder`: `text` / `url` / `is_error` / `preset`).
#[derive(Debug, Clone, Default)]
pub struct PlaceholderState {
    pub text: Option<String>,
    pub url: Option<String>,
    pub is_error: bool,
    pub preset: Option<String>,
}

/// Build the placeholder button (Python `_placeholder_button`).
/// Precedence: error (`{^}` + truncated text) > URL button >
/// copy-text button > idle "ready" button.
pub fn placeholder_button(ph: &PlaceholderState, ready_label: &str) -> InlineButton {
    let text = ph.text.clone().or_else(|| ph.url.clone());
    if ph.is_error {
        let label = match &text {
            Some(t) => format!("{{^}} {}", t.chars().take(40).collect::<String>()),
            None => "{^}".to_string(),
        };
        return InlineButton::callback(label, "plph_info");
    }
    if let Some(url) = &ph.url {
        let label = text
            .as_ref()
            .map(|t| t.chars().take(60).collect::<String>())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Open".to_string());
        return InlineButton::url(label, url.clone());
    }
    if let Some(t) = &ph.text {
        return InlineButton::callback(t.chars().take(40).collect::<String>(), "plph_copy");
    }
    InlineButton::callback(ready_label, "plph_info")
}

/// Replace the button whose callback data matches `data_str` with
/// `replacement` (Python `_rebuild_buttons`); `None` when there are no
/// buttons to rebuild.
pub fn rebuild_buttons(
    buttons: &[Vec<InlineButton>],
    data_str: &str,
    replacement: InlineButton,
) -> Option<Vec<Vec<InlineButton>>> {
    if buttons.is_empty() {
        return None;
    }
    let mut out: Vec<Vec<InlineButton>> = Vec::new();
    for row in buttons {
        let mut new_row: Vec<InlineButton> = Vec::new();
        for btn in row {
            if btn.data.as_deref() == Some(data_str) {
                new_row.push(replacement.clone());
            } else {
                new_row.push(btn.clone());
            }
        }
        out.push(new_row);
    }
    Some(out)
}

// ── STUFF store (`something()`) ───────────────────────────────────────────

/// One stored `something()` payload (Python `STUFF[n]`).
#[derive(Debug, Clone)]
pub struct StuffItem {
    pub msg: String,
    pub media: Option<String>,
    pub buttons: Vec<Vec<InlineButton>>,
}

static STUFF: LazyLock<Mutex<HashMap<usize, StuffItem>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Store a `something()` payload (Python `something()`); returns its number.
pub fn stuff_put(msg: String, media: Option<String>, buttons: Vec<Vec<InlineButton>>) -> usize {
    let mut map = STUFF.lock().unwrap_or_else(|e| e.into_inner());
    let num = map.len() + 1;
    map.insert(
        num,
        StuffItem {
            msg,
            media,
            buttons,
        },
    );
    num
}

/// Read a stored payload (Python `ibuild`'s `STUFF.get(n)`).
pub fn stuff_get(n: usize) -> Option<StuffItem> {
    STUFF.lock().ok()?.get(&n).cloned()
}

/// How a stored item should be answered as an inline result (Python
/// `ibuild`'s media-type detection).
#[derive(Debug, Clone, PartialEq)]
pub enum StuffMediaKind {
    Photo,
    Gif,
    /// A Telegram file id / bot file id (the binary resolves it to a
    /// document result; falls back to [`StuffMediaKind::Article`]).
    Document,
    Article,
}

#[derive(Debug, Clone)]
pub struct StuffResult {
    pub title: String,
    pub text: String,
    pub kind: StuffMediaKind,
    /// Media reference: URL for Photo/Gif, file id for Document.
    pub media: Option<String>,
    pub mime: Option<String>,
    pub buttons: Vec<Vec<InlineButton>>,
}

/// Build the inline answer description for a stored item (Python `ibuild`).
/// `text` defaults to `"Hey!"` when there is neither media nor text.
pub fn stuff_result(n: usize) -> Option<StuffResult> {
    let item = stuff_get(n)?;
    let mut txt = item.msg.clone();
    let mut kind = StuffMediaKind::Article;
    let mut mime = None;
    let mut media = None;
    if txt.is_empty() && item.media.is_none() {
        txt = "Hey!".to_string();
    }
    if let Some(pic) = &item.media {
        let ext = pic.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "img" | "jpg" | "png" => {
                kind = StuffMediaKind::Photo;
                mime = Some("image/jpg".to_string());
                media = Some(pic.clone());
            }
            "mp4" | "mkv" | "gif" => {
                kind = StuffMediaKind::Gif;
                mime = Some("video/mp4".to_string());
                media = Some(pic.clone());
            }
            _ => {
                // Bot file id path: the binary tries `resolve_bot_file_id`;
                // on failure it answers the article branch instead.
                kind = StuffMediaKind::Document;
                media = Some(pic.clone());
            }
        }
    }
    Some(StuffResult {
        title: "Ultroid Op".to_string(),
        text: txt,
        kind,
        media,
        mime,
        buttons: item.buttons,
    })
}

// ── ping / uptime answers ─────────────────────────────────────────────────

/// `Ping: <µs> microseconds` (Python `pkng` callback — measures a
/// zero-length span; the caller passes the measured delta).
pub fn ping_answer(elapsed_micros: u128) -> String {
    format!("Ping: {elapsed_micros} microseconds")
}

/// `Uptime: <formatted>` (Python `upp` callback).
pub fn uptime_answer(uptime_ms: u64) -> String {
    format!("Uptime: {}", time_formatter(uptime_ms))
}
