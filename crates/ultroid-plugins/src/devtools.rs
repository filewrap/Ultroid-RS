//! Dev tools — port of `plugins/devtools.py`
//! (`.sysinfo` / `.bash` / `.cpp`).
//!
//! * `.sysinfo` runs `neofetch` through the same sandboxed shell
//!   (`ultroid_core::modules::run_shell`, `sh -c`, 1–120 s clamp) and
//!   renders the stripped output with the Carbonara API, exactly like
//!   Python's `fns.bash` + `fns.Carbon`.
//! * `.bash` runs the command via `run_shell` and formats output the way
//!   Python does, including the `pip` YAML-ish reformat (a light
//!   `key: value` line parser — there is no YAML dep in the workspace)
//!   and the carbon/rayso image variants.
//! * `.cpp` writes the snippet (wrapped in the `DUMMY_CPP` template when
//!   it has no `main(`), compiles with `g++`, runs the binary, cleans up.
//!
//! Differences forced by the surface:
//! * Carbon/rayso in Python is gated on the chat being private or the
//!   user being a group admin / embed-links being allowed; `Ctx` carries
//!   no admin-rights info, so the image variants are used only in private
//!   chats (and on an explicit `-c` / `-r` flag); elsewhere the plain
//!   text output is shown.
//! * Python writes `cpp-ultroid.cpp` / `CppUltroid` into the working
//!   directory; the Rust port uses the system temp dir so a bot run never
//!   litters its repo checkout.

use crate::util::{db_get, def, reg_add};
use std::path::PathBuf;
use ultroid_core::modules::run_shell;
use ultroid_core::{CommandDef, Ctx, Error, Registry, Result};

/// Carbonara background palette (Python `ATRA_COL` in `plugins/__init__.py`).
pub const ATRA_COL: [&str; 12] = [
    "DarkCyan",
    "DeepSkyBlue",
    "DarkTurquoise",
    "Cyan",
    "LightSkyBlue",
    "Turquoise",
    "MediumVioletRed",
    "Aquamarine",
    "Lightcyan",
    "Azure",
    "Moccasin",
    "PowderBlue",
];

fn random_color() -> &'static str {
    ATRA_COL[rand::random::<usize>() % ATRA_COL.len()]
}

fn is_carbon_error(bytes: &[u8]) -> Option<String> {
    // Carbonara returns a JSON object on error and raw image bytes on
    // success (Python: `isinstance(file, dict)` check in the callers).
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) if v.is_object() => Some(v.to_string()),
        _ => None,
    }
}

/// Port of `fns.Carbon` — render `code` to an image via the Carbonara
/// API (or the Ray.so API when `rayso` is set) and write it to a temp
/// file. Returns the file path, or `Err` with the API's error payload.
pub async fn carbon_render(ctx: &Ctx, code: &str, file_name: &str) -> Result<PathBuf> {
    carbon_render_mode(ctx, code, file_name, false).await
}

pub async fn carbon_render_mode(
    ctx: &Ctx,
    code: &str,
    file_name: &str,
    rayso: bool,
) -> Result<PathBuf> {
    let (url, body) = if rayso {
        (
            "https://rayso-api-desvhu-33.koyeb.app/generate",
            serde_json::json!({
                "text": code,
                "theme": "breeze",
                "darkMode": true,
                "title": "Ultroid",
            }),
        )
    } else {
        (
            "https://carbonara.solopov.dev/api/cook",
            serde_json::json!({
                "code": code,
                "backgroundColor": random_color(),
            }),
        )
    };
    let resp = ctx.http.post(url).json(&body).send().await?;
    let bytes = resp.bytes().await.map_err(Error::from)?;
    if let Some(err) = is_carbon_error(&bytes) {
        return Err(Error::Other(format!("Carbon API error: {err}")));
    }
    let path = std::env::temp_dir().join(format!("{file_name}.jpg"));
    tokio::fs::write(&path, &bytes).await?;
    Ok(path)
}

