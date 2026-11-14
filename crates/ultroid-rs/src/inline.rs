//! Inline-query + inline-send routing, port of `plugins/_inline.py`
//! and the `alive` inline flow from `plugins/bot.py`.
//!
//! The assistant bot owns inline queries (grammers `Update::InlineQuery`
//! arrives on its update stream); the user client never answers them.
//! All handlers are owner-gated, mirroring Python's `owner=True`.
//!
//! grammers limitations, verified against 0.12.0 source (see
//! `inline_query.rs`): `Article::new` builds an
//! `InputBotInlineMessageText` from the message's text + entities +
//! reply markup — a `rich_message` on the `InputMessage` is dropped
//! ("TODO: also allow other types of messages than text"). Articles
//! therefore go out as HTML text with entities, never as RichMessage.
//! The same applies to `InlineSend::edit_message` (its
//! `edit_inline_message` hardcodes `rich_message: None`). The Hi-button
//! edit uses the raw `messages.EditInlineBotMessage` call instead, but
//! still with plain text: the deploy guide's plain variant
//! (`deploy_guide_plain`) is the ported `blocks_to_plain_text`
//! fallback. Only `Article` exists as a grammers inline-result builder
//! — there is no Photo/Document/Gif result type — so `stf` items with
//! media degrade to an article whose text carries the media URL (or
//! the file id for documents).

use crate::Shared;
use grammers_client::{
    Client,
    message::InputMessage,
    peer::Peer,
    tl,
    update::{Article, CallbackQuery, InlineQuery, InlineSend},
};
use std::sync::Arc;
use std::time::Duration;
use ultroid_core::{
    ClientKind, Ctx, Error, Result, Strings, ULTROID_VERSION, fns::time_formatter,
    tg::InlineButton, updater::Updater,
};
use ultroid_plugins::help::command_syntax;
use ultroid_plugins::inline as pi;
use ultroid_rich::to_reply_markup;

// ── shared helpers ────────────────────────────────────────────────────────

/// (official plugin count, addon plugin count) for the alive cards.
/// Python counts `len(HELP["Official"])` / `len(HELP["Addons"])`; Rust
/// has no dynamic addon category, so addons are always 0.
fn plugin_counts(listing: &[(&'static str, Vec<&ultroid_core::CommandDef>)]) -> (usize, usize) {
    let official = listing
        .iter()
        .filter(|(name, _)| *name != "assistant" && *name != "vcmusic" && *name != "vctools")
        .count();
    (official, 0)
}

/// Handler totals per catalogue section (Official / Voice Chat / all).
fn category_totals(
    listing: &[(&'static str, Vec<&ultroid_core::CommandDef>)],
) -> (usize, usize, usize) {
    let mut official = 0usize;
    let mut vc = 0usize;
    let mut total = 0usize;
    for (name, defs) in listing {
        if *name == "assistant" {
            continue;
        }
        total += defs.len();
        if *name == "vcmusic" || *name == "vctools" {
            vc += defs.len();
        } else {
            official += defs.len();
        }
    }
    (official, vc, total)
}

fn uptime_ms(shared: &Arc<Shared>) -> u64 {
    shared.started.elapsed().as_millis() as u64
}

fn s(strings: &Strings, key: &str) -> String {
    strings.get(key)
}

/// Convert `plugins::inline` plain-data buttons (label + callback-data
/// string / URL string) into core `InlineButton`s for `to_reply_markup`.
fn convert_buttons(rows: &[Vec<pi::InlineButton>]) -> Vec<Vec<InlineButton>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|b| {
                    if let Some(url) = &b.url {
                        InlineButton::url(b.label.clone(), url.clone())
                    } else {
                        InlineButton::data(
                            b.label.clone(),
                            b.data.clone().unwrap_or_default().into_bytes(),
                        )
                    }
                })
                .collect()
        })
        .collect()
}

fn display_name(peer: Option<&Peer>) -> String {
    match peer {
        Some(Peer::User(u)) => u
            .first_name()
            .map(str::to_string)
            .or_else(|| u.username().map(|n| format!("@{n}")))
            .unwrap_or_else(|| "there".to_string()),
        _ => "there".to_string(),
    }
}

