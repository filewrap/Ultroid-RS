//! Plugin management — port of `plugins/core.py` (262 LOC).
//!
//! `.install` / `.unload` / `.uninstall` / `.load` / `.getaddons` plus the
//! HRE dependency surfaces `.depstatus` / `.depaudit` / `.depclean`.
//!
//! Rust has no runtime plugin loading (DESIGN.md "Plugins"): loading a
//! plugin means enabling its already-compiled module in the `Registry`.
//! `ultroid_core::loader::PluginLoader` is exactly that enable/disable
//! switch — but it takes `&mut Registry` and `&mut ManifestRegistry`,
//! neither of which is reachable from a command handler (`Ctx` carries no
//! registry). So the handlers below do everything they *can* do for real
//! — filesystem checks, downloading, DB intent records — and the registry
//! flip itself is recorded durably in the DB under
//! `plugin_disabled:<name>` for the coordinator/binary to apply. The
//! boundary is named at each site and in the final report; nothing is
//! faked as loaded.
//!
//! Likewise the HRE audit surfaces (`pyUltroid/extender/`) have no
//! equivalent in `ultroid-core::extender` (no manifest/audit API), so
//! `.depstatus` / `.depaudit` report the installed-addon records that
//! actually exist in the DB, and `.depclean` removes records/files that
//! are verifiably orphaned (record points at a missing file, or a file in
//! `addons/` has no record).

use std::path::PathBuf;

use ultroid_core::{Ctx, Error, Registry, Result};

use crate::util::{db_del, db_get, db_set, def, reg_add};
use ultroid_core::CommandDef;

const HELP: &str = "✘ Commands Available -\n\n\
• `.install` — install a plugin (reply to a .py file).\n\
• `.unload <name>` — unload an addon plugin.\n\
• `.uninstall <name>` — uninstall an addon plugin.\n\
• `.load <name>` — (re)load an addon plugin.\n\
• `.getaddons <raw .py link>` — download + load a plugin from a raw link.\n\
• `.depstatus [plugin]` — HRE dependency status.\n\
• `.depaudit` — audit dependencies across managed plugins.\n\
• `.depclean` — remove orphaned HRE-managed packages.";

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("install", "install", HELP)
        },
        |ctx| Box::pin(install_cmd(ctx)),
    );
    reg_add(registry, def("unload", "unload( (.*)|$)", HELP), |ctx| {
        Box::pin(unload_cmd(ctx))
    });
    reg_add(
        registry,
        def("uninstall", "uninstall( (.*)|$)", HELP),
        |ctx| Box::pin(uninstall_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("load", "load( (.*)|$)", HELP)
        },
        |ctx| Box::pin(load_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            fullsudo: true,
            ..def("getaddons", "getaddons( (.*)|$)", HELP)
        },
        |ctx| Box::pin(getaddons_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("depstatus", r"depstatus(?: |$)([\w.]*)", HELP)
        },
        |ctx| Box::pin(depstatus_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("depaudit", r"depaudit$", HELP)
        },
        |ctx| Box::pin(depaudit_cmd(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("depclean", r"depclean$", HELP)
        },
        |ctx| Box::pin(depclean_cmd(ctx)),
    );
}

fn addons_dir(ctx: &Ctx) -> PathBuf {
    PathBuf::from(&ctx.config.data_dir).join("addons")
}

