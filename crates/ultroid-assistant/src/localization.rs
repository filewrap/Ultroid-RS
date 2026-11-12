//! Language picker for the assistant bot.
//!
//! Port of `assistant/localization.py`.
//!
//! * `assistant:lang:menu` — owner-only language menu: one row per
//!   shipped locale, native name + `[code]`, the current language
//!   marked; Back returns to `assistant:start:mainmenu`.
//! * `assistant:lang:set:<code>` — owner-only; resolves the token
//!   against the shipped locales (exact, then case-insensitive),
//!   writes udB `language` (deleted again for `en`, exactly as
//!   Python does), and shows the confirmation page.
//!
//! Language metadata comes from `ultroid_core::locales` (the same
//! generated tables the `Strings` store reads), so the menu can never
//! offer a locale the strings system does not ship.

use ultroid_core::Ctx;
use ultroid_core::error::Result;
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};

use crate::cbs;
use crate::rich_pages::{edit_callback_page, rich_page};

/// Exact-match callback data for the language menu.
pub fn lang_menu_data() -> String {
    cbs::cb(&["assistant", "lang", "menu"]).unwrap_or_default()
}

/// Callback-data prefix for language selection.
pub const LANG_SET_PREFIX: [&str; 3] = ["assistant", "lang", "set"];

/// Resolve a callback token against the shipped locale keys: exact
/// match first, then case-insensitive (Python `_language_key`).
pub fn language_key(raw: &str) -> Option<String> {
    let key = raw.trim();
    if ultroid_core::locales::locale(key).is_some() {
        return Some(key.to_string());
    }
    ultroid_core::locales::LANGUAGES
        .iter()
        .find(|c| c.eq_ignore_ascii_case(key))
        .map(|c| c.to_string())
}

/// One menu entry per language: `(label, callback_data, is_current)`.
pub fn language_entries(current: &str) -> Vec<(String, String, bool)> {
    ultroid_core::locales::LANGUAGES
        .iter()
        .map(|code| {
            let natively = ultroid_core::locales::locale(code)
                .map(|(_, m)| m.natively)
                .unwrap_or(code);
            let data = cbs::cb(&["assistant", "lang", "set", code]).unwrap_or_default();
            (
                format!("{natively} [{}]", code.to_ascii_lowercase()),
                data,
                code.eq_ignore_ascii_case(current),
            )
        })
        .collect()
}

/// Keyboard for the language menu (two entries per row, Back last).
pub fn lang_keyboard(current: &str) -> Vec<Vec<InlineButton>> {
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let mut row: Vec<InlineButton> = Vec::new();
    for (label, data, is_current) in language_entries(current) {
        let mut btn = InlineButton::data(label, data.into_bytes());
        if is_current {
            btn = btn.styled(ButtonStyle::Success);
        }
        row.push(btn);
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let main_menu = cbs::cb(&["assistant", "start", "mainmenu"]).unwrap_or_default();
    rows.push(vec![InlineButton::data("Back", main_menu.into_bytes())]);
    rows
}

/// The language menu page (rich table, one row per language).
pub fn lang_page(current: &str) -> Result<RichDoc> {
    let rows: Vec<Vec<String>> = language_entries(current)
        .into_iter()
        .map(|(label, _, is_current)| {
            vec![
                label,
                if is_current {
                    "Currently selected".to_string()
                } else {
                    "Choose this language".to_string()
                },
            ]
        })
        .collect();
    let total = rows.len();
    rich_page(
        "Language",
        &rows,
        Some("Choose the language used by the assistant."),
        &["Language", "Status"],
        1,
        1,
        Some(total),
        None,
    )
}

/// Current language: udB `language`, default `en` (Python reads
/// `ULTConfig.lang`, seeded from the same key; `Ctx.lang` carries the
/// same value for the running update).
pub async fn current_lang_async(ctx: &Ctx) -> String {
    match ctx.db.get("language").await {
        Ok(Some(v)) if !v.trim().is_empty() => v.trim().to_string(),
        _ => ctx.lang.clone(),
    }
}

/// Dispatch callback data owned by this module. Returns `true` when
/// the data belonged here. The binary crate feeds callback-query data
/// in; `Ctx` itself carries no callback-query fields.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if data == lang_menu_data() {
        setlang(ctx).await?;
        return Ok(true);
    }
    if let Some(tail) = cbs::cb_tail(data, &LANG_SET_PREFIX)? {
        set_language(ctx, &tail).await?;
        return Ok(true);
    }
    Ok(false)
}

async fn setlang(ctx: &Ctx) -> Result<()> {
    let current = current_lang_async(ctx).await;
    let page = lang_page(&current)?;
    edit_callback_page(ctx, &page, lang_keyboard(&current)).await
}

async fn set_language(ctx: &Ctx, raw: &str) -> Result<()> {
    let Some(lang) = language_key(raw) else {
        // Python answers an alert; `Ctx` has no alert channel, so the
        // failure is delivered as a reply (see `crate::cbs` docs).
        ctx.reply("That language is no longer available.").await?;
        return Ok(());
    };
    // Python: `udB.del_key("language")` for `en`, else set it.
    if lang.eq_ignore_ascii_case("en") {
        ctx.db.del("language").await?;
    } else {
        ctx.db.set("language", &lang).await?;
    }
    let name = ultroid_core::locales::locale(&lang)
        .map(|(_, m)| m.natively.to_string())
        .unwrap_or_else(|| lang.clone());
    let page = rich_page(
        "Language updated",
        &[vec!["Language".to_string(), format!("{name} [{lang}]")]],
        Some("The assistant language has been updated."),
        &["Setting", "Current value"],
        1,
        1,
        Some(1),
        None,
    )?;
    let back = cbs::back_button(&lang_menu_data(), "Back");
    edit_callback_page(ctx, &page, vec![vec![back]]).await
}