// ── inline queries ────────────────────────────────────────────────────────

/// Route one inline query from the assistant bot (Python's
/// `@in_pattern` handlers in `_inline.py` + `bot.py`).
pub async fn handle_inline_query(
    shared: &Arc<Shared>,
    kind: ClientKind,
    query: InlineQuery,
) -> Result<()> {
    // Inline queries only ever arrive on the bot client.
    if kind != ClientKind::AsstBot {
        return Ok(());
    }
    // Owner-only, like Python's `in_pattern(owner=True)`. A non-owner
    // gets no answer at all (the client shows no results).
    let sender = query.sender_id().bot_api_dialog_id().unwrap_or(-1);
    if shared.config.owner_id == 0 || sender != shared.config.owner_id {
        return Ok(());
    }
    let (official, addons) = plugin_counts(&shared.registry.help_listing());
    match pi::parse_inline_query(query.text()) {
        pi::InlineQueryKind::Alive => {
            let html = pi::alive_rich_markup(
                &shared.owner_name,
                uptime_ms(shared),
                official,
                addons,
                None,
            );
            let buttons = convert_buttons(&pi::sup_buttons(
                &s(&shared.strings, "btn_repo"),
                &s(&shared.strings, "btn_support"),
            ));
            let article = Article::new(
                "Ultroid Userbot",
                InputMessage::new()
                    .html(&html)
                    .reply_markup(to_reply_markup(&buttons)),
            )
            .id(pi::INLINE_ALIVE_RESULT_ID)
            .description("Userbot | Telethon");
            query
                .answer(vec![article])
                .private()
                .cache_time(0)
                .switch_pm("Ultroid Portal", "start")
                .send()
                .await
                .map_err(|e| Error::Telegram(e.to_string()))?;
        }
        pi::InlineQueryKind::AliveStatus => {
            let html = alive_status_html(shared).await;
            let buttons = vec![vec![
                InlineButton::url(
                    s(&shared.strings, "bot_3"),
                    "https://github.com/Mahesh953-hub/SarahUB",
                ),
                InlineButton::url(
                    s(&shared.strings, "bot_4"),
                    "https://t.me/UltroidSupportChat",
                ),
            ]];
            // The thumbnail is the ALIVE_PIC URL when it is one.
            let article = article_with_thumb(shared, "Ultroid · Alive", &html, &buttons).await;
            query
                .answer(vec![article])
                .send()
                .await
                .map_err(|e| Error::Telegram(e.to_string()))?;
        }
        pi::InlineQueryKind::Help => {
            let (html, buttons) = help_menu(
                &shared.registry,
                &shared.strings,
                shared.asst_username.as_deref(),
            );
            let article = Article::new(
                "Bot Help",
                InputMessage::new()
                    .html(&html)
                    .reply_markup(to_reply_markup(&buttons)),
            );
            query
                .answer(vec![article])
                .private()
                .cache_time(0)
                .gallery()
                .send()
                .await
                .map_err(|e| Error::Telegram(e.to_string()))?;
        }
        pi::InlineQueryKind::Pasta(id) => {
            let (link, raw) = pi::pasta_links(&id);
            let buttons = vec![vec![
                InlineButton::url("SpaceBin", link),
                InlineButton::url("Raw", raw),
            ]];
            let article = Article::new(
                "Paste",
                InputMessage::new()
                    .text("Pasted to Spacebin.")
                    .reply_markup(to_reply_markup(&buttons)),
            );
            query
                .answer(vec![article])
                .send()
                .await
                .map_err(|e| Error::Telegram(e.to_string()))?;
        }
        pi::InlineQueryKind::Stuff(n) => {
            let Some(res) = pi::stuff_result(n) else {
                // Python's `ibuild` crashes on a missing STUFF id; the
                // Rust port answers nothing instead.
                return Ok(());
            };
            let mut text = res.text.clone();
            match res.kind {
                pi::StuffMediaKind::Photo | pi::StuffMediaKind::Gif => {
                    // No photo/gif result type in grammers: the media
                    // URL rides in the article text.
                    if let Some(media) = &res.media {
                        text.push_str(&format!("\n{media}"));
                    }
                }
                pi::StuffMediaKind::Document => {
                    if let Some(media) = &res.media {
                        text.push_str(&format!("\nFile: {media}"));
                    }
                }
                pi::StuffMediaKind::Article => {}
            }
            let article = Article::new(
                "Ultroid Op",
                crate::markdown::md_input(&text)
                    .reply_markup(to_reply_markup(&convert_buttons(&res.buttons))),
            )
            .description("@TeamUltroid");
            query
                .answer(vec![article])
                .send()
                .await
                .map_err(|e| Error::Telegram(e.to_string()))?;
        }
        pi::InlineQueryKind::Other(_) => {
            // Other plugins own their patterns; none exist in this port.
        }
    }
    Ok(())
}

