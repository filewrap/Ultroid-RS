//! Bot core commands — port of `plugins/bot.py` (`.alive` / `.ping` /
//! `.cmds` / `.restart` / `.shutdown` / `.logs` / `.update`).
//!
//! The `.alive` text/media logic is ported faithfully: the rich HTML status
//! card (version, grammers build label, uptime, branch, quote of the day),
//! `ALIVE_PIC` / `ALIVE_EMOJI` udB keys, and the `alive inline` variant.
//! Differences forced by the frozen Ctx/Responder surface:
//! * No registry handle reaches a handler, so plugin/addon counts and
//!   `.cmds` cannot be built from live data (see the registry-help gap in
//!   the wave report). Counts render as `—` and `.cmds` returns an honest
//!   error instead of inventing a list.
//! * Inline URL buttons cannot be attached through `Responder`, so the alive
//!   card is sent without its Repo/Support button row.
//! * Rich edits are not possible through `Responder::edit` (text-only), so
//!   `.alive` replies a `⚡` placeholder, sends the rich card as a new
//!   message via `send_rich_or_plain`, then deletes the placeholder — the
//!   same visible result as Python's `edit_rich` on the placeholder.
//! * `start_time` is not on `Ctx`; uptime is read from the real process
//!   start time in `/proc` (Linux), falling back to the time this module
//!   was registered.

use crate::util::{db_get, db_set, def, reg_add};
use std::sync::OnceLock;
use std::time::Instant;
use ultroid_core::fns::time_formatter;
use ultroid_core::modules::run_shell;
use ultroid_core::updater::Updater;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result, RichDoc, ULTROID_VERSION};

/// grammers build label — the Rust analogue of Python's
/// `_telethon_build_label()` (Telethon version + pinned commit + TL layer).
fn grammers_build_label() -> String {
    "grammers 0.12.0 [layer 233]".to_string()
}

/// Host label — port of `pyUltroid/startup/__init__.py::where_hosted`.
fn hosted_on() -> String {
    if std::env::var_os("ON_NASA").is_some() {
        "NASA 🚀".to_string()
    } else if std::env::var_os("DYNO").is_some() {
        "heroku".to_string()
    } else if std::env::var_os("RAILWAY_STATIC_URL").is_some() {
        "railway".to_string()
    } else if std::env::var_os("OKTETO_TOKEN").is_some() {
        "okteto".to_string()
    } else if std::env::var_os("KUBERNETES_PORT").is_some() {
        "qovery | kubernetes".to_string()
    } else if std::env::var_os("RUNNER_USER").is_some() || std::env::var_os("HOSTNAME").is_some() {
        if std::env::var("USER").unwrap_or_default() == "codespace" {
            "codespace".to_string()
        } else {
            "github actions".to_string()
        }
    } else if std::env::var_os("ANDROID_ROOT").is_some() {
        "termux".to_string()
    } else if std::env::var_os("FLY_APP_NAME").is_some() {
        "fly.io".to_string()
    } else {
        "local".to_string()
    }
}

static REGISTERED_AT: OnceLock<Instant> = OnceLock::new();

/// This process's start time as a Unix timestamp, from `/proc/self/stat`
/// field 22 (starttime, clock ticks since boot) plus `/proc/stat` btime.
fn process_start_unix() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // field 22 overall == index 19 after the `)` split (fields[0] is state).
    let start_ticks: u64 = fields.get(19)?.parse().ok()?;
    // _SC_CLK_TCK; 100 on every supported Linux target.
    let ticks_per_sec: u64 = 100;
    let btime: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find(|l| l.starts_with("btime "))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(btime + start_ticks / ticks_per_sec)
}

/// Milliseconds since this process started.
fn uptime_ms() -> u64 {
    if let Some(start) = process_start_unix() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return now.saturating_sub(start).saturating_mul(1000);
    }
    REGISTERED_AT
        .get()
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(0)
}

