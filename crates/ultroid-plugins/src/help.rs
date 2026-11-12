//! `.help` — port of `plugins/_help.py`.
//!
//! REGISTRY-HELP GAP (verified against
//! `crates/ultroid-core/src/router.rs`): the `.help` command reads its data
//! from the live `HELP` / `LIST` maps (plugin -> docstring entries, plugin
//! -> command patterns) and from the Plugin Manifest Framework. In Rust:
//! * `ultroid_core::router::Registry` exposes only `command_names()` and
//!   `command_count()` — no pattern/help listing, no per-plugin grouping
//!   (Official / Addons / VCBot), no docstring entries, no manifest entries.
//! * `Ctx` carries no registry handle at all, so a handler cannot even
//!   call `command_names()`.
//! * Inline delivery (`inline_query(asst, "ultd")` + click) needs an
//!   assistant inline-query op that does not exist on `Responder`.
//! * The paged help-menu callbacks (`help_menu`, `inlone`, `ownr`, `close`)
//!   are assistant-bot callback handlers; there is no callback surface in
//!   the plugin crate.
//!
//! The command is registered with its exact Python pattern and the pure
//! helpers (`_plugin_title`, `_md_escape`, `_page_footer`) are ported
//! faithfully. Both delivery paths return an honest error naming the gap
//! instead of inventing help text. When the coordinator extends `Registry`
//! with a help listing (and `Ctx` with a handle to it), the lookup branch
//! below is where the `_plugin_help_markup` rendering plugs in.

use crate::util::{def, reg_add};
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

/// `.help [plugin|command]` — port of `_help.py::_help`.
async fn help(ctx: &Ctx) -> Result<()> {
    let plug = ctx.args.trim();
    if !plug.is_empty() {
        // Plugin/command lookup against HELP/LIST — unavailable (gap above).
        return Err(Error::Other(
            "`.help <plugin>` needs a Registry help listing (plugin -> \
             docstring entries + command patterns, grouped Official/Addons/VCBot); \
             ultroid-core::router::Registry exposes only command_names()/command_count() \
             and Ctx carries no registry handle (Python: _help.py:_help)"
                .to_string(),
        ));
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