/// The `@bot alive` status card (Python `bot.py::_alive_inline_markup`).
/// The Python row is omitted — the port has no Python runtime — and
/// the Telethon row names grammers instead.
async fn alive_status_html(shared: &Arc<Shared>) -> String {
    let header = shared
        .db
        .get("ALIVE_TEXT")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| s(&shared.strings, "bot_1"));
    let (official, addons) = plugin_counts(&shared.registry.help_listing());
    let rows: [(&str, String); 6] = [
        ("Version", format!("{ULTROID_VERSION} [Rust]")),
        ("Telethon", "grammers 0.12.0 (layer 233)".to_string()),
        ("Uptime", time_formatter(uptime_ms(shared))),
        ("Branch", shared.branch.clone()),
        ("Plugins", official.to_string()),
        ("Addons", addons.to_string()),
    ];
    let esc = |v: &str| {
        v.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut parts = vec![
        "<h1>Ultroid · Alive</h1>".to_string(),
        format!("<p>{}</p>", esc(&header)),
        "<hr>".to_string(),
        "<table class=\"bordered compact\"><tr><th>Item</th><th>Details</th></tr>".to_string(),
    ];
    for (label, value) in rows {
        parts.push(format!(
            "<tr><td>{}</td><td>{}</td></tr>",
            esc(label),
            esc(&value)
        ));
    }
    parts.push("</table>".to_string());
    parts.join("")
}

/// Build the alive-status article, attaching ALIVE_PIC as a thumbnail
/// when it is an http(s) URL (a local path or file id cannot be a
/// grammers article thumb).
async fn article_with_thumb(
    shared: &Arc<Shared>,
    title: &str,
    html: &str,
    buttons: &[Vec<InlineButton>],
) -> Article {
    // buttons are borrowed; the article needs its own message.
    let mut article = Article::new(
        title,
        InputMessage::new()
            .html(html)
            .reply_markup(to_reply_markup(buttons)),
    )
    .description("Ultroid userbot status");
    if let Ok(Some(pic)) = shared.db.get("ALIVE_PIC").await {
        let pic = pic.trim().to_string();
        if pic.starts_with("http://") || pic.starts_with("https://") {
            article = article.thumb_url(pic);
        }
    }
    article
}

/// The main help menu: text (`_help_blocks`) + buttons
/// (`main_help_menu_buttons`, classic layout).
fn help_menu(
    registry: &ultroid_core::Registry,
    strings: &Strings,
    asst_username: Option<&str>,
) -> (String, Vec<Vec<InlineButton>>) {
    let listing = registry.help_listing();
    let (official_handlers, vc_handlers, total) = category_totals(&listing);
    let total_with_inline = total + 5;
    let html = format!(
        "<h2>Help Menu</h2><p>Browse plugin commands, inline tools, and account settings from here.</p><h2>Overview</h2><table><tr><th>Section name</th><th>Total handlers</th><th>Contains</th></tr><tr><td>Plugins</td><td>{official_handlers}</td><td>Core plugin commands and controls</td></tr><tr><td>Add-ons</td><td>0</td><td>Optional plugin commands and controls</td></tr><tr><td>Voice Chat</td><td>{vc_handlers}</td><td>Voice chat commands and controls</td></tr><tr><td>Inline Plugins</td><td>5</td><td>Searchable inline tools</td></tr></table><hr><p><i>page 1 of 1 • total {total_with_inline}</i></p>"
    );
    (html, main_menu_buttons(strings, asst_username))
}

