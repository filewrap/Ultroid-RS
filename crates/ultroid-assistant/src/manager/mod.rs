//! Group-manager plugin namespace.
//!
//! Port of `assistant/manager/__init__.py`, whose only job is to be the
//! namespace the manager modules import through (`from . import *`) and
//! to re-export the assistant runtime names. In Rust that is the module
//! tree below plus the single [`register`] entry point that registers
//! every manager command on the shared [`Registry`].
//!
//! Note: a sibling worker owns the `assistant/*.py` root files in this
//! same `src/` directory — this module only declares the `manager/`
//! subtree.

pub mod admins;
pub mod afk;
pub mod help;
pub mod misc;
pub mod on_adds;
pub mod stickermanager;

use ultroid_core::router::Registry;

/// Register every manager command. Modules whose behaviour cannot run on
/// the current core surface (`admins`, `on_adds`, `stickermanager`) keep a
/// no-op `register` that documents the exact blocker — see each module's
/// docs.
pub fn register(registry: &mut Registry) {
    help::register(registry);
    afk::register(registry);
    misc::register(registry);
    admins::register(registry);
    on_adds::register(registry);
    stickermanager::register(registry);
}