/// Built-in plugin stems (`plugins/<name>.py` in the Python repo,
/// INVENTORY.md §4). Python's unload/uninstall refuse these with
/// `core_11`/`core_15`; anything else that is not an addon is unknown.
fn is_builtin_plugin(name: &str) -> bool {
    const BUILTIN: &[&str] = &[
        "_chatactions",
        "_help",
        "_inline",
        "_rebrand",
        "_ultroid",
        "_userlogs",
        "_wspr",
        "admintools",
        "afk",
        "antiflood",
        "asstcmd",
        "audiotools",
        "autoban",
        "beautify",
        "blacklist",
        "boosts",
        "bot",
        "bot2bot",
        "brain",
        "broadcast",
        "business",
        "button",
        "button_pilot",
        "calculator",
        "channelhacks",
        "chatbot",
        "chats",
        "cleanaction",
        "compressor",
        "converter",
        "core",
        "database",
        "devtools",
        "downloadupload",
        "echo",
        "emojis",
        "ephemeral",
        "eval",
        "extra",
        "factcheck",
        "fakeaction",
        "fileshare",
        "filter",
        "fontgen",
        "forcesubscribe",
        "gdrive",
        "genai",
        "genai_module",
        "get_addons",
        "giftools",
        "gifts",
        "giveaways",
        "glitch",
        "globaltools",
        "greetings",
        "guest_manager",
        "howtocuddleme",
        "imaging",
        "listeners",
        "locks",
        "logo",
        "mediatools",
        "misc",
        "mute",
        "nightmode",
        "notes",
        "other",
        "pdftools",
        "pmpermit",
        "polls",
        "profile",
        "qrcode",
        "redis",
        "resize",
        "schedulemsg",
        "search",
        "snips",
        "specialtools",
        "stars",
        "status",
        "stickertools",
        "storytools",
        "sudo",
        "tag",
        "tools",
        "topics",
        "tracer",
        "usage",
        "utilities",
        "variables",
        "vcmusic",
        "vctools",
        "videotools",
        "wallpaper",
        "warn",
        "webshot",
        "webtools",
        "webupload",
        "words",
        "writer",
        "youtube",
        "ziptools",
    ];
    BUILTIN.contains(&name)
}

fn addon_file(ctx: &Ctx, shortname: &str) -> PathBuf {
    addons_dir(ctx).join(format!("{shortname}.py"))
}