/// Main help menu keyboard (Python `_main_help_menu`, classic layout;
/// the styled variant needs the button kit, which the port does not
/// carry — classic is always used).
fn main_menu_buttons(strings: &Strings, asst_username: Option<&str>) -> Vec<Vec<InlineButton>> {
    let settings_url = match asst_username {
        Some(u) => format!("https://t.me/{u}?start=set"),
        None => "https://t.me/UltroidSupportChat".to_string(),
    };
    vec![
        vec![
            InlineButton::data(
                s(strings, "help_4"),
                pi::help_menu_data("Official", 0).into_bytes(),
            ),
            InlineButton::data(
                s(strings, "help_5"),
                pi::help_menu_data("Addons", 0).into_bytes(),
            ),
        ],
        vec![
            InlineButton::data(
                s(strings, "help_6"),
                pi::help_menu_data("VCBot", 0).into_bytes(),
            ),
            InlineButton::data(s(strings, "help_7"), b"inlone".to_vec()),
        ],
        vec![
            InlineButton::data(s(strings, "help_8"), b"ownr".to_vec()),
            InlineButton::url(s(strings, "help_9"), settings_url),
        ],
        vec![InlineButton::data(s(strings, "help_10"), b"close".to_vec())],
    ]
}

// ── inline sends (deploy flow) ────────────────────────────────────────────

/// The deploy flow: when the empty-query alive article is chosen,
/// wait for the settle interval and personalise the sent message in
/// place (Python `_handle_deploy_inline_send`).
pub async fn handle_inline_send(shared: &Arc<Shared>, kind: ClientKind, send: InlineSend) {
    // Only the assistant bot's sends carry this flow.
    if kind != ClientKind::AsstBot {
        return;
    }
    if !pi::is_deploy_inline_send(send.text(), send.result_id(), send.message_id().is_some()) {
        return;
    }
    tokio::time::sleep(Duration::from_secs_f64(
        pi::INLINE_DEPLOY_SEND_DELAY_SECONDS,
    ))
    .await;
    let who = match send.sender() {
        Some(u) => u
            .first_name()
            .map(str::to_string)
            .or_else(|| u.username().map(|n| format!("@{n}")))
            .unwrap_or_else(|| "there".to_string()),
        None => "there".to_string(),
    };
    let (official, addons) = plugin_counts(&shared.registry.help_listing());
    let html = pi::alive_rich_markup(
        &shared.owner_name,
        uptime_ms(shared),
        official,
        addons,
        Some(&who),
    );
    let buttons = vec![vec![InlineButton::data("Hi", b"alive_deploy_hi".to_vec())]];
    if let Err(e) = send
        .edit_message(
            InputMessage::new()
                .html(&html)
                .reply_markup(to_reply_markup(&buttons)),
        )
        .await
    {
        tracing::warn!("[inline_deploy] inline-send edit failed: {e}");
    }
}

// ── inline callbacks (help menu system) ───────────────────────────────────

