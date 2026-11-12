//! `.help` — port of `plugins/_help.py`.
//!
//! `.help <plugin|command>` renders from the live
//! `Registry::help_listing()` (plugin -> commands, in registration
//! order) — the Rust equivalent of Python's HELP/LIST lookup. Python
//! groups Official/Addons/VCBot and shows docstring entries + PMF
//! controls; here the grouping is by plugin module and the "docs" are
//! the commands' own help texts, because the Rust port carries no
//! per-plugin docstrings and no manifest entries. Rendered with the
//! Telegram-safe HTML subset (`<b> <i> <code>`) via `ctx.edit_html`
//! (port of `_edit_help` + `_plugin_help_markup`, minus the rich-only
//! table/expandable blocks, which plain HTML cannot carry).
//!
//! Bare `.help` is delivered as an inline result from the assistant bot
//! (Python: `client.inline_query(asst, "ultd")` + click result 0) —
//! that needs an assistant inline-query op on `Responder` (see the
//! TODO(responder-op) in `help` below). The paged help-menu callbacks
//! (`help_menu`, `plugin`, `ownr`, `close`) are assistant-bot callback
//! handlers (see `plugins::inline` + the assistant crate).

use crate::util::{def, reg_add};
use std::sync::LazyLock;
use ultroid_core::fns::escape_html;
use ultroid_core::router::CommandDef;
use ultroid_core::{Ctx, Error, Registry, Result};

/// Port of `_help.py::_plugin_title`.
pub fn plugin_title(name: &str) -> String {
    let titled: String = name
        .replace('_', " ")
        .trim()
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if titled.is_empty() {
        name.to_string()
    } else {
        titled
    }
}

const MD_ESCAPE: &str = "_*[]()~`>-|";

/// Port of `_help.py::_md_escape`.
pub fn md_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if MD_ESCAPE.contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Port of `_help.py::_page_footer`.
pub fn page_footer(page: u64, pages: u64, total: u64) -> String {
    format!("page {page} of {pages} • total {total}")
}

// ── command_help.py ports ────────────────────────────────────────────────
// Pure ports of `pyUltroid/fns/command_help.py`: `command_aliases`,
// `command_syntax`, `command_matches`. They render registered command
// regexes as readable command syntax for the help roster.

fn group_end(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut escaped = false;
    for index in start..chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// `command_aliases` — literal command-name variants in a handler regex.
pub fn command_aliases(pattern: &str) -> Vec<String> {
    let mut source = pattern.trim().to_string();
    if let Some(rest) = source.strip_prefix('^') {
        source = rest.to_string();
    }
    if let Some(rest) = source.strip_suffix('$') {
        source = rest.to_string();
    }
    if let Some(rest) = source.strip_prefix('/') {
        source = rest.to_string();
    }
    let chars: Vec<char> = source.chars().collect();
    let mut variants: Vec<String> = vec![String::new()];
    let mut index = 0usize;
    let mut has_command = false;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            break;
        }
        if ch == '\\' {
            if index + 1 >= chars.len() {
                break;
            }
            let escaped = chars[index + 1];
            if escaped.is_alphanumeric() {
                break;
            }
            variants = variants.iter().map(|v| format!("{v}{escaped}")).collect();
            has_command = true;
            index += 2;
            continue;
        }
        if ch == '(' {
            let Some(end) = group_end(&chars, index) else {
                break;
            };
            let content: String = chars[index + 1..end].iter().collect();
            // `_SIMPLE_ALTERNATIVES` fullmatch: optional `?:` prefix, then
            // `A|B|...` with each part [A-Za-z0-9_+-]+, at least one `|`.
            let bare = content.strip_prefix("?:").unwrap_or(&content);
            let options: Vec<&str> = bare.split('|').collect();
            let simple = options.len() >= 2
                && options.iter().all(|o| {
                    !o.is_empty()
                        && o.chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
                });
            if !simple {
                break;
            }
            variants = variants
                .iter()
                .flat_map(|prefix| options.iter().map(move |o| format!("{prefix}{o}")))
                .collect();
            has_command = true;
            index = end + 1;
            continue;
        }
        if "?$*+[{".contains(ch) {
            break;
        }
        if ".^$|]}".contains(ch) {
            break;
        }
        variants = variants.iter().map(|v| format!("{v}{ch}")).collect();
        has_command = true;
        index += 1;
    }
    if !has_command {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    variants
        .into_iter()
        .filter(|v| seen.insert(v.clone()))
        .take(24)
        .collect()
}

static ARGUMENT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\s|\\[sSdDwW]|\.\*|\.\+|\\d|\\w|\[\^?.*?\]|\(\?[:P=!]")
        .expect("static regex")
});

