//! Assistant package facade.
//!
//! Port of `assistant/__init__.py`. The Python module is the facade
//! its submodules star-import from: owner identity, the `setit`
//! database writer, the navigation-label cleaner and the back-button
//! builder, plus the `AST_PLUGINS` loader table.
//!
//! In Rust there is no star import: siblings import the helpers they
//! need from `crate::cbs` / this module explicitly. The loader table
//! is the command `Registry` itself (the coordinator owns `lib.rs`,
//! which declares the modules and calls each `register`); the names
//! below are the assistant plugin modules the loader knows, matching
//! the Python `AST_PLUGINS` keys (`pmbot`, plus the manager/
//! sub-package modules, which live outside this 18-file port).

use ultroid_core::Ctx;
use ultroid_core::error::Result;
use ultroid_core::tg::InlineButton;

use crate::cbs;

/// Owner identity, from the live config (Python: `OWNER_NAME` /
/// `OWNER_ID` read off `ultroid_bot` at import time).
pub fn owner_id(ctx: &Ctx) -> i64 {
    ctx.config.owner_id
}

/// The assistant plugin modules the loader knows, in load order
/// (Python `AST_PLUGINS` is populated by `AsyncPluginLoader` for
/// `assistant/pmbot.py` and `assistant/manager/*.py`; the Rust
/// `Registry` is the equivalent table).
pub const AST_PLUGIN_MODULES: &[&str] = &[
    "pmbot",
    "manager/_help",
    "manager/_on_adds",
    "manager/admins",
    "manager/afk",
    "manager/misc",
    "manager/stickermanager",
];

/// Port of `assistant/__init__.py::setit`: write one udB key; on a
/// database error, reply `"Something Went Wrong"` (the Python
/// fallback edits that text into the event).
pub async fn setit(ctx: &Ctx, name: &str, value: &str) -> Result<()> {
    match ctx.db.set(name, value).await {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = ctx.reply("Something Went Wrong").await;
            Err(ultroid_core::error::Error::Database(format!(
                "setit failed for key {name}"
            )))
        }
    }
}

/// Re-export of the navigation-label cleaner (Python
/// `assistant/__init__.py::_clean_nav_label`).
pub fn clean_nav_label(label: &str) -> String {
    cbs::clean_nav_label(label)
}

/// Re-export of the back-button builder (Python
/// `assistant/__init__.py::get_back_button`): a blue, glyph-free row
/// targeting `name`, the destination's own callback data.
pub fn get_back_button(name: &str) -> InlineButton {
    cbs::back_button(name, "Back")
}

/// Same, with an explicit label.
pub fn get_back_button_label(name: &str, label: &str) -> InlineButton {
    cbs::back_button(name, label)
}