/// Route a callback query that belongs to the inline help system
/// (Python `plugins/_inline.py` callbacks + `plugins/bot.py:alive`).
/// Returns true when the data was owned here. Runs AFTER the assistant
/// `handle_callback` chain in `main.rs` and BEFORE the "🖐️" fallback.
///
/// PMF callbacks (`pltog_` / `plact_` / `plman_` / `plph_copy` /
/// `plph_info`) are NOT owned here: the core `ManifestRegistry` has
/// no toggle/action surface to flip, so they fall through to the
/// unknown-callback answer. `send_plugin` is owned by the assistant
/// crate's `callbackstuffs` handler, which runs before this one.
pub async fn handle_inline_callback(
    ctx: &mut Ctx,
    query: &CallbackQuery,
    client: &Client,
) -> Result<bool> {
    let data = String::from_utf8_lossy(query.data()).into_owned();
    // Owner-only, like Python's `owner=True` on every one of these.
    let sender = query.sender_id().bot_api_dialog_id().unwrap_or(-1);
    if ctx.config.owner_id == 0 || sender != ctx.config.owner_id {
        return Ok(false);
    }

    // -- the Hi button on an inline-sent deploy article -------------------
    // It lives on an INLINE message, so it is answered and edited with
    // the raw `messages.EditInlineBotMessage` call (grammers'
    // `edit_inline_message` is `pub(crate)` and drops rich_message).
    if data == "alive_deploy_hi" {
        if !query.is_from_inline() {
            return Ok(false);
        }
        let msg_id = match &query.raw {
            tl::enums::Update::InlineBotCallbackQuery(u) => u.msg_id.clone(),
            _ => return Ok(false),
        };
        let who = display_name(query.sender());
        let _ = query.answer().send().await;
        let buttons = vec![vec![
            InlineButton::url("Repository", "https://github.com/Mahesh953-hub/SarahUB"),
            InlineButton::url("Support", "https://t.me/UltroidSupportChat"),
        ]];
        client
            .invoke(&tl::functions::messages::EditInlineBotMessage {
                no_webpage: true,
                invert_media: false,
                id: msg_id,
                message: Some(pi::deploy_guide_plain(&who)),
                media: None,
                reply_markup: Some(to_reply_markup(&buttons).raw),
                entities: None,
                rich_message: None,
            })
            .await
            .map_err(|e| Error::Telegram(e.to_string()))?;
        return Ok(true);
    }

    // -- plain answers ------------------------------------------------------
    match data.as_str() {
        "alive" => {
            // Python `plugins/bot.py:alive` — an alert with the status
            // lines (version / Telethon / plugins / addons).
            let (official, addons) = plugin_counts(&ctx.registry.help_listing());
            let text = format!(
                "\nUltroid Userbot\n\n  ◍ Version - {}\n  ◍ Telethon - {}\n  ◍ Plugins - {}\n  ◍ Addons - {}\n",
                ULTROID_VERSION, "grammers 0.12.0 (layer 233)", official, addons
            );
            let _ = query.answer().alert(text).send().await;
            return Ok(true);
        }
        "pkng" => {
            // Python measures two `datetime.now()` calls back to back,
            // so the answer is (almost) always 0 microseconds.
            let start = std::time::Instant::now();
            let micros = start.elapsed().as_micros();
            let _ = query
                .answer()
                .alert(format!("Ping: {micros} microseconds"))
                .cache_time(Duration::from_secs(0))
                .send()
                .await;
            return Ok(true);
        }
        "upp" => {
            let uptime = time_formatter(ctx_uptime_ms(ctx));
            let _ = query
                .answer()
                .alert(format!("Uptime: {uptime}"))
                .cache_time(Duration::from_secs(0))
                .send()
                .await;
            return Ok(true);
        }
        "ownr" => {
            // The settings page reuses the help-menu text with the
            // Ping/Uptime/Stats/Update buttons (Python `setting`).
            let (html, _) = help_menu_ctx(ctx);
            let buttons = vec![
                vec![
                    InlineButton::data("Ping", b"pkng".to_vec()),
                    InlineButton::data("Uptime", b"upp".to_vec()),
                ],
                vec![
                    InlineButton::data("Stats", b"alive".to_vec()),
                    InlineButton::data("Update", b"doupdate".to_vec()),
                ],
                vec![InlineButton::data(
                    s(&ctx.strings, "btn_back"),
                    pi::main_menu_data().into_bytes(),
                )],
            ];
            answer_edit(query, &html, &buttons).await?;
            return Ok(true);
        }
        "inlone" => {
            // Switch-inline buttons for the known inline entry points
            // (Python builds one per `InlinePlugin` key; Rust has no
            // such registry, so the five built-in queries are listed).
            let entries: [(&str, &str); 5] = [
                ("Alive", ""),
                ("Help", "ultd"),
                ("Alive status", "alive"),
                ("Paste", "pasta-"),
                ("Stuff", "stf"),
            ];
            let mut rows: Vec<Vec<InlineButton>> = entries
                .chunks(2)
                .map(|c| {
                    c.iter()
                        .map(|(label, q)| InlineButton::switch_inline(*label, *q))
                        .collect()
                })
                .collect();
            rows.push(vec![InlineButton::data(
                s(&ctx.strings, "btn_back"),
                pi::main_menu_data().into_bytes(),
            )]);
            // Text is unchanged (Python edits buttons only); reuse the
            // current message text when it can be loaded.
            let text = match query.load_message().await {
                Ok(m) => m.text().to_string(),
                Err(_) => help_menu_ctx(ctx).0,
            };
            answer_edit(query, &text, &rows).await?;
            return Ok(true);
        }
        "close" => {
            let buttons = vec![vec![InlineButton::data(
                s(&ctx.strings, "btn_open_again"),
                pi::main_menu_data().into_bytes(),
            )]];
            answer_edit(query, &s(&ctx.strings, "inline_5"), &buttons).await?;
            return Ok(true);
        }
        "assistant:inline:main" => {
            let (html, buttons) = help_menu_ctx(ctx);
            answer_edit(query, &html, &buttons).await?;
            return Ok(true);
        }
        "doupdate" => {
            return do_update(ctx, query).await;
        }
        "upd_apply" => {
            return apply_update(ctx, query).await;
        }
        _ => {}
    }

    // -- catalog pages ------------------------------------------------------
    if let Some(tail) = data.strip_prefix("assistant:inline:help_menu:") {
        let Some((key, count)) = pi::parse_help_menu_tail(tail) else {
            return Ok(false);
        };
        return catalog_page(ctx, query, &key, count).await;
    }
    if let Some(tail) = data.strip_prefix("assistant:inline:plugin:") {
        let Some((key, file, index)) = pi::parse_plugin_tail(tail) else {
            return Ok(false);
        };
        return plugin_card(ctx, query, &key, &file, index.as_deref()).await;
    }
    Ok(false)
}

