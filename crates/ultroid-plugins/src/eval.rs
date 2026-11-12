//! Code evaluation — port of `plugins/eval.py` (`.eval`).
//!
//! WHAT PYTHON ACTUALLY DOES (verified in the source): `.eval` runs the
//! code **in-process** via `exec()` of a generated `async def __aexec__`
//! wrapper, with the live Telethon client, event, database, logger and
//! helper objects injected into the eval scope (`client`, `c`, `bot`,
//! `asst`, `e`, `m`, `msg`, `reply`, `chat`, `db`, `udB`, `http`, `os`,
//! `sys`, `json_parse`, `pp`, …). There is no Rust equivalent of
//! in-process Python evaluation with live objects, and no `python`
//! embedding is available in this crate.
//!
//! IMPLEMENTED MECHANISM (shell-out, reported exactly): the code is
//! written to a temp file and executed out-of-process with the system
//! `python3` binary via `tokio::process::Command`, capturing stdout /
//! stderr / exit code with a 120 s timeout (the agent shell clamp).
//! The output rendering is ported faithfully: the `✅ Eval (dur)` HTML
//! header with `In:` / `Out:` `<pre>` blocks, the 4096-char cap, the
//! paste.rs fallback for long output, and the `eval.txt` file fallback.
//! Error output is a short in-chat notice (no traceback in chat); the
//! full traceback is only logged, matching Python's log-channel-only
//! rule in spirit (there is no log-channel send op here — see below).
//!
//! Deltas vs Python, stated plainly:
//! * No live objects in scope: `client`, `e`, `db`, `udB`, `http` etc.
//!   do not exist in the shell-out process. Code that touches Telegram
//!   or the bot database cannot work; it fails with a normal Python
//!   `NameError` in the captured stderr.
//! * `-gs` / `--source` and `-ga` / `--args` (inspect-based introspection
//!   of the evaluated value) have no meaning for a subprocess — they
//!   reply explaining that instead of faking a result.
//! * No `black` formatting pass (no formatter dep in the workspace).
//! * Silent mode (`-s`) deletes the trigger and says nothing in chat;
//!   on error Python reports the traceback to the log channel, which
//!   needs a send-to-chat op — here the error is logged server-side
//!   (`tracing::warn`) instead.

use crate::util::reg_add;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use ultroid_core::{CommandDef, Ctx, Registry, Result};

/// Port of `eval.py::_fmt_dur`.
fn fmt_dur(micros: u64) -> String {
    let ms = micros as f64 / 1_000.0;
    let s = ms / 1_000.0;
    let m = s / 60.0;
    let h = m / 60.0;
    let d = h / 24.0;
    if d >= 1.0 {
        format!("{}d {}h", d as u64, (h % 24.0) as u64)
    } else if h >= 1.0 {
        format!("{}h {}m", h as u64, (m % 60.0) as u64)
    } else if m >= 1.0 {
        format!("{}m {}s", m as u64, (s % 60.0) as u64)
    } else if s >= 1.0 {
        format!("{s:.2} sec")
    } else if ms >= 1.0 {
        format!("{} ms", ms as u64)
    } else {
        format!("{micros} μs")
    }
}

fn escape_html(text: &str) -> String {
    ultroid_core::fns::escape_html(text)
}