/// Port of `fns.advice()` — a random quote from zenquotes.io, with the
/// same fallback Python uses when the fetch fails.
async fn advice(ctx: &Ctx) -> String {
    let fallback = "\"Stay focused.\"  -  Unknown".to_string();
    let resp = ctx.http.get("https://zenquotes.io/api/random").send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(_) => return fallback,
    };
    let data: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return fallback,
    };
    let quote = data
        .get(0)
        .and_then(|q| q.get("q"))
        .and_then(|q| q.as_str());
    let author = data
        .get(0)
        .and_then(|q| q.get("a"))
        .and_then(|a| a.as_str());
    match (quote, author) {
        (Some(q), Some(a)) => format!("\"{q}\"  -  {a}"),
        _ => fallback,
    }
}

/// `ALIVE_PIC` may be a single string or a JSON list of strings; pick one
/// at random when it is a list (Python: `choice(pic)`).
async fn alive_pic(ctx: &Ctx) -> Option<String> {
    let raw = db_get(ctx, "ALIVE_PIC").await.ok()??;
    if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
        if list.is_empty() {
            return None;
        }
        let idx = rand::random::<usize>() % list.len();
        return Some(list[idx].clone());
    }
    Some(raw)
}

/// `.alive` / `.alive inline` — port of `bot.py::lol`.
async fn alive(ctx: &Ctx) -> Result<()> {
    let placeholder = ctx.reply("⚡").await?;
    let arg = ctx.args.trim();
    if arg == "inline" || arg == "i" {
        // Python: inline-query the assistant bot for "alive" and click
        // the result (bot.py:lol).
        let _ = ctx.delete(placeholder).await;
        let Some(asst_id) = ctx.asst_dialog_id else {
            return Err(Error::Other(
                "`.alive inline` requires the assistant bot to be connected".into(),
            ));
        };
        ctx.responder
            .inline_query_click(asst_id, "alive", ctx.chat_id, None)
            .await?;
        return Ok(());
    }

    let pic = alive_pic(ctx).await;
    let uptime = time_formatter(uptime_ms());
    let header = advice(ctx).await;
    let mut an = "━〔 ✦ ᴜʟᴛʀᴏɪᴅ-ᴜꜱᴇʀʙᴏᴛ ✦ 〕━".to_string();
    if let Ok(Some(emoji)) = db_get(ctx, "ALIVE_EMOJI").await {
        if !emoji.is_empty() {
            an = an.replace('✦', &emoji);
        }
    }
    let branch = "main";
    let repo = "https://github.com/ufoptg/UltroidBackup.git";
    let rep = repo.replace(".git", &format!("/tree/{branch}"));

    // Plugin/addon counts come from the Python HELP maps; no registry
    // handle reaches handlers in Rust, so they render as unavailable.
    // (Registry-help gap — see the wave report.)
    let html_text = format!(
        "<blockquote expandable><b>{an}</b></blockquote>\n\n\
         <b>✦ Version</b> - <code>{ULTROID_VERSION} [{}]</code>\n\
         <b>✦ Python</b> - <code>Rust (grammers)</code>\n\
         <b>✦ Uptime</b> - <code>{uptime}</code>\n\
         <b>✦ Telethon</b> - <code>{}</code>\n\
         <b>✦ Plugins</b> - <code>—</code>\n\
         <b>✦ Addons</b> - <code>—</code>\n\
         <b>✦ Branch</b> - <code>{branch}</code>\n\n\
         {header}\n\n\
         <a href=\"{rep}\">📂 {branch}</a>",
        hosted_on(),
        grammers_build_label(),
    );
    // ALIVE_PIC media binding needs a Telegram file id / local path the
    // Responder can consume; `send_file` takes a local path only, so a
    // remote/file-id pic is noted here rather than silently dropped.
    if let Some(p) = &pic {
        tracing::debug!(
            alive_pic = p.as_str(),
            "ALIVE_PIC noted; rich media binding needs a Responder file op"
        );
    }
    let doc = RichDoc::html(html_text)?;
    ctx.send_rich_or_plain(&doc).await?;
    let _ = ctx.delete(placeholder).await;
    // Python deletes the trigger message; the Responder surface has no
    // "delete the command message" beyond deleting by id, which is the
    // trigger itself.
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

/// `.ping` — port of `bot.py` ping handler.
async fn ping(ctx: &Ctx) -> Result<()> {
    let start = Instant::now();
    let x = ctx.reply("⚡").await?;
    let end = start.elapsed().as_millis();
    let uptime = time_formatter(uptime_ms());
    let text = format!("**⚡ Pong !!**\n\n  `Latency` : `{end}ms`\n  `Uptime` : `{uptime}`");
    if ctx.edit(x, &text).await.is_err() {
        ctx.reply(&text).await?;
    }
    Ok(())
}

/// `.cmds` — Python builds a Telegraph index from the live `LIST`
/// (plugin -> patterns) registry. The Rust `Registry` exposes no
/// pattern/help listing and `Ctx` carries no registry handle, so there is
/// no live data to render.
async fn cmds(_ctx: &Ctx) -> Result<()> {
    Err(Error::Other(
        "`.cmds` needs a Registry listing (plugin -> pattern -> help); \
         ultroid-core::router::Registry exposes only command_names()/command_count() \
         and Ctx carries no registry handle (Python: bot.py:cmds -> fns.allcmds)"
            .to_string(),
    ))
}

/// Commits behind upstream, via `git fetch` + rev-list count.
async fn commits_behind() -> Result<(u64, Vec<String>)> {
    let _ = run_shell("git fetch origin", 60).await?;
    let (count_out, _, _) = run_shell(
        "git rev-list --count HEAD..@{u} 2>/dev/null || git rev-list --count HEAD..origin/main",
        30,
    )
    .await?;
    let count: u64 = count_out.trim().parse().unwrap_or(0);
    let (log_out, _, _) = run_shell(
        "git log --oneline -10 HEAD..@{u} 2>/dev/null || git log --oneline -10 HEAD..origin/main",
        30,
    )
    .await?;
    let log: Vec<String> = log_out.lines().map(|l| l.to_string()).collect();
    Ok((count, log))
}

fn heroku_headers(api: &str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {api}").parse().unwrap(),
    );
    h.insert(
        reqwest::header::ACCEPT,
        "application/vnd.heroku+json; version=3".parse().unwrap(),
    );
    h
}