/// Answer a callback AND edit its message (text + buttons) in one
/// grammers call (`Answer::edit`).
async fn answer_edit(
    query: &CallbackQuery,
    html: &str,
    buttons: &[Vec<InlineButton>],
) -> Result<()> {
    query
        .answer()
        .edit(
            InputMessage::new()
                .html(html)
                .reply_markup(to_reply_markup(buttons)),
        )
        .await
        .map_err(|e| Error::Telegram(e.to_string()))
}

/// Uptime in milliseconds for a `Ctx` (process start captured by
/// `main()` in `crate::PROCESS_START`).
fn ctx_uptime_ms(_ctx: &Ctx) -> u64 {
    crate::PROCESS_START
        .get()
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(0)
}

/// Help menu text + buttons from a `Ctx`.
fn help_menu_ctx(ctx: &Ctx) -> (String, Vec<Vec<InlineButton>>) {
    help_menu(&ctx.registry, &ctx.strings, ctx.asst_username.as_deref())
}

/// Read HELP_ROWS / HELP_COLUMNS from the DB (defaults 5 / 2, like
/// Python's `udB.get_key` fallbacks).
async fn help_grid(ctx: &Ctx) -> (usize, usize) {
    let rows = db_int(ctx, "HELP_ROWS", 5).await;
    let cols = db_int(ctx, "HELP_COLUMNS", 2).await;
    (rows.max(1), cols.max(1))
}

async fn db_int(ctx: &Ctx, key: &str, default: usize) -> usize {
    match ctx.db.get(key).await {
        Ok(Some(v)) => v.trim().parse().unwrap_or(default),
        _ => default,
    }
}

