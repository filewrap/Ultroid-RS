//! `ultroid-core` — core runtime of Ultroid-RS.
//! Port of `pyUltroid/` (config, DB, dispatcher, rich data, loader, fns).

pub mod akinator;
pub mod business;
pub mod config;
pub mod context;
pub mod db;
pub mod entities;
pub mod error;
pub mod extender;
pub mod fns;
pub mod fmt;
pub mod games;
pub mod imaging;
pub mod listen;
pub mod locales;
pub mod loader;
pub mod modules;
pub mod notices;
pub mod queue;
pub mod rich;
pub mod router;
pub mod startup;
pub mod stars;
pub mod strings;
pub mod tg;
pub mod toggles;
pub mod tracer;
pub mod universal;
pub mod updater;

pub use config::{Config, ULTROID_VERSION, VERSION};
pub use context::{ChatType, ClientKind, Ctx, Responder};
pub use db::Database;
pub use error::{Error, Result};
pub use rich::RichDoc;
pub use router::{CommandDef, CommandHandler, Registry};
pub use strings::Strings;

#[cfg(test)]
mod tests;
