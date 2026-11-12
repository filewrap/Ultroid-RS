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

// All 102 plugins, one module per Python plugin file (waves A-G).
// Every module exposes `pub fn register(registry: &mut Registry)`;
// `inline` registers nothing (the binary owns the inline dispatcher).
pub mod admintools;
pub mod afk;
pub mod antiflood;
pub mod asstcmd;
pub mod audiotools;
pub mod autoban;
pub mod beautify;
pub mod blacklist;
pub mod boosts;
pub mod bot;
pub mod bot2bot;
pub mod brain;
pub mod broadcast;
pub mod business;
pub mod button;
pub mod button_pilot;
pub mod calculator;
pub mod channelhacks;
pub mod chatactions;
pub mod chatbot;
pub mod chats;
pub mod cleanaction;
pub mod compressor;
pub mod converter;
pub mod core;
pub mod database;
pub mod devtools;
pub mod downloadupload;
pub mod echo;
pub mod emojis;
pub mod ephemeral;
pub mod eval;
pub mod extra;
pub mod factcheck;
pub mod fakeaction;
pub mod fileshare;
pub mod filter;
pub mod fontgen;
pub mod forcesubscribe;
pub mod gdrive;
pub mod genai;
pub mod genai_module;
pub mod get_addons;
pub mod giftools;
pub mod gifts;
pub mod giveaways;
pub mod glitch;
pub mod globaltools;
pub mod greetings;
pub mod guest_manager;
pub mod help;
pub mod howtocuddleme;
pub mod imaging;
pub mod inline;
pub mod listeners;
pub mod locks;
pub mod logo;
pub mod mediatools;
pub mod misc;
pub mod mute;
pub mod nightmode;
pub mod notes;
pub mod other;
pub mod pdftools;
pub mod pmpermit;
pub mod polls;
pub mod profile;
pub mod qrcode;
pub mod rebrand;
pub mod redis;
pub mod resize;
pub mod schedulemsg;
pub mod search;
pub mod snips;
pub mod specialtools;
pub mod stars;
pub mod status;
pub mod stickertools;
pub mod storytools;
pub mod sudo;
pub mod tag;
pub mod tools;
pub mod topics;
pub mod tracer;
pub mod ultroid;
pub mod usage;
pub mod userlogs;
pub mod utilities;
pub mod variables;
pub mod vcmusic;
pub mod vctools;
pub mod videotools;
pub mod wallpaper;
pub mod warn;
pub mod webshot;
pub mod webtools;
pub mod webupload;
pub mod words;
pub mod writer;
pub mod wspr;
pub mod youtube;
pub mod ziptools;

/// Register every ported plugin on the shared registry.
///
/// Modules are added here as their waves land. A module is listed only
/// once its real port exists — never a placeholder.
pub fn register(registry: &mut Registry) {
    admintools::register(registry);
    afk::register(registry);
    antiflood::register(registry);
    asstcmd::register(registry);
    audiotools::register(registry);
    autoban::register(registry);
    beautify::register(registry);
    blacklist::register(registry);
    boosts::register(registry);
    bot::register(registry);
    bot2bot::register(registry);
    brain::register(registry);
    broadcast::register(registry);
    business::register(registry);
    button::register(registry);
    button_pilot::register(registry);
    calculator::register(registry);
    channelhacks::register(registry);
    chatactions::register(registry);
    chatbot::register(registry);
    chats::register(registry);
    cleanaction::register(registry);
    compressor::register(registry);
    converter::register(registry);
    core::register(registry);
    database::register(registry);
    devtools::register(registry);
    downloadupload::register(registry);
    echo::register(registry);
    emojis::register(registry);
    ephemeral::register(registry);
    eval::register(registry);
    extra::register(registry);
    factcheck::register(registry);
    fakeaction::register(registry);
    fileshare::register(registry);
    filter::register(registry);
    fontgen::register(registry);
    forcesubscribe::register(registry);
    gdrive::register(registry);
    genai::register(registry);
    genai_module::register(registry);
    get_addons::register(registry);
    giftools::register(registry);
    gifts::register(registry);
    giveaways::register(registry);
    glitch::register(registry);
    globaltools::register(registry);
    greetings::register(registry);
    guest_manager::register(registry);
    help::register(registry);
    howtocuddleme::register(registry);
    imaging::register(registry);
    inline::register(registry);
    listeners::register(registry);
    locks::register(registry);
    logo::register(registry);
    mediatools::register(registry);
    misc::register(registry);
    mute::register(registry);
    nightmode::register(registry);
    notes::register(registry);
    other::register(registry);
    pdftools::register(registry);
    pmpermit::register(registry);
    polls::register(registry);
    profile::register(registry);
    qrcode::register(registry);
    rebrand::register(registry);
    redis::register(registry);
    resize::register(registry);
    schedulemsg::register(registry);
    search::register(registry);
    snips::register(registry);
    specialtools::register(registry);
    stars::register(registry);
    status::register(registry);
    stickertools::register(registry);
    storytools::register(registry);
    sudo::register(registry);
    tag::register(registry);
    tools::register(registry);
    topics::register(registry);
    tracer::register(registry);
    ultroid::register(registry);
    usage::register(registry);
    userlogs::register(registry);
    utilities::register(registry);
    variables::register(registry);
    vcmusic::register(registry);
    vctools::register(registry);
    videotools::register(registry);
    wallpaper::register(registry);
    warn::register(registry);
    webshot::register(registry);
    webtools::register(registry);
    webupload::register(registry);
    words::register(registry);
    writer::register(registry);
    wspr::register(registry);
    youtube::register(registry);
    ziptools::register(registry);
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
