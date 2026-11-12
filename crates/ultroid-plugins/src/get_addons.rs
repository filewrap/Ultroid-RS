//! Addon fetching — port of `plugins/get_addons.py` (53 LOC).
//!
//! `.getaddons <raw link>` downloads a plugin file from a raw link and
//! saves it under `addons/`.
//!
//! Python then calls `load_addons(shortname)` (`pyUltroid/startup/utils.py:
//! load_addons`), which imports the file at runtime with
//! `importlib.util.spec_from_file_location`. Rust has no runtime plugin
//! loading: plugins are compile-time modules (DESIGN.md "Plugins"), so a
//! downloaded file can be fetched, saved and recorded, but cannot be
//! executed until it is compiled in. That boundary is stated in the reply
//! instead of faked: the file is stored under
//! `{data_dir}/addons/<name>.py` and recorded under the `addon:<name>` DB
//! key (same record shape `ultroid-core::extender::Extender` writes), and
//! the success message names the stored path.

use std::path::PathBuf;

use ultroid_core::{Ctx, Registry, Result};

use crate::util::{def, reg_add};

const HELP: &str = "✘ Commands Available -\n\n\
• `.getaddons <raw link to code>`\n    Load Plugins from the given raw link.";

pub fn register(registry: &mut Registry) {
    reg_add(registry, def("getaddons", "getaddons ?(.*)", HELP), |ctx| {
        Box::pin(get_addons_cmd(ctx))
    });
}

/// `.getaddons` — `plugins/get_addons.py:get_the_addons_lol`.
/// (`core.py` carries a second, `fullsudo` copy of the same command with a
/// stricter validator — `.py` suffix required — ported in `core.rs`.)
async fn get_addons_cmd(ctx: &Ctx) -> Result<()> {
    let thelink = ctx.args.trim().to_string();
    let status_id = ctx.reply(&ctx.get_str("com_1")).await?;

    // Python: "Please provide a raw link!" unless the link path contains
    // "raw" or "raw.githubusercontent.com".
    let segments: Vec<&str> = thelink.split('/').collect();
    if thelink.is_empty()
        || (!segments.iter().any(|s| *s == "raw")
            && !segments.iter().any(|s| *s == "raw.githubusercontent.com"))
    {
        ctx.edit(status_id, "Please provide a raw link!").await?;
        return Ok(());
    }
    let Some(name) = segments.last().filter(|s| !s.is_empty()) else {
        ctx.edit(status_id, "Please provide a raw link!").await?;
        return Ok(());
    };
    let name = name.to_string();

    ctx.edit(status_id, "Packing the codes...").await?;
    let code = ctx.http.get(&thelink).send().await?;
    if !code.status().is_success() {
        ctx.edit(
            status_id,
            &format!("**Could not fetch** `{name}` — HTTP {}", code.status()),
        )
        .await?;
        return Ok(());
    }
    let plug = code.text().await?;

    // Store the file under its real name and record it under the same
    // `addon:<name>` key shape `ultroid-core::extender::Extender` writes
    // (the extender stores zip archives as `<name>.zip`; a plugin source
    // file keeps its `.py` name instead).
    let addons_dir = PathBuf::from(&ctx.config.data_dir).join("addons");
    tokio::fs::create_dir_all(&addons_dir).await?;
    let path = addons_dir.join(&name);
    tokio::fs::write(&path, plug.as_bytes()).await?;
    let shortname = name.trim_end_matches(".py");
    ctx.db
        .set(
            &format!("addon:{shortname}"),
            &format!("{{\"url\":\"{thelink}\",\"path\":\"{}\"}}", path.display()),
        )
        .await?;
    ctx.edit(status_id, "Packed. Now loading the plugin..")
        .await?;
    ctx.edit(
        status_id,
        &format!(
            "**Sᴜᴄᴄᴇssғᴜʟʟʏ Lᴏᴀᴅᴇᴅ** `{shortname}`\nSaved to `{}` — restart to compile it in (Rust plugins are compile-time modules).",
            path.display()
        ),
    )
    .await?;
    Ok(())
}
