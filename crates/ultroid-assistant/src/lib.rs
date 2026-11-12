//! The assistant bot: handlers, games, inline helpers and the manager.
//!
//! Port of `assistant/*.py` (root handlers) plus `assistant/manager/*`,
//! `pyUltroid/_core/assistant_creations.py` (`creations`) and
//! `pyUltroid/_misc/_assistant.py` (`misc_assistant`).
//!
//! Every module exposes `pub fn register(registry: &mut Registry)` when it
//! owns commands; the helper-only modules (`cbs`, `callbackstuffs`,
//! `creations`, `init`, `initial`, `inlinestuff`, `localization`,
//! `misc_assistant`, `rich_pages`) export their helpers instead. The
//! single entry point is [`register`], which registers every assistant
//! command on the shared [`Registry`] — the binary crate calls it once
//! with the same registry the plugins use.

pub mod agent;
pub mod agent_bridge;
pub mod ai_dm;
pub mod callbackstuffs;
pub mod cbs;
pub mod creations;
pub mod games;
pub mod init;
pub mod initial;
pub mod inlinestuff;
pub mod localization;
pub mod ludo_arcade;
pub mod manager;
pub mod matrix_arcade;
pub mod misc_assistant;
pub mod pmbot;
pub mod rich_creations;
pub mod rich_pages;
pub mod rich_showcase;
pub mod start;
pub mod ytdl;

use ultroid_core::Registry;

/// Register every assistant command on the shared registry.
pub fn register(registry: &mut Registry) {
    agent::register(registry);
    agent_bridge::register(registry);
    ai_dm::register(registry);
    games::register(registry);
    ludo_arcade::register(registry);
    matrix_arcade::register(registry);
    pmbot::register(registry);
    rich_creations::register(registry);
    rich_showcase::register(registry);
    start::register(registry);
    ytdl::register(registry);
    manager::register(registry);
}

pub use init::AST_PLUGIN_MODULES;