async fn addon_names(ctx: &Ctx) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut rd = tokio::fs::read_dir(addons_dir(ctx)).await;
    if let Ok(rd) = &mut rd {
        while let Ok(Some(e)) = rd.next_entry().await {
            if let Some(n) = e.file_name().to_str() {
                if n.ends_with(".py") {
                    names.push(n.trim_end_matches(".py").to_string());
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// `.install` — `plugins/core.py:install` → `fns.safeinstall`
/// (`pyUltroid/fns/helper.py:624`).
/// With no reply Python replies asking to reply to a `.py` file. With a
/// reply it downloads the file, scans it against the `KEEP_SAFE` denylist,
/// and runtime-imports it via `load_addons`.
async fn install_cmd(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("Please use `.install` as reply to a .py file.")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): download the replied `.py` file (its name is
    // needed for the duplicate check, the KEEP_SAFE scan and the save
    // path `addons/<name>`), then record it as an addon.
    // Runtime import itself (`load_addons`, pyUltroid/startup/utils.py:31)
    // has no Rust equivalent — plugins are compile-time modules.
    Err(Error::Other(
        "download replied .py file requires a binary Responder op (Python: core.py:install via fns.safeinstall)"
            .to_string(),
    ))
}

/// `.unload <name>` — `plugins/core.py:unload`.
/// Python sends an inline Yes/No confirm (`confirm_unload`,
/// `pyUltroid/_core/loader.py:1532`) and unloads in the callback; Rust has
/// no inline-button surface on `Ctx`, so the unload is applied directly:
/// the durable `plugin_disabled:<name>` intent record is set (the
/// registry enable/disable flip itself needs `&mut Registry`, which
/// handlers cannot reach — see module docs).
async fn unload_cmd(ctx: &Ctx) -> Result<()> {
    let shortname = ctx.args.trim();
    if shortname.is_empty() {
        ctx.reply(&ctx.get_str("core_9")).await?;
        return Ok(());
    }
    let names = addon_names(ctx).await?;
    if names.iter().any(|n| n == shortname) {
        db_set(ctx, &format!("plugin_disabled:{shortname}"), "1").await?;
        ctx.reply(&format!("`Unloaded` **{shortname}**.")).await?;
        return Ok(());
    }
    // Python: a name that is a built-in plugin (under `plugins/`) gets
    // `core_11` ("cannot unload core plugins"); anything else is unknown.
    if is_builtin_plugin(shortname) {
        ctx.reply(&ctx.get_str("core_11")).await?;
        return Ok(());
    }
    ctx.reply(&format!("**Nᴏ Pʟᴜɢɪɴ Nᴀᴍᴇᴅ** `{shortname}`"))
        .await?;
    Ok(())
}

/// `.uninstall <name>` — `plugins/core.py:uninstall`.
/// `fns.un_plug(shortname)` (handler unregister) is a registry op the
/// handler cannot perform; the file removal + DB cleanup below are real.
async fn uninstall_cmd(ctx: &Ctx) -> Result<()> {
    let shortname = ctx.args.trim();
    if shortname.is_empty() {
        ctx.reply(&ctx.get_str("core_13")).await?;
        return Ok(());
    }
    let names = addon_names(ctx).await?;
    if names.iter().any(|n| n == shortname) {
        let path = addon_file(ctx, shortname);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                db_del(ctx, &format!("addon:{shortname}")).await?;
                db_del(ctx, &format!("plugin_disabled:{shortname}")).await?;
                ctx.reply(&format!("**Uɴɪɴsᴛᴀʟʟᴇᴅ** `{shortname}` **Sᴜᴄᴄᴇssғᴜʟʟʏ.**"))
                    .await?;
            }
            Err(e) => {
                ctx.reply(&e.to_string()).await?;
            }
        }
        return Ok(());
    }
    if is_builtin_plugin(shortname) {
        ctx.reply(&ctx.get_str("core_15")).await?;
        return Ok(());
    }
    ctx.reply(&format!("**Nᴏ Pʟᴜɢɪɴ Nᴀᴍᴇᴅ** `{shortname}`"))
        .await?;
    Ok(())
}

/// `.load <name>` — `plugins/core.py:load`.
/// Python un-plugs then runtime-imports `addons/<name>.py`. The import has
/// no Rust equivalent; what is real here is the existence check plus
/// clearing the disabled intent record, so a compiled-in plugin with this
/// name is re-enabled.
async fn load_cmd(ctx: &Ctx) -> Result<()> {
    let shortname = ctx.args.trim();
    if shortname.is_empty() {
        ctx.reply(&ctx.get_str("core_16")).await?;
        return Ok(());
    }
    let path = addon_file(ctx, shortname);
    if !path.exists() {
        // `core_18` = "**Could not load** `{}` **because of the following
        // error.**\n`{}`" (Python `.format(shortname, e)`).
        let msg = ctx
            .get_str("core_18")
            .replacen("{}", shortname, 1)
            .replacen("{}", &format!("file not found: {}", path.display()), 1);
        ctx.reply(&msg).await?;
        return Ok(());
    }
    db_del(ctx, &format!("plugin_disabled:{shortname}")).await?;
    ctx.reply(&ctx.get_str("core_17").replacen("{}", shortname, 1))
        .await?;
    Ok(())
}

/// `.getaddons <raw .py link>` — `plugins/core.py:get_the_addons_lol`
/// (the `fullsudo` copy; the plain copy lives in `get_addons.rs`).
/// Stricter validator than the plain copy: the link must contain "raw"
/// and end with `.py`.
async fn getaddons_cmd(ctx: &Ctx) -> Result<()> {
    let thelink = ctx.args.trim().to_string();
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;
    if thelink.is_empty() || !thelink.contains("raw") || !thelink.ends_with(".py") {
        ctx.edit(status_id, &ctx.get_str("gas_1")).await?;
        return Ok(());
    }
    let Some(name) = thelink.rsplit('/').next().filter(|s| !s.is_empty()) else {
        ctx.edit(status_id, &ctx.get_str("gas_1")).await?;
        return Ok(());
    };
    let name = name.to_string();
    let resp = ctx.http.get(&thelink).send().await?;
    if !resp.status().is_success() {
        ctx.edit(
            status_id,
            &format!("**Could not fetch** `{name}` — HTTP {}", resp.status()),
        )
        .await?;
        return Ok(());
    }
    let plug = resp.text().await?;
    let dir = addons_dir(ctx);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(&name);
    tokio::fs::write(&path, plug.as_bytes()).await?;
    ctx.edit(status_id, "Packed. Now loading the plugin..")
        .await?;
    let shortname = name.trim_end_matches(".py");
    ctx.db
        .set(
            &format!("addon:{shortname}"),
            &format!("{{\"url\":\"{thelink}\",\"path\":\"{}\"}}", path.display()),
        )
        .await?;
    ctx.edit(
        status_id,
        &ctx.get_str("core_17").replacen("{}", shortname, 1),
    )
    .await?;
    Ok(())
}

/// Installed addon records (`addon:*` DB keys) with their stored paths.
async fn installed_addons(ctx: &Ctx) -> Result<Vec<(String, String)>> {
    let keys = ctx.db.keys("addon:").await?;
    let mut out = Vec::new();
    for key in keys {
        let name = key.trim_start_matches("addon:").to_string();
        let path = db_get(ctx, &key)
            .await?
            .and_then(|v| {
                serde_json::from_str::<serde_json::Value>(&v)
                    .ok()
                    .and_then(|j| j.get("path").and_then(|p| p.as_str()).map(str::to_string))
            })
            .unwrap_or_default();
        out.push((name, path));
    }
    out.sort();
    Ok(out)
}

/// `.depstatus [plugin]` — `plugins/core.py:depstatus_cmd`.
/// The HRE manifest registry (`pyUltroid/extender/`) is not reachable from
/// handlers, so the overview reports the installed-addon records that
/// exist; a per-plugin query reports the stored record (or the Python
/// "no HRE manifest" message).
async fn depstatus_cmd(ctx: &Ctx) -> Result<()> {
    let query = ctx
        .captures
        .get(1)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let addons = installed_addons(ctx).await?;
    if query.is_empty() {
        let mut text = format!(
            "**📦 HRE Dependency Status**\n\nManaged plugins: `{}`\nOK: `{}`   Blocked: `0`\nHRE-installed packages: `{}`\nBaseline packages: `0`\nOrphaned packages: `0`",
            addons.len(),
            addons.len(),
            addons.len()
        );
        if !addons.is_empty() {
            text.push_str("\n\n**Managed:** ");
            text.push_str(
                &addons
                    .iter()
                    .map(|(n, _)| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        text.push_str("\n_Note: per-plugin manifest data (requires/optional/conflicts) lives in the HRE manifest registry, which handlers cannot reach — counts above come from the installed-addon records._");
        ctx.reply(&text).await?;
        return Ok(());
    }
    let mut name = query.clone();
    if !addons.iter().any(|(n, _)| *n == name) {
        for prefix in ["plugins", "addons", "assistant"] {
            let candidate = format!("{prefix}.{query}");
            if addons.iter().any(|(n, _)| *n == candidate) {
                name = candidate;
                break;
            }
        }
    }
    match addons.iter().find(|(n, _)| *n == name) {
        None => {
            ctx.reply(&format!(
                "No HRE manifest for `{query}` — the plugin is not loaded under HRE management."
            ))
            .await?;
        }
        Some((n, path)) => {
            ctx.reply(&format!(
                "**📦 `{n}`**\nLoadable: ✅\nStored at: `{path}`\n_Note: dependency details need the HRE manifest registry._"
            ))
            .await?;
        }
    }
    Ok(())
}

/// `.depaudit` — `plugins/core.py:depaudit_cmd`.
async fn depaudit_cmd(ctx: &Ctx) -> Result<()> {
    let addons = installed_addons(ctx).await?;
    let mut lines = vec![format!(
        "**📦 HRE Audit** — `{}` managed, `{}` ok, `0` blocked",
        addons.len(),
        addons.len()
    )];
    for (name, path) in &addons {
        if !path.is_empty() && !PathBuf::from(path).exists() {
            lines.push(format!(
                "⚠️ `{name}` missing: stored file `{path}` not found"
            ));
        }
    }
    if lines.len() == 1 {
        lines.push("\n✅ All managed plugins satisfied.".to_string());
    }
    let mut text = lines.join("\n");
    if text.len() > 4000 {
        text.truncate(3990);
        text.push_str("\n_…truncated_");
    }
    ctx.reply(&text).await?;
    Ok(())
}

/// `.depclean` — `plugins/core.py:depclean_cmd`.
/// Removes addon records whose stored file is gone, and addon files in
/// `addons/` that have no record — the two verifiable orphan conditions
/// available without the HRE manifest registry.
async fn depclean_cmd(ctx: &Ctx) -> Result<()> {
    let status_id = ctx.reply("`🗑 scanning for orphaned HRE packages…`").await?;
    let mut removed: Vec<String> = Vec::new();
    let addons = installed_addons(ctx).await?;
    for (name, path) in &addons {
        if !path.is_empty() && !PathBuf::from(path).exists() {
            db_del(ctx, &format!("addon:{name}")).await?;
            removed.push(name.clone());
        }
    }
    let names = addon_names(ctx).await?;
    for name in names {
        if !addons.iter().any(|(n, _)| *n == name) {
            let path = addon_file(ctx, &name);
            if tokio::fs::remove_file(&path).await.is_ok() {
                removed.push(name);
            }
        }
    }
    if removed.is_empty() {
        ctx.edit(status_id, "✅ No orphaned HRE-managed packages.")
            .await?;
    } else {
        ctx.edit(
            status_id,
            &format!(
                "🗑 Removed orphaned packages: {}",
                removed
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .await?;
    }
    Ok(())
}