/// One catalogue page (Python `_catalog_help_blocks` + `page_num`).
async fn catalog_page(ctx: &mut Ctx, query: &CallbackQuery, key: &str, index: i64) -> Result<bool> {
    let (title, plugins): (&str, Vec<(&'static str, usize)>) = match key {
        "Official" => {
            let plugins: Vec<(&'static str, usize)> = ctx
                .registry
                .help_listing()
                .into_iter()
                .filter(|(name, _)| {
                    *name != "assistant" && *name != "vcmusic" && *name != "vctools"
                })
                .map(|(name, defs)| (name, defs.len()))
                .collect();
            ("Core plugin library", plugins)
        }
        "Addons" => {
            // Rust loads no addons into this catalogue; Python answers
            // the same alert when HELP["Addons"] is missing.
            let text = s(&ctx.strings, "help_13").replace("{}", ".");
            let _ = query.answer().alert(text).send().await;
            return Ok(true);
        }
        "VCBot" => {
            let plugins: Vec<(&'static str, usize)> = ctx
                .registry
                .help_listing()
                .into_iter()
                .filter(|(name, _)| *name == "vcmusic" || *name == "vctools")
                .map(|(name, defs)| (name, defs.len()))
                .collect();
            if plugins.is_empty() {
                let _ = query
                    .answer()
                    .alert(s(&ctx.strings, "help_12"))
                    .send()
                    .await;
                return Ok(true);
            }
            ("Voice chat library", plugins)
        }
        _ => return Ok(false),
    };
    let handlers: usize = plugins.iter().map(|(_, n)| n).sum();
    let (rows, cols) = help_grid(ctx).await;
    let names: Vec<&'static str> = {
        let mut names: Vec<&'static str> = plugins.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names
    };
    let label_buttons: Vec<InlineButton> = names
        .iter()
        .map(|name| {
            InlineButton::data(
                pi::plugin_label(name),
                pi::plugin_data(key, name, &index.to_string()).into_bytes(),
            )
        })
        .collect();
    let pages = pi::menu_grid_pages(&label_buttons, cols, rows);
    let page_count = pages.len().max(1);
    let page = pi::clamp_page(index, page_count);
    let mut rows_out: Vec<Vec<InlineButton>> = pages.get(page).cloned().unwrap_or_default();
    if page == 0 && page_count == 1 {
        rows_out.push(vec![InlineButton::data(
            s(&ctx.strings, "btn_back_page"),
            pi::main_menu_data().into_bytes(),
        )]);
    } else {
        rows_out.push(vec![
            InlineButton::data(
                s(&ctx.strings, "btn_previous"),
                pi::help_menu_data(key, index - 1).into_bytes(),
            ),
            InlineButton::data(
                s(&ctx.strings, "btn_back_page"),
                pi::main_menu_data().into_bytes(),
            ),
            InlineButton::data(
                s(&ctx.strings, "btn_next"),
                pi::help_menu_data(key, index + 1).into_bytes(),
            ),
        ]);
    }
    let html = pi::catalog_overview_html(title, handlers, plugins.len(), page, page_count);
    answer_edit(query, &html, &rows_out).await?;
    Ok(true)
}

/// One plugin card (Python `_show_plugin` + `_plugin_card_markup`).
async fn plugin_card(
    ctx: &mut Ctx,
    query: &CallbackQuery,
    key: &str,
    file: &str,
    index: Option<&str>,
) -> Result<bool> {
    let idx = index.unwrap_or("0");
    let back_data = pi::help_menu_data(key, idx.parse().unwrap_or(0));
    let back_label = match key {
        "Official" => "Back to Plugins",
        "Addons" => "Back to Add-ons",
        "VCBot" => "Back to Voice Chat",
        _ => "Back",
    };
    let buttons = |extra: Vec<Vec<InlineButton>>| {
        let mut rows = extra;
        rows.push(vec![InlineButton::data(
            back_label,
            back_data.clone().into_bytes(),
        )]);
        rows
    };
    let listing = ctx.registry.help_listing();
    let Some((_, defs)) = listing.iter().find(|(name, _)| *name == file) else {
        // Python's fallback card when the guide cannot be rendered.
        let html = format!(
            "<h2>Plugin guide unavailable</h2><p>The guide could not be rendered in place. Use this command instead:</p><code>.help {key}</code>"
        );
        answer_edit(query, &html, &buttons(vec![])).await?;
        return Ok(true);
    };
    let mut commands: Vec<String> = defs
        .iter()
        .map(|d| command_syntax(d.pattern, "."))
        .collect();
    commands.sort();
    commands.dedup();
    let docs_html = defs
        .iter()
        .map(|d| ultroid_core::fns::escape_html(d.help))
        .collect::<Vec<_>>()
        .join("<br>");
    let category = match key {
        "Official" => "Plugins",
        "Addons" => "Add-ons",
        "VCBot" => "Voice Chat",
        _ => key,
    };
    let html = pi::plugin_card_html(file, category, &commands, None, &docs_html, defs.len());
    let rows = buttons(vec![vec![InlineButton::data(
        s(&ctx.strings, "btn_send_plugin"),
        pi::send_plugin_data(key, file, idx).into_bytes(),
    )]]);
    answer_edit(query, &html, &rows).await?;
    Ok(true)
}

/// The `doupdate` prompt (Python `_inline.py:doupdate`): check for
/// upstream commits and show the changelog with an Update Now button.
/// The >1024-char path truncates the log instead of attaching it as a
/// file — the Responder has no edit-with-file op.
async fn do_update(ctx: &mut Ctx, query: &CallbackQuery) -> Result<bool> {
    let check = tokio::task::spawn_blocking(|| {
        Updater::new(std::env::current_dir().unwrap_or_default()).check()
    })
    .await;
    let check = match check {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let _ = query
                .answer()
                .alert(format!("Update check failed: {e}"))
                .send()
                .await;
            return Ok(true);
        }
        Err(e) => {
            let _ = query
                .answer()
                .alert(format!("Update check failed: {e}"))
                .send()
                .await;
            return Ok(true);
        }
    };
    if !check.has_changes {
        let _ = query
            .answer()
            .alert(s(&ctx.strings, "inline_9"))
            .cache_time(Duration::from_secs(0))
            .send()
            .await;
        return Ok(true);
    }
    let mut log = check.changelog.join("\n");
    if log.len() > 900 {
        log.truncate(900);
        log.push_str("\n… (truncated)");
    }
    let esc = |v: &str| {
        v.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let text = format!(
        "<b>Ultroid updates for [{}]:</b>\n\n{}\n\n{}\n\n{}",
        esc(&check.branch),
        esc(&log),
        esc(&check.impact_summary()),
        esc(&s(&ctx.strings, "inline_8")),
    );
    let buttons = vec![
        vec![InlineButton::data(
            s(&ctx.strings, "btn_update_now"),
            b"upd_apply".to_vec(),
        )],
        vec![InlineButton::data(
            s(&ctx.strings, "btn_back"),
            b"ownr".to_vec(),
        )],
    ];
    answer_edit(query, &text, &buttons).await?;
    Ok(true)
}

/// Apply an update (Python `plugins/bot.py:upd_apply`): answer, show
/// progress, fast-forward, and report. The bot is NOT restarted here —
/// the host process manager owns restarts (Python restarts in 3s via
/// its supervisor); the message says so instead of re-execing from
/// inside a callback.
async fn apply_update(ctx: &mut Ctx, query: &CallbackQuery) -> Result<bool> {
    let _ = query.answer().text("⏳ Applying update…").send().await;
    ctx.responder
        .edit(
            ctx.message_id,
            "⏳ Updating… (the bot stays up while the update applies)",
        )
        .await?;
    let applied = tokio::task::spawn_blocking(|| {
        Updater::new(std::env::current_dir().unwrap_or_default()).update(false)
    })
    .await;
    let text = match applied {
        Ok(Ok(summary)) => {
            if summary == "already up to date" {
                summary
            } else {
                format!("{summary}\n\nCore files changed — restart the bot to run the new code.")
            }
        }
        Ok(Err(e)) => format!("Update failed: {e}"),
        Err(e) => format!("Update failed: {e}"),
    };
    ctx.responder.edit(ctx.message_id, &text).await?;
    Ok(true)
}
