//! All 102 Ultroid plugins, one module per Python plugin file.
//!
//! Port of `plugins/*.py` (source HEAD 5d09531). Every module exposes
//! `pub fn register(registry: &mut Registry)`; the single entry point
//! [`register`] wires every plugin onto the shared [`Registry`] — the
//! binary crate calls it once with the same registry the assistant uses.
//!
//! Plugin modules land in waves; this file's `pub mod` list and the
//! `register` calls grow with them (coordinator-owned).

use ultroid_core::Registry;

// Wave B (small media tools) — 15 modules, all ported.
pub mod audiotools;
pub mod beautify;
pub mod calculator;
pub mod compressor;
pub mod converter;
pub mod emojis;
pub mod fontgen;
pub mod giftools;
pub mod glitch;
pub mod imaging;
pub mod mediatools;
pub mod qrcode;
pub mod resize;
pub mod specialtools;
pub mod wallpaper;

/// Register every ported plugin on the shared registry.
///
/// Modules are added here as their waves land. A module is listed only
/// once its real port exists — never a placeholder.
pub fn register(registry: &mut Registry) {
    audiotools::register(registry);
    beautify::register(registry);
    calculator::register(registry);
    compressor::register(registry);
    converter::register(registry);
    emojis::register(registry);
    fontgen::register(registry);
    giftools::register(registry);
    glitch::register(registry);
    imaging::register(registry);
    mediatools::register(registry);
    qrcode::register(registry);
    resize::register(registry);
    specialtools::register(registry);
    wallpaper::register(registry);
}

// ── Shared registration helpers ──────────────────────────────────────────

/// Helpers every plugin module uses: the `CommandHandler` adapter for
/// plain async handler fns, `def`/`reg_add` for one-line registration,
/// and the raw udB key helpers. Mirrors the assistant crate's `cbs.rs`
/// adapter so plugins and assistant register identically.
pub mod util {
    use async_trait::async_trait;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use ultroid_core::{CommandDef, CommandHandler, Ctx, Registry, Result};

    /// `CommandHandler` adapter for plain async handler functions, so each
    /// module registers `|ctx| Box::pin(handler(ctx))` instead of a
    /// one-off struct per command.
    pub struct Handler<F>(pub F);

    #[async_trait]
    impl<F> CommandHandler for Handler<F>
    where
        F: for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
            + Send
            + Sync,
    {
        async fn handle(&self, ctx: &Ctx) -> Result<()> {
            (self.0)(ctx).await
        }
    }

    /// Register one command: `reg_add(registry, def, |ctx| Box::pin(f(ctx)))`.
    pub fn reg_add<F>(registry: &mut Registry, def: CommandDef, f: F)
    where
        F: for<'a> Fn(&'a Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        registry.add(def, Arc::new(Handler(f)));
    }

    /// A command definition with the common defaults (dual `.`/`/`
    /// handler prefixes come from `Registry::add`).
    pub fn def(name: &'static str, pattern: &'static str, help: &'static str) -> CommandDef {
        CommandDef {
            name,
            pattern,
            help,
            ..Default::default()
        }
    }

    /// Read a raw udB key (Python `udB.get_key`).
    pub async fn db_get(ctx: &Ctx, key: &str) -> Result<Option<String>> {
        ctx.db.get(key).await
    }

    /// Write a raw udB key (Python `udB.set_key`).
    pub async fn db_set(ctx: &Ctx, key: &str, value: &str) -> Result<()> {
        ctx.db.set(key, value).await
    }

    /// Delete a udB key (Python `udB.del_key`).
    pub async fn db_del(ctx: &Ctx, key: &str) -> Result<()> {
        ctx.db.del(key).await
    }
}