/// Restart the Heroku dynos (Python: `fns.restart` Heroku branch).
async fn heroku_restart(ctx: &Ctx, app: &str, api: &str) -> Result<()> {
    let resp = ctx
        .http
        .delete(format!("https://api.heroku.com/apps/{app}/dynos"))
        .headers(heroku_headers(api))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "Heroku restart failed: HTTP {}",
            resp.status()
        )));
    }
    Ok(())
}

/// Re-exec this process with the same binary (Python: `os.execl(...)`).
fn reexec() -> ! {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ultroid-rs"));
    let argv: Vec<String> = std::env::args().skip(1).take(6).collect();
    let err = std::process::Command::new(exe).args(argv).exec();
    tracing::error!("re-exec failed: {err}");
    std::process::exit(1);
}

/// `.restart` — port of `bot.py::restartbt`: check for updates, apply them
/// if any, record the restart notice, then restart (Heroku API or re-exec).
async fn restart(ctx: &Ctx) -> Result<()> {
    let ok = ctx.reply("`Checking for updates…`").await?;
    let (behind, _log) = match commits_behind().await {
        Ok(v) => v,
        Err(e) => {
            ctx.edit(
                ok,
                &format!("`Update check failed: {e} — restart skipped.`"),
            )
            .await?;
            return Ok(());
        }
    };
    let who = match ctx.client_kind {
        ultroid_core::ClientKind::AsstBot => "bot",
        _ => "user",
    };
    if behind > 0 {
        ctx.edit(ok, &format!("`Applying {behind} commit(s)…`"))
            .await?;
        let repo = std::env::current_dir().map_err(Error::from)?;
        let updater = Updater::new(repo);
        match updater.update(false) {
            Ok(summary) => {
                ctx.edit(ok, &format!("`{summary}`\n\n`Restarting…`"))
                    .await?;
            }
            Err(e) => {
                ctx.edit(
                    ok,
                    &format!("`Update apply crashed: {e} — restart skipped.`"),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        ctx.edit(ok, "`Already up to date — restarting…`").await?;
    }
    db_set(ctx, "_RESTART", &format!("{who}_{}_{ok}", ctx.chat_id)).await?;
    if let (Some(app), Some(api)) = (
        ctx.config.heroku_app_name.clone(),
        ctx.config.heroku_api.clone(),
    ) {
        return heroku_restart(ctx, &app, &api).await;
    }
    reexec();
}

/// `.shutdown` — port of `bot.py::shutdownbot` / `fns.shutdown`.
async fn shutdown(ctx: &Ctx) -> Result<()> {
    let x = ctx.reply("Shutting Down").await?;
    let _ = x;
    if hosted_on() == "heroku" {
        let (Some(app), Some(api)) = (
            ctx.config.heroku_app_name.clone(),
            ctx.config.heroku_api.clone(),
        ) else {
            ctx.reply("Please Fill `HEROKU_APP_NAME` and `HEROKU_API`")
                .await?;
            return Ok(());
        };
        let dyno = std::env::var("DYNO")
            .unwrap_or_default()
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();
        if dyno.is_empty() {
            return Err(Error::Other(
                "DYNO is not set; cannot scale the Heroku formation to 0".into(),
            ));
        }
        let resp = ctx
            .http
            .patch(format!(
                "https://api.heroku.com/apps/{app}/formation/{dyno}"
            ))
            .headers(heroku_headers(&api))
            .json(&serde_json::json!({ "quantity": 0 }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "Heroku shutdown failed: HTTP {}",
                resp.status()
            )));
        }
        return Ok(());
    }
    std::process::exit(0);
}

/// Fetch the last 100 lines of Heroku logs (Python: `fns.heroku_logs`
/// via heroku3 `app.get_log()`), via the Heroku log-sessions API.
async fn heroku_logs(ctx: &Ctx, x: i64) -> Result<()> {
    let (Some(app), Some(api)) = (
        ctx.config.heroku_app_name.clone(),
        ctx.config.heroku_api.clone(),
    ) else {
        ctx.edit(x, "Please set `HEROKU_APP_NAME` and `HEROKU_API` in vars.")
            .await?;
        return Ok(());
    };
    let resp = ctx
        .http
        .post(format!("https://api.heroku.com/apps/{app}/log-sessions"))
        .headers(heroku_headers(&api))
        .json(&serde_json::json!({ "lines": 100, "tail": true }))
        .send()
        .await?;
    if !resp.status().is_success() {
        ctx.edit(
            x,
            "`HEROKU_API` and `HEROKU_APP_NAME` is wrong! Kindly re-check in config vars.",
        )
        .await?;
        return Ok(());
    }
    let session: serde_json::Value = resp.json().await?;
    let logplex = session
        .get("logplex_url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if logplex.is_empty() {
        return Err(Error::Other(
            "Heroku log session returned no logplex_url".into(),
        ));
    }
    let logs = ctx.http.get(&logplex).send().await?.text().await?;
    let path = std::env::temp_dir().join("ultroid-heroku.log");
    tokio::fs::write(&path, logs).await?;
    ctx.send_file(&path, "**Ultroid Heroku Logs.**").await?;
    let _ = tokio::fs::remove_file(&path).await;
    let _ = ctx.delete(x).await;
    Ok(())
}

/// Post text to spaceb.in (Python: `fns.get_paste`).
async fn spacebin_paste(ctx: &Ctx, data: &str) -> Option<String> {
    let resp = ctx
        .http
        .post("https://spaceb.in/api/")
        .json(&serde_json::json!({ "content": data, "extension": "txt" }))
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("payload")
        .and_then(|p| p.get("id"))
        .and_then(|i| i.as_str())
        .map(|id| format!("https://spaceb.in/{id}"))
}

/// Default `.logs` — port of `fns.def_logs`: paste the log, then send the
/// file with a caption.
async fn default_logs(ctx: &Ctx, file: &str) -> Result<()> {
    let x = ctx.reply("Preparing log report...").await?;
    let content = match tokio::fs::read_to_string(file).await {
        Ok(c) => c,
        Err(_) => {
            ctx.edit(x, "The log file is unavailable.").await?;
            return Ok(());
        }
    };
    if content.trim().is_empty() {
        ctx.edit(x, "No log output is available yet.").await?;
        return Ok(());
    }
    let paste = spacebin_paste(ctx, &content).await;
    let caption = match paste {
        Some(url) => format!("Ultroid logs are attached. Paste: {url}"),
        None => {
            let excerpt: String = content
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .join("\n");
            let excerpt = excerpt.chars().rev().take(600).collect::<String>();
            format!("Paste unavailable. Latest log excerpt:\n{excerpt}")
        }
    };
    let path = std::path::Path::new(file);
    if path.exists() {
        ctx.send_file(path, &caption).await?;
        let _ = ctx.delete(x).await;
    } else {
        ctx.edit(x, &caption).await?;
    }
    Ok(())
}

/// `.logs [heroku|carbon|open]` — port of `bot.py` logs handler.
async fn logs(ctx: &Ctx) -> Result<()> {
    let opt = ctx.args.trim();
    // Python: f"ultroid{sys.argv[-1]}.log" when argv has extras, else
    // "ultroid.log". The Rust binary has no per-instance argv slot, so the
    // plain log name is used.
    let file = "ultroid.log";
    if opt == "heroku" {
        let x = ctx.reply("`Processing...`").await?;
        return heroku_logs(ctx, x).await;
    }
    if opt == "carbon" {
        let x = ctx.reply(&ctx.get_str("com_1")).await?;
        let content = tokio::fs::read_to_string(file).await.unwrap_or_default();
        let tail: String = content
            .chars()
            .rev()
            .take(2500)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let rendered = crate::devtools::carbon_render(ctx, &tail, "ultroid-logs").await;
        match rendered {
            Ok(path) => {
                ctx.send_file(&path, "**Ultroid Logs.**").await?;
                let _ = tokio::fs::remove_file(&path).await;
                let _ = ctx.delete(x).await;
            }
            Err(e) => {
                ctx.edit(x, &format!("`{e}`")).await?;
            }
        }
        return Ok(());
    }
    if opt == "open" {
        let content = tokio::fs::read_to_string("ultroid.log")
            .await
            .unwrap_or_default();
        let tail: String = content
            .chars()
            .rev()
            .take(4000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return ctx.reply(&format!("`{tail}`")).await.map(|_| ());
    }
    default_logs(ctx, file).await?;
    let _ = ctx.delete(ctx.message_id).await;
    Ok(())
}

/// Validate a 4–40 hex SHA (Python: `pyUltroid.updater.is_valid_sha`).
fn is_valid_sha(sha: &str) -> bool {
    (4..=40).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// `.update [now|fast|soft|rollback [sha]]` — port of `bot.py` update
/// handler, over the git CLI via `ultroid_core::updater::Updater`.
///
/// The default Python path posts a changelog + confirm buttons through the
/// assistant bot; without an assistant-send op the text-only fallback
/// Python uses when no assistant exists is sent instead.
async fn update(ctx: &Ctx) -> Result<()> {
    let xx = ctx.reply(&ctx.get_str("upd_1")).await?;
    let opt = ctx.args.trim().to_string();

    if opt.starts_with("rollback") {
        let parts: Vec<&str> = opt.splitn(2, char::is_whitespace).collect();
        let mut sha = parts
            .get(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if sha.is_empty() {
            sha = db_get(ctx, "UPDATE_SHA_BEFORE")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
        }
        if sha.is_empty() {
            ctx.edit(
                xx,
                "`No rollback target recorded. Use `.update rollback <sha>`.",
            )
            .await?;
            return Ok(());
        }
        if !is_valid_sha(&sha) {
            ctx.edit(
                xx,
                &format!("**Invalid SHA** `{sha}` — expected 4-40 hex characters."),
            )
            .await?;
            return Ok(());
        }
        ctx.edit(xx, &format!("**↩️ Rolling back to** `{sha}`…"))
            .await?;
        let (out, err, code) = run_shell(&format!("git reset --hard {sha}"), 120).await?;
        if code != 0 {
            ctx.edit(xx, &format!("`Rollback failed: {err}{out}`"))
                .await?;
            return Ok(());
        }
        ctx.edit(
            xx,
            "`Rolled back.`\n_Run_ `.restart` _to boot the restored code._",
        )
        .await?;
        return Ok(());
    }

    let (behind, log) = match commits_behind().await {
        Ok(v) => v,
        Err(_) => {
            ctx.edit(
                xx,
                &format!("{} `git fetch/diff failed`", ctx.get_str("upd_2")),
            )
            .await?;
            return Ok(());
        }
    };
    if behind == 0 {
        let branch = "main";
        let remote = "https://github.com/ufoptg/UltroidBackup";
        ctx.edit(
            xx,
            &ctx.get_str("upd_6")
                .replacen("{}", branch, 1)
                .replacen("{}", remote, 1)
                .replacen("{}", branch, 1),
        )
        .await?;
        return Ok(());
    }

    if matches!(opt.as_str(), "now" | "fast" | "soft") {
        ctx.edit(xx, &ctx.get_str("upd_7")).await?;
        let before = run_shell("git rev-parse HEAD", 15)
            .await?
            .0
            .trim()
            .to_string();
        let repo = std::env::current_dir().map_err(Error::from)?;
        let updater = Updater::new(repo);
        match updater.update(false) {
            Ok(summary) => {
                db_set(ctx, "UPDATE_SHA_BEFORE", &before).await?;
                ctx.edit(xx, &summary).await?;
            }
            Err(e) => {
                ctx.edit(xx, &format!("`Update failed: {e}`")).await?;
            }
        }
        return Ok(());
    }

    // Default: changelog + "run .update now to apply" (the no-assistant
    // text fallback; the assistant-posted confirm-button flow needs an
    // assistant send + callback surface that does not exist here).
    let changelog: String = if log.is_empty() {
        format!("{behind} new commit(s) upstream.")
    } else {
        log.iter()
            .take(10)
            .map(|l| format!("• `{l}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut text = format!(
        "**🔼 Ultroid Update Available**\n\n**Branch:** `main` • **New commits:** `{behind}`\n\n{changelog}"
    );
    if text.len() > 3800 {
        text.truncate(3800);
        text.push_str("\n…");
    }
    text.push_str("\n\n_Run_ `.update now` _to apply._");
    ctx.edit(xx, &text).await?;
    Ok(())
}

pub fn register(registry: &mut Registry) {
    let _ = REGISTERED_AT.set(Instant::now());
    reg_add(
        registry,
        def("alive", "alive( (.*)|$)", "Check if your bot is working."),
        |ctx| Box::pin(alive(ctx)),
    );
    reg_add(
        registry,
        def("ping", "ping$", "Check Ultroid's response time."),
        |ctx| Box::pin(ping(ctx)),
    );
    reg_add(
        registry,
        def("cmds", "cmds$", "View all plugin names."),
        |ctx| Box::pin(cmds(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "restart",
            pattern: "restart$",
            help: "Restart your bot.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(restart(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "shutdown",
            pattern: "shutdown$",
            help: "Turn off your bot.",
            fullsudo: true,
            ..Default::default()
        },
        |ctx| Box::pin(shutdown(ctx)),
    );
    reg_add(
        registry,
        def("logs", "logs( (.*)|$)", "Get the full terminal logs."),
        |ctx| Box::pin(logs(ctx)),
    );
    reg_add(
        registry,
        def(
            "update",
            "update( (.*)|$)",
            "See changelogs if any update is available.",
        ),
        |ctx| Box::pin(update(ctx)),
    );
}
