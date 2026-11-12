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
    registry.set_plugin("admintools");
    admintools::register(registry);
    registry.set_plugin("afk");
    afk::register(registry);
    registry.set_plugin("antiflood");
    antiflood::register(registry);
    registry.set_plugin("asstcmd");
    asstcmd::register(registry);
    registry.set_plugin("audiotools");
    audiotools::register(registry);
    registry.set_plugin("autoban");
    autoban::register(registry);
    registry.set_plugin("beautify");
    beautify::register(registry);
    registry.set_plugin("blacklist");
    blacklist::register(registry);
    registry.set_plugin("boosts");
    boosts::register(registry);
    registry.set_plugin("bot");
    bot::register(registry);
    registry.set_plugin("bot2bot");
    bot2bot::register(registry);
    registry.set_plugin("brain");
    brain::register(registry);
    registry.set_plugin("broadcast");
    broadcast::register(registry);
    registry.set_plugin("business");
    business::register(registry);
    registry.set_plugin("button");
    button::register(registry);
    registry.set_plugin("button_pilot");
    button_pilot::register(registry);
    registry.set_plugin("calculator");
    calculator::register(registry);
    registry.set_plugin("channelhacks");
    channelhacks::register(registry);
    registry.set_plugin("chatactions");
    chatactions::register(registry);
    registry.set_plugin("chatbot");
    chatbot::register(registry);
    registry.set_plugin("chats");
    chats::register(registry);
    registry.set_plugin("cleanaction");
    cleanaction::register(registry);
    registry.set_plugin("compressor");
    compressor::register(registry);
    registry.set_plugin("converter");
    converter::register(registry);
    registry.set_plugin("core");
    core::register(registry);
    registry.set_plugin("database");
    database::register(registry);
    registry.set_plugin("devtools");
    devtools::register(registry);
    registry.set_plugin("downloadupload");
    downloadupload::register(registry);
    registry.set_plugin("echo");
    echo::register(registry);
    registry.set_plugin("emojis");
    emojis::register(registry);
    registry.set_plugin("ephemeral");
    ephemeral::register(registry);
    registry.set_plugin("eval");
    eval::register(registry);
    registry.set_plugin("extra");
    extra::register(registry);
    registry.set_plugin("factcheck");
    factcheck::register(registry);
    registry.set_plugin("fakeaction");
    fakeaction::register(registry);
    registry.set_plugin("fileshare");
    fileshare::register(registry);
    registry.set_plugin("filter");
    filter::register(registry);
    registry.set_plugin("fontgen");
    fontgen::register(registry);
    registry.set_plugin("forcesubscribe");
    forcesubscribe::register(registry);
    registry.set_plugin("gdrive");
    gdrive::register(registry);
    registry.set_plugin("genai");
    genai::register(registry);
    registry.set_plugin("genai_module");
    genai_module::register(registry);
    registry.set_plugin("get_addons");
    get_addons::register(registry);
    registry.set_plugin("giftools");
    giftools::register(registry);
    registry.set_plugin("gifts");
    gifts::register(registry);
    registry.set_plugin("giveaways");
    giveaways::register(registry);
    registry.set_plugin("glitch");
    glitch::register(registry);
    registry.set_plugin("globaltools");
    globaltools::register(registry);
    registry.set_plugin("greetings");
    greetings::register(registry);
    registry.set_plugin("guest_manager");
    guest_manager::register(registry);
    registry.set_plugin("help");
    help::register(registry);
    registry.set_plugin("howtocuddleme");
    howtocuddleme::register(registry);
    registry.set_plugin("imaging");
    imaging::register(registry);
    registry.set_plugin("inline");
    inline::register(registry);
    registry.set_plugin("listeners");
    listeners::register(registry);
    registry.set_plugin("locks");
    locks::register(registry);
    registry.set_plugin("logo");
    logo::register(registry);
    registry.set_plugin("mediatools");
    mediatools::register(registry);
    registry.set_plugin("misc");
    misc::register(registry);
    registry.set_plugin("mute");
    mute::register(registry);
    registry.set_plugin("nightmode");
    nightmode::register(registry);
    registry.set_plugin("notes");
    notes::register(registry);
    registry.set_plugin("other");
    other::register(registry);
    registry.set_plugin("pdftools");
    pdftools::register(registry);
    registry.set_plugin("pmpermit");
    pmpermit::register(registry);
    registry.set_plugin("polls");
    polls::register(registry);
    registry.set_plugin("profile");
    profile::register(registry);
    registry.set_plugin("qrcode");
    qrcode::register(registry);
    registry.set_plugin("rebrand");
    rebrand::register(registry);
    registry.set_plugin("redis");
    redis::register(registry);
    registry.set_plugin("resize");
    resize::register(registry);
    registry.set_plugin("schedulemsg");
    schedulemsg::register(registry);
    registry.set_plugin("search");
    search::register(registry);
    registry.set_plugin("snips");
    snips::register(registry);
    registry.set_plugin("specialtools");
    specialtools::register(registry);
    registry.set_plugin("stars");
    stars::register(registry);
    registry.set_plugin("status");
    status::register(registry);
    registry.set_plugin("stickertools");
    stickertools::register(registry);
    registry.set_plugin("storytools");
    storytools::register(registry);
    registry.set_plugin("sudo");
    sudo::register(registry);
    registry.set_plugin("tag");
    tag::register(registry);
    registry.set_plugin("tools");
    tools::register(registry);
    registry.set_plugin("topics");
    topics::register(registry);
    registry.set_plugin("tracer");
    tracer::register(registry);
    registry.set_plugin("ultroid");
    ultroid::register(registry);
    registry.set_plugin("usage");
    usage::register(registry);
    registry.set_plugin("userlogs");
    userlogs::register(registry);
    registry.set_plugin("utilities");
    utilities::register(registry);
    registry.set_plugin("variables");
    variables::register(registry);
    registry.set_plugin("vcmusic");
    vcmusic::register(registry);
    registry.set_plugin("vctools");
    vctools::register(registry);
    registry.set_plugin("videotools");
    videotools::register(registry);
    registry.set_plugin("wallpaper");
    wallpaper::register(registry);
    registry.set_plugin("warn");
    warn::register(registry);
    registry.set_plugin("webshot");
    webshot::register(registry);
    registry.set_plugin("webtools");
    webtools::register(registry);
    registry.set_plugin("webupload");
    webupload::register(registry);
    registry.set_plugin("words");
    words::register(registry);
    registry.set_plugin("writer");
    writer::register(registry);
    registry.set_plugin("wspr");
    wspr::register(registry);
    registry.set_plugin("youtube");
    youtube::register(registry);
    registry.set_plugin("ziptools");
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