/// `.sysinfo` — port of `devtools.py` sysinfo handler.
async fn sysinfo(ctx: &Ctx) -> Result<()> {
    let xx = ctx.reply(&ctx.get_str("com_1")).await?;
    // Python: `neofetch|sed 's/\x1b\[[0-9;\?]*[a-zA-Z]//g' >> neo.txt`
    // — the ANSI-stripping sed, with stdout captured directly instead of
    // via a scratch file.
    let (out, err, _code) = run_shell(
        "neofetch 2>/dev/null | sed 's/\x1b\\[[0-9;\\?]*[a-zA-Z]//g'",
        60,
    )
    .await?;
    if err.to_lowercase().contains("not found") || out.trim().is_empty() && err.contains("neofetch")
    {
        return ctx.edit(xx, &format!("Error: `{err}`")).await.map(|_| ());
    }
    let p = out.replace("\n\n", "");
    match carbon_render(ctx, &p, "neofetch").await {
        Ok(path) => {
            ctx.send_file(&path, "").await?;
            let _ = tokio::fs::remove_file(&path).await;
            let _ = ctx.delete(xx).await;
            Ok(())
        }
        Err(e) => ctx.edit(xx, &format!("`{e}`")).await.map(|_| ()),
    }
}

/// Reformat `pip ...` output the way Python's `fns.safe_load` branch
/// does: `key: value` lines become `**key**  :  `value`` lines.
fn pip_format(stdout: &str) -> Option<String> {
    let mut out = String::new();
    for line in stdout.lines() {
        let Some((k, v)) = line.split_once(':') else {
            return None;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() {
            return None;
        }
        let v = if !v.is_empty() && !v.contains("http") {
            format!("`{v}`")
        } else {
            v.to_string()
        };
        out.push_str(&format!("**{k}**  :  {v}\n"));
    }
    Some(out)
}

/// `.bash [-c|--carbon] [-r|--rayso] <cmd>` — port of `devtools.py` bash.
async fn bash(ctx: &Ctx) -> Result<()> {
    let mut cmd = match ctx.text.splitn(2, char::is_whitespace).nth(1) {
        Some(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ => {
            return ctx.reply(&ctx.get_str("devs_1")).await.map(|_| ());
        }
    };
    let mut carb = false;
    let mut rayso = false;
    let first = cmd.split_whitespace().next().unwrap_or("");
    if first == "-c" || first == "--carbon" {
        carb = true;
        cmd = cmd
            .splitn(2, char::is_whitespace)
            .nth(1)
            .unwrap_or("")
            .to_string();
    } else if first == "-r" || first == "--rayso" {
        rayso = true;
        cmd = cmd
            .splitn(2, char::is_whitespace)
            .nth(1)
            .unwrap_or("")
            .to_string();
    }
    let xx = ctx.reply(&ctx.get_str("com_1")).await?;
    let (stdout, stderr, _code) = run_shell(&cmd, 120).await?;
    let stdout = stdout.trim().to_string();
    let stderr = stderr.trim().to_string();

    let mut out_text = format!("**☞ BASH\n\n• COMMAND:**\n`{cmd}` \n\n");
    let mut err_text = String::new();
    if !stderr.is_empty() {
        err_text = format!("**• ERROR:** \n`{stderr}`\n\n");
    }
    let mut body = String::new();
    if !stdout.is_empty() {
        let carbon_on =
            db_get(ctx, "CARBON_ON_BASH").await.ok().flatten() == Some("true".to_string());
        let rayso_on =
            db_get(ctx, "RAYSO_ON_BASH").await.ok().flatten() == Some("true".to_string());
        // Admin-rights info is not on Ctx; image variants are used only
        // in private chats (Python also allows group admins / embed-links).
        let allow_image = matches!(ctx.chat_type, ultroid_core::ChatType::Private);
        if (carb || carbon_on) && allow_image {
            // Python uploads the render to Telegraph and links it; there
            // is no Telegraph-upload op here, so the rendered image is
            // sent as the message media with the header as its caption.
            return match carbon_render_mode(ctx, &stdout, "bash", false).await {
                Ok(path) => {
                    let caption = format!("{out_text}{err_text}**• OUTPUT:**");
                    ctx.send_file(&path, &caption).await?;
                    let _ = tokio::fs::remove_file(&path).await;
                    let _ = ctx.delete(xx).await;
                    Ok(())
                }
                Err(e) => {
                    return ctx
                        .edit(
                            xx,
                            &format!(
                                "Unknown Response from Carbon: `{e}`\n\nstdout`:{stdout}`\nstderr: `{stderr}`"
                            ),
                        )
                        .await
                        .map(|_| ());
                }
            };
        } else if (rayso || rayso_on) && allow_image {
            return match carbon_render_mode(ctx, &stdout, "bash", true).await {
                Ok(path) => {
                    let caption = format!("{out_text}{err_text}**• OUTPUT:**");
                    ctx.send_file(&path, &caption).await?;
                    let _ = tokio::fs::remove_file(&path).await;
                    let _ = ctx.delete(xx).await;
                    Ok(())
                }
                Err(e) => {
                    return ctx
                        .edit(
                            xx,
                            &format!(
                                "Unknown Response from Carbon: `{e}`\n\nstdout`:{stdout}`\nstderr: `{stderr}`"
                            ),
                        )
                        .await
                        .map(|_| ());
                }
            };
        } else if cmd.contains("pip")
            && stdout.lines().all(|l| l.contains(':'))
            && let Some(fmt) = pip_format(&stdout)
        {
            body = format!("**• OUTPUT:**\n{fmt}");
        } else {
            body = format!("**• OUTPUT:**\n`{stdout}`");
        }
    }
    if stderr.is_empty() && stdout.is_empty() {
        body = "**• OUTPUT:**\n`Success`".to_string();
    }
    out_text.push_str(&err_text);
    out_text.push_str(&body);
    if out_text.len() > 4096 {
        let text = format!("{err_text}{body}");
        let path = std::env::temp_dir().join("bash.txt");
        tokio::fs::write(&path, text).await?;
        let caption = if cmd.len() < 998 {
            format!("`{cmd}`")
        } else {
            String::new()
        };
        ctx.send_file(&path, &caption).await?;
        let _ = tokio::fs::remove_file(&path).await;
        let _ = ctx.delete(xx).await;
    } else {
        ctx.edit(xx, &out_text).await?;
    }
    Ok(())
}

const DUMMY_CPP: &str = "#include <iostream>\nusing namespace std;\n\nint main(){\n!code\n}\n";

/// `.cpp <code>` — port of `devtools.py::doie`.
async fn cpp(ctx: &Ctx) -> Result<()> {
    let mut code = match ctx.text.splitn(2, char::is_whitespace).nth(1) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => {
            return ctx.reply(&ctx.get_str("devs_3")).await.map(|_| ());
        }
    };
    let msg = ctx.reply(&ctx.get_str("com_1")).await?;
    if !code.contains("main(") {
        let indented: String = code.lines().map(|l| format!("    {l}\n")).collect();
        code = DUMMY_CPP.replace("!code", &indented);
    }
    let dir = std::env::temp_dir().join(format!("ultroid-cpp-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await?;
    let src = dir.join("cpp-ultroid.cpp");
    let bin = dir.join("CppUltroid");
    tokio::fs::write(&src, &code).await?;
    let (compile_out, compile_err, _c) =
        run_shell(&format!("g++ -o {} {}", bin.display(), src.display()), 120).await?;
    let mut o_cpp = format!("• **Cpp**\n`{code}`");
    if !compile_err.trim().is_empty() || !compile_out.trim().is_empty() && !bin.exists() {
        let err = if compile_err.trim().is_empty() {
            compile_out.clone()
        } else {
            compile_err.clone()
        };
        o_cpp.push_str(&format!("\n\n**• Error :**\n`{err}`"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        if o_cpp.len() > 3000 {
            let path = std::env::temp_dir().join("error.txt");
            tokio::fs::write(&path, &o_cpp).await?;
            ctx.send_file(&path, &format!("`{code}`")).await?;
            let _ = tokio::fs::remove_file(&path).await;
        } else {
            ctx.edit(msg, &o_cpp).await?;
        }
        return Ok(());
    }
    let (run_out, run_err, _r) = run_shell(&format!("{}", bin.display()), 120).await?;
    if !run_out.trim().is_empty() {
        o_cpp.push_str(&format!("\n\n**• Output :**\n`{}`", run_out.trim()));
    }
    if !run_err.trim().is_empty() {
        o_cpp.push_str(&format!("\n\n**• Error :**\n`{}`", run_err.trim()));
    }
    let _ = tokio::fs::remove_dir_all(&dir).await;
    if o_cpp.len() > 3000 {
        let path = std::env::temp_dir().join("cpp.txt");
        tokio::fs::write(&path, &o_cpp).await?;
        ctx.send_file(&path, &format!("`{code}`")).await?;
        let _ = tokio::fs::remove_file(&path).await;
    } else {
        ctx.edit(msg, &o_cpp).await?;
    }
    Ok(())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("sysinfo", "sysinfo$", "Show system info (neofetch)."),
        |ctx| Box::pin(sysinfo(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "bash",
            pattern: "bash",
            help: "Run linux commands on telegram.",
            fullsudo: true,
            only_devs: true,
            ..Default::default()
        },
        |ctx| Box::pin(bash(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            name: "cpp",
            pattern: "cpp",
            help: "Compile and run C++ code from Telegram.",
            only_devs: true,
            ..Default::default()
        },
        |ctx| Box::pin(cpp(ctx)),
    );
}