/// Port of `eval.py::_paste` — paste.rs, returns the URL or `None`.
async fn paste(ctx: &Ctx, text: &str) -> Option<String> {
    let resp = ctx
        .http
        .post("https://paste.rs")
        .body(text.to_string())
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let url = resp.text().await.ok()?;
    let url = url.trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

/// Render eval output — port of `eval.py::_render_output`.
async fn render_output(
    ctx: &Ctx,
    xx: Option<i64>,
    code: &str,
    evaluation: &str,
    dur_str: &str,
    is_err: bool,
) -> Result<()> {
    if is_err {
        // Short in-chat notice, never the traceback (Python's rule).
        let text = format!(
            "❌ **Eval failed** `({dur_str})`\n\n```\n{}\n```\n\n_Eval ({dur_str}) — see logs for the full traceback_",
            evaluation.lines().take(5).collect::<Vec<_>>().join("\n")
        );
        tracing::warn!("eval error:\n{evaluation}");
        return match xx {
            Some(id) => ctx.edit(id, &text).await,
            None => ctx.reply(&text).await.map(|_| ()),
        };
    }

    let header = format!(
        "<b>✅ Eval</b>  <code>({dur_str})</code>\n<b>In:</b>\n<pre language='python'>{}</pre>\n<b>Out:</b>",
        escape_html(&code.chars().take(512).collect::<String>())
            + if code.len() > 512 { "..." } else { "" }
    );
    let body = escape_html(evaluation);
    let full = format!("{header}\n<pre language='python'>{body}</pre>");

    if full.len() <= 4096 {
        return match xx {
            Some(id) => match ctx.edit(id, &full).await {
                Ok(()) => Ok(()),
                Err(_) => ctx.reply_html(&full).await.map(|_| ()),
            },
            None => ctx.reply_html(&full).await.map(|_| ()),
        };
    }

    // Too long -> paste.rs.
    if let Some(url) = paste(ctx, &format!("In:\n{code}\n\nOut:\n{evaluation}")).await {
        let snip = escape_html(
            &(evaluation.chars().take(1500).collect::<String>()
                + if evaluation.len() > 1500 { "\n..." } else { "" }),
        );
        let text = format!(
            "{header}\n<pre language='python'>{snip}</pre>\n<blockquote><a href='{url}'><b>Full output →</b></a></blockquote>"
        );
        return match xx {
            Some(id) => match ctx.edit(id, &text).await {
                Ok(()) => Ok(()),
                Err(_) => ctx.reply_html(&text).await.map(|_| ()),
            },
            None => ctx.reply_html(&text).await.map(|_| ()),
        };
    }

    // paste.rs failed -> file upload.
    let path = std::env::temp_dir().join("eval.txt");
    let content = format!("In:\n{code}\n\nOut:\n{evaluation}");
    tokio::fs::write(&path, content).await?;
    let summary = format!("<b>Eval</b> <code>({dur_str})</code> — output attached");
    // `send_file` sends plain-text captions; strip the tags for the caption.
    ctx.send_file(&path, "Eval output attached").await?;
    let _ = summary;
    let _ = tokio::fs::remove_file(&path).await;
    if let Some(id) = xx {
        let _ = ctx.delete(id).await;
    }
    Ok(())
}

/// Run Python code out-of-process and return `(stdout, stderr, exit_code)`.
async fn run_python(code: &str) -> Result<(String, String, i32)> {
    let path = std::env::temp_dir().join(format!("ultroid-eval-{}.py", uuid::Uuid::new_v4()));
    {
        let mut f = tokio::fs::File::create(&path).await?;
        f.write_all(code.as_bytes()).await?;
        f.flush().await?;
    }
    let fut = tokio::process::Command::new("python3").arg(&path).output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(120), fut).await;
    let _ = tokio::fs::remove_file(&path).await;
    let out = match out {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            return Err(ultroid_core::Error::Other(
                "eval timed out after 120s".to_string(),
            ));
        }
    };
    Ok((
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    ))
}

/// `.eval [-s|--silent|-n|--noedit|-gs|--source|-ga|--args] <code>` —
/// port of `eval.py::_cmd_eval`.
async fn eval_cmd(ctx: &Ctx) -> Result<()> {
    let mut raw = match ctx.text.splitn(2, char::is_whitespace).nth(1) {
        Some(r) if !r.trim().is_empty() => r.trim().to_string(),
        _ => {
            return ctx.reply(&ctx.get_str("devs_2")).await.map(|_| ());
        }
    };
    let mut mode = "";
    let first = raw.split_whitespace().next().unwrap_or("");
    for (flag, m) in [
        ("-s", "silent"),
        ("--silent", "silent"),
        ("-n", "noedit"),
        ("--noedit", "noedit"),
        ("-gs", "gsource"),
        ("--source", "gsource"),
        ("-ga", "gargs"),
        ("--args", "gargs"),
    ] {
        if first == flag {
            mode = m;
            raw = raw
                .splitn(2, char::is_whitespace)
                .nth(1)
                .unwrap_or("")
                .to_string();
            break;
        }
    }
    let code = raw.trim().to_string();
    if code.is_empty() {
        return ctx.reply(&ctx.get_str("devs_2")).await.map(|_| ());
    }
    if mode == "gsource" || mode == "gargs" {
        // `inspect.getsource` / `inspect.signature` of the evaluated value
        // only exist for in-process eval; the shell-out port has no
        // evaluated value to introspect.
        return ctx
            .reply(
                "`--source` / `--args` need in-process eval and are unavailable \
                 in the shell-out port (the code runs in a separate python3 process).",
            )
            .await
            .map(|_| ());
    }

    let mut xx: Option<i64> = None;
    if mode == "silent" {
        let _ = ctx.delete(ctx.message_id).await;
    } else if mode == "noedit" {
        xx = Some(ctx.reply(&ctx.get_str("com_1")).await?);
    } else {
        xx = Some(ctx.reply_html("<b>⏳ Running eval</b>").await?);
    }

    let t0 = Instant::now();
    let (stdout, stderr, code_status) = run_python(&code).await?;
    let elapsed = t0.elapsed().as_micros() as u64;
    let dur_str = fmt_dur(elapsed);

    let is_err = code_status != 0 || !stderr.trim().is_empty();
    let evaluation = if is_err {
        if stderr.trim().is_empty() {
            format!("exited with status {code_status}")
        } else {
            stderr.trim().to_string()
        }
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        ctx.get_str("instu_4")
    };

    if mode == "silent" {
        if is_err {
            tracing::warn!("silent eval error:\n{evaluation}");
        }
        return Ok(());
    }
    render_output(ctx, xx, &code, &evaluation, &dur_str, is_err).await
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            name: "eval",
            pattern: "eval",
            help: "Evaluate python code on telegram (runs out-of-process via python3).",
            fullsudo: true,
            only_devs: true,
            ..Default::default()
        },
        |ctx| Box::pin(eval_cmd(ctx)),
    );
}