/// `command_syntax` — render a registered pattern as readable syntax.
pub fn command_syntax(pattern: &str, prefix: &str) -> String {
    let aliases = command_aliases(pattern);
    let has_arguments = ARGUMENT_RE.is_match(pattern);
    let command_prefix = if ["", " ", "NO_HNDLR"].contains(&prefix) {
        ""
    } else {
        prefix
    };
    let names: Vec<String> = if aliases.is_empty() {
        vec!["command".to_string()]
    } else {
        aliases
    };
    let label = names
        .iter()
        .map(|n| format!("{command_prefix}{n}"))
        .collect::<Vec<_>>()
        .join(" / ");
    if has_arguments {
        format!("{label} [arguments]")
    } else {
        label
    }
}

/// `command_matches` — whether `query` names one of the literal
/// commands in `pattern`.
pub fn command_matches(pattern: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    command_aliases(pattern)
        .iter()
        .any(|a| a.to_lowercase() == q)
}

/// Port of `_help.py::_plugin_help_markup` (content; the rich-only
/// table/expandable blocks are flattened to the Telegram-safe HTML
/// subset because this is delivered with `edit_html`).
pub fn plugin_help_markup(name: &str, defs: &[&CommandDef]) -> String {
    let title = plugin_title(name);
    // `dict.fromkeys(command_syntax(...))` — dedup, order kept.
    let mut seen = std::collections::HashSet::new();
    let mut commands: Vec<String> = Vec::new();
    for d in defs {
        let syn = command_syntax(d.pattern, ".");
        if seen.insert(syn.clone()) {
            commands.push(syn);
        }
    }
    let total = commands.len();
    let mut out = format!(
        "<b>{title}</b>\nBrowse this plugin's command handlers, documentation, and controls.\n\n"
    );
    out.push_str(&format!(
        "<b>Overview</b>\nSection: Plugins — {total} handler(s), {total} command(s)\n\n"
    ));
    if commands.is_empty() {
        out.push_str("No commands are registered for this plugin.\n");
    } else {
        out.push_str("<b>Commands</b>\n");
        for syn in &commands {
            out.push_str(&format!("<code>{}</code>\n", escape_html(syn)));
        }
        // Per-command help lines (the "Docs" block: no docstrings exist
        // in the port, so the commands' help texts are the docs).
        let docs: Vec<String> = defs
            .iter()
            .filter(|d| !d.help.is_empty())
            .map(|d| {
                format!(
                    "• {} — {}",
                    command_syntax(d.pattern, "."),
                    escape_html(d.help)
                )
            })
            .collect();
        if !docs.is_empty() {
            out.push_str("\n<b>Docs</b>\n");
            out.push_str(&docs.join("\n"));
            out.push('\n');
        }
    }
    out.push_str(&format!("\n<i>{}</i>", page_footer(1, 1, total as u64)));
    out
}

fn did_you_mean(listing: &[(&str, Vec<&CommandDef>)], plug: &str) -> Vec<String> {
    let needle = plug.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for (plugin, defs) in listing {
        if plugin.to_lowercase().contains(&needle) {
            push(plugin.to_string());
        }
        for d in defs {
            for a in command_aliases(d.pattern) {
                if a.to_lowercase().contains(&needle) {
                    push(a);
                }
            }
        }
    }
    out.truncate(5);
    out
}

/// `.help [plugin|command]` — port of `_help.py::_help`.
async fn help(ctx: &Ctx) -> Result<()> {
    let plug = ctx.args.trim();
    if !plug.is_empty() {
        let listing = ctx.registry.help_listing();
        // 1. Plugin name (casefold).
        if let Some((name, defs)) = listing.iter().find(|(p, _)| p.eq_ignore_ascii_case(plug)) {
            let html = plugin_help_markup(name, defs);
            return ctx.edit_html(ctx.message_id, &html).await;
        }
        // 2. A command the query names (`command_matches` per pattern).
        for (name, defs) in &listing {
            if defs.iter().any(|d| command_matches(d.pattern, plug)) {
                let html = plugin_help_markup(name, defs);
                return ctx.edit_html(ctx.message_id, &html).await;
            }
        }
        // 3. No match — "Did you mean" suggestions (Python: difflib
        // close matches; here: substring matches over plugin/command
        // names, first 5).
        let suggestions = did_you_mean(&listing, plug);
        let mut out = format!(
            "<b>Help could not be opened.</b> No plugin or command named <code>{}</code>.",
            escape_html(plug)
        );
        if !suggestions.is_empty() {
            out.push_str("\n\nDid you mean: ");
            out.push_str(
                &suggestions
                    .iter()
                    .map(|s| format!("<code>{}</code>", escape_html(s)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        return ctx.edit_html(ctx.message_id, &out).await;
    }
    // Bare `.help` is delivered as an inline result from the assistant bot.
    // TODO(responder-op): assistant-bot inline query "ultd" + click result
    Err(Error::Other(
        "bare `.help` needs an assistant-bot inline query ('ultd') + click; \
         requires a binary Responder op (Python: _help.py:_help)"
            .to_string(),
    ))
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "help",
            "help( (.*)|$)",
            "Browse plugin commands, inline tools, and account settings.",
        ),
        |ctx| Box::pin(help(ctx)),
    );
}
