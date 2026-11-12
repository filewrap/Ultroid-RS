//! Port of `plugins/tracer.py` — code-index search + trace inspection.
//!
//! `.find <query>`, `.src <plugin> <func>`, `.srcof <pattern>`,
//! `.whocalls <func>`, `.dbkey <KEY>`, `.hmap`, `.conflicts`,
//! `.traces [n]`, `.tfail [plugin]`, `.tslow`, `.tuser <user_id>`,
//! `.tstats`, `.idxstats`, `.reindex`.
//!
//! Wired to `ultroid_core::tracer::Tracer`, which indexes the Rust
//! source tree under the process working directory (the `ultroid-rs`
//! checkout at runtime). Search / definitions / source extraction are
//! real. Differences from the Python version, all honest:
//! * `.hmap` / `.conflicts` are built by grepping the plugins crate for
//!   `def("name", "pattern", ...)` registrations — the real handler map
//!   of this build — instead of Telethon's runtime registry.
//! * The live trace history (`.traces`, `.tfail`, `.tslow`, `.tuser`,
//!   `.tstats`) has no counterpart in `ultroid_core::tracer` (no live
//!   tracker was ported); those commands say so instead of inventing
//!   traces.
//! * `.reindex` re-walks the tree and reports how many `.rs` files the
//!   stateless grep index sees.

use crate::util::{def, reg_add};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use ultroid_core::tracer::{TraceHit, Tracer};
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    let owner = |name: &'static str, pattern: &'static str, help: &'static str| {
        let mut d = def(name, pattern, help);
        d.owner_only = true;
        d
    };
    reg_add(
        registry,
        owner(
            "find",
            "find(?: |$)([\\s\\S]*)",
            "Find a handler/function by name or pattern: `.find <query>`.",
        ),
        |ctx| Box::pin(find_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "src",
            "src(?: |$)(\\w+)\\s+(\\w+)",
            "Extract live source of a function: `.src <plugin> <func>`.",
        ),
        |ctx| Box::pin(src_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "srcof",
            "srcof(?: |$)([\\w.]+)",
            "Find a handler and extract its source: `.srcof <pattern>`.",
        ),
        |ctx| Box::pin(srcof_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "whocalls",
            "whocalls(?: |$)([\\w_]+)",
            "Find all callers of a function: `.whocalls <func>`.",
        ),
        |ctx| Box::pin(whocalls_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "dbkey",
            "dbkey(?: |$)([\\w_]+)",
            "Find all functions touching a DB key: `.dbkey <KEY>`.",
        ),
        |ctx| Box::pin(dbkey_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "hmap",
            "hmap$",
            "Show the full handler map (pattern → plugin).",
        ),
        |ctx| Box::pin(hmap_cmd(ctx)),
    );
    reg_add(
        registry,
        owner("conflicts", "conflicts$", "Show handler pattern conflicts."),
        |ctx| Box::pin(conflicts_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "traces",
            "traces(?: |$)(\\d*)",
            "Show last n execution traces: `.traces [n]`.",
        ),
        |ctx| Box::pin(traces_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "tfail",
            "tfail(?: |$)([\\w_]*)",
            "Show recent failures: `.tfail [plugin]`.",
        ),
        |ctx| Box::pin(tfail_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "tslow",
            "tslow$",
            "Show slowest handlers from trace history.",
        ),
        |ctx| Box::pin(tslow_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "tuser",
            "tuser(?: |$)(\\d+)",
            "Show trace history for a user: `.tuser <user_id>`.",
        ),
        |ctx| Box::pin(tuser_cmd(ctx)),
    );
    reg_add(
        registry,
        owner("tstats", "tstats$", "Trace statistics."),
        |ctx| Box::pin(tstats_cmd(ctx)),
    );
    reg_add(
        registry,
        owner("idxstats", "idxstats$", "Code index statistics."),
        |ctx| Box::pin(idxstats_cmd(ctx)),
    );
    reg_add(
        registry,
        owner(
            "reindex",
            "reindex$",
            "Rebuild the plugin index from scratch.",
        ),
        |ctx| Box::pin(reindex_cmd(ctx)),
    );
}

fn tracer() -> Tracer {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Tracer::new(root)
}

fn short_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn fmt_hit(h: &TraceHit) -> String {
    format!(
        "• `{}` (line {})\n  `{}`",
        short_path(&h.file),
        h.line,
        h.text
    )
}

/// Extract the source of the function whose definition starts at
/// `line` (1-based) in `file`, by brace counting.
fn extract_source(file: &Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    if line == 0 || line > lines.len() {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    let mut depth: i64 = 0;
    let mut started = false;
    for l in &lines[line - 1..] {
        out.push(l);
        for ch in l.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if started && depth <= 0 {
            break;
        }
    }
    Some(out.join("\n"))
}

async fn find_cmd(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim();
    if query.is_empty() {
        ctx.reply("`Usage: .find <pattern or function name>`")
            .await?;
        return Ok(());
    }
    let id = ctx.reply("`🔍 searching…`").await?;
    let tr = tracer();
    let hits = tr.search(query);
    if hits.is_empty() {
        ctx.edit(id, &format!("Nothing found for `{query}`."))
            .await?;
        return Ok(());
    }
    let mut text = format!("**🔍 search results for `{query}` ({})**\n\n", hits.len());
    for h in hits.iter().take(15) {
        text.push_str(&fmt_hit(h));
        text.push('\n');
    }
    if hits.len() > 15 {
        text.push_str(&format!("\n_…and {} more_", hits.len() - 15));
    }
    if text.len() > 4000 {
        send_as_file(ctx, &text, "search_results.txt").await?;
        ctx.delete(id).await?;
    } else {
        ctx.edit(id, &text).await?;
    }
    Ok(())
}

async fn src_cmd(ctx: &Ctx) -> Result<()> {
    let mut parts = ctx.args.split_whitespace();
    let plugin = parts.next().unwrap_or("");
    let func = parts.next().unwrap_or("");
    let id = ctx.reply("`🥚 extracting…`").await?;
    let tr = tracer();
    let defs = tr.definitions(func);
    let hit = defs
        .into_iter()
        .find(|h| short_path(&h.file).contains(plugin));
    let Some(hit) = hit else {
        ctx.edit(
            id,
            &format!("Could not extract `{func}` from `{plugin}`.\nCheck `.find {func}` to verify the plugin and function name."),
        )
        .await?;
        return Ok(());
    };
    let Some(code) = extract_source(&hit.file, hit.line) else {
        ctx.edit(id, &format!("Could not extract `{func}` from `{plugin}`."))
            .await?;
        return Ok(());
    };
    let snippet: String = code.chars().take(3500).collect();
    let text = format!("**📄 `{plugin}.{func}`** (live)\n\n```rust\n{snippet}\n```");
    if code.len() > 3500 {
        ctx.delete(id).await?;
        send_as_file(ctx, &code, &format!("{plugin}_{func}.rs")).await?;
    } else {
        ctx.edit(id, &text).await?;
    }
    Ok(())
}

async fn srcof_cmd(ctx: &Ctx) -> Result<()> {
    let pattern = ctx.args.trim().trim_start_matches('.');
    let id = ctx.reply("`🥚 pulling yellow from egg…`").await?;
    let tr = tracer();
    let hits = tr.search(pattern);
    let Some(hit) = hits
        .into_iter()
        .find(|h| h.text.contains("def(") || h.text.contains("pattern"))
    else {
        ctx.edit(id, &format!("No handler found matching `{pattern}`."))
            .await?;
        return Ok(());
    };
    // Walk up to the enclosing `fn` for context.
    let text = std::fs::read_to_string(&hit.file).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let mut fn_line = hit.line;
    for (i, l) in lines.iter().enumerate().take(hit.line).skip(0).rev() {
        if l.trim_start().starts_with("async fn ") || l.trim_start().starts_with("fn ") {
            fn_line = i + 1;
            break;
        }
    }
    let Some(code) = extract_source(&hit.file, fn_line) else {
        ctx.edit(id, &format!("No handler found matching `{pattern}`."))
            .await?;
        return Ok(());
    };
    let header = format!(
        "**📄 Handler: `.{pattern}`**\n`{}` — line {fn_line}\n\n",
        short_path(&hit.file)
    );
    let body: String = code.chars().take(3000).collect();
    if code.len() > 3000 {
        ctx.delete(id).await?;
        send_as_file(ctx, &format!("{header}{code}"), "handler.rs").await?;
    } else {
        ctx.edit(id, &format!("{header}```rust\n{body}\n```"))
            .await?;
    }
    Ok(())
}

async fn whocalls_cmd(ctx: &Ctx) -> Result<()> {
    let func = ctx.args.trim();
    let id = ctx
        .reply(&format!("`🔍 tracing callers of {func}…`"))
        .await?;
    let tr = tracer();
    let hits: Vec<TraceHit> = tr
        .search(func)
        .into_iter()
        .filter(|h| h.text.contains(&format!("{func}(")) && !h.text.contains(&format!("fn {func}")))
        .collect();
    if hits.is_empty() {
        ctx.edit(id, &format!("No callers found for `{func}`."))
            .await?;
        return Ok(());
    }
    let mut text = format!("**🔗 Callers of `{func}` ({})**\n\n", hits.len());
    for h in hits.iter().take(20) {
        text.push_str(&format!("• `{}` (line {})\n", short_path(&h.file), h.line));
    }
    ctx.edit(id, &text).await?;
    Ok(())
}

async fn dbkey_cmd(ctx: &Ctx) -> Result<()> {
    let key = ctx.args.trim();
    let id = ctx.reply(&format!("`🔍 searching for {key}…`")).await?;
    let tr = tracer();
    let hits = tr.search(key);
    if hits.is_empty() {
        ctx.edit(id, &format!("No functions found accessing key `{key}`."))
            .await?;
        return Ok(());
    }
    let mut text = format!("**🗄 Functions using `{key}` ({})**\n\n", hits.len());
    for h in hits.iter().take(20) {
        text.push_str(&fmt_hit(h));
        text.push('\n');
    }
    ctx.edit(id, &text).await?;
    Ok(())
}

/// Build the handler map by grepping the plugins crate for
/// `def("name", "pattern", ...)` registrations.
fn handler_map() -> BTreeMap<String, String> {
    let tr = tracer();
    let mut map = BTreeMap::new();
    for h in tr.search("def(\"") {
        // `def("name", "pattern", "help")` — pull the first two strings.
        let mut strings: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut in_str = false;
        let mut esc = false;
        for ch in h.text.chars() {
            if in_str {
                if esc {
                    cur.push(ch);
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    in_str = false;
                    strings.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            } else if ch == '"' {
                in_str = true;
            }
        }
        if strings.len() >= 2 {
            let plugin = h
                .file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            map.insert(strings[1].clone(), plugin);
        }
    }
    map
}

async fn hmap_cmd(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`📋 building handler map…`").await?;
    let map = handler_map();
    if map.is_empty() {
        ctx.edit(id, "No handlers indexed yet. Try `.reindex`.")
            .await?;
        return Ok(());
    }
    let mut text = format!("**📋 Handler Map ({} patterns)**\n\n", map.len());
    for (pat, plugin) in &map {
        text.push_str(&format!("`{pat}` → `{plugin}`\n"));
    }
    if text.len() > 4000 {
        ctx.delete(id).await?;
        let raw: String = map.iter().map(|(p, pl)| format!("{p}\t{pl}\n")).collect();
        send_as_file(ctx, &raw, "handler_map.txt").await?;
    } else {
        ctx.edit(id, &text).await?;
    }
    Ok(())
}

async fn conflicts_cmd(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`🔍 checking conflicts…`").await?;
    // Exact duplicates of the same pattern only: a pattern registered
    // twice lands in `Registry` twice, so count occurrences per pattern.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for h in tracer().search("def(\"") {
        let mut strings: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut in_str = false;
        for ch in h.text.chars() {
            if in_str {
                if ch == '"' {
                    in_str = false;
                    strings.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            } else if ch == '"' {
                in_str = true;
            }
        }
        if strings.len() >= 2 {
            *counts.entry(strings[1].clone()).or_default() += 1;
        }
    }
    let conflicts: Vec<(&String, &usize)> = counts.iter().filter(|(_, c)| **c > 1).collect();
    if conflicts.is_empty() {
        ctx.edit(id, "✅ No handler conflicts detected.").await?;
        return Ok(());
    }
    let mut text = format!("**⚠️ Handler Conflicts ({})**\n\n", conflicts.len());
    for (pat, n) in conflicts {
        text.push_str(&format!("⚠️ `{pat}` — registered {n} times\n\n"));
    }
    ctx.edit(id, &text).await?;
    Ok(())
}

const NO_LIVE_TRACES: &str = "No live trace history in this build: `ultroid_core::tracer` indexes source code only and has no execution tracker (Python `pyUltroid/tracer/_tracker.py` was not ported). No traces recorded yet.";

async fn traces_cmd(ctx: &Ctx) -> Result<()> {
    let _n: usize = ctx.args.trim().parse().unwrap_or(10).clamp(1, 50);
    let id = ctx.reply("`📊 fetching traces…`").await?;
    ctx.edit(id, NO_LIVE_TRACES).await?;
    Ok(())
}

async fn tfail_cmd(ctx: &Ctx) -> Result<()> {
    let _plugin = ctx.args.trim();
    let id = ctx.reply("`📊 fetching failures…`").await?;
    ctx.edit(id, NO_LIVE_TRACES).await?;
    Ok(())
}

async fn tslow_cmd(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`⏱ fetching slow traces…`").await?;
    ctx.edit(id, NO_LIVE_TRACES).await?;
    Ok(())
}

async fn tuser_cmd(ctx: &Ctx) -> Result<()> {
    let uid = ctx.args.trim();
    let id = ctx
        .reply(&format!("`📊 fetching traces for {uid}…`"))
        .await?;
    ctx.edit(id, NO_LIVE_TRACES).await?;
    Ok(())
}

async fn tstats_cmd(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`📊 computing stats…`").await?;
    ctx.edit(id, NO_LIVE_TRACES).await?;
    Ok(())
}

async fn idxstats_cmd(ctx: &Ctx) -> Result<()> {
    let tr = tracer();
    let mut files = 0usize;
    let mut funcs = 0usize;
    // Walk the tree the same way `Tracer` does, counting real files.
    let root = tr.root.clone();
    fn walk(dir: &Path, files: &mut usize, funcs: &mut usize) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name()
                        .map_or(false, |n| n == "target" || n == ".git")
                    {
                        continue;
                    }
                    walk(&p, files, funcs);
                } else if p.extension().map_or(false, |x| x == "rs") {
                    *files += 1;
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        *funcs += text
                            .lines()
                            .filter(|l| {
                                let t = l.trim_start();
                                t.starts_with("pub fn ")
                                    || t.starts_with("fn ")
                                    || t.starts_with("pub async fn ")
                                    || t.starts_with("async fn ")
                            })
                            .count();
                    }
                }
            }
        }
    }
    walk(&root, &mut files, &mut funcs);
    let handlers = handler_map().len();
    let text = format!(
        "**📋 Code Index**\n\nPlugins:   `{files}`\nHandlers:  `{handlers}`\nFunctions: `{funcs}`\nRoot: `{}`",
        short_path(&root)
    );
    ctx.reply(&text).await?;
    Ok(())
}

async fn reindex_cmd(ctx: &Ctx) -> Result<()> {
    let id = ctx.reply("`🔄 reindexing all plugins…`").await?;
    let t0 = std::time::Instant::now();
    let mut count = 0usize;
    let root = tracer().root.clone();
    fn walk(dir: &Path, count: &mut usize) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name()
                        .map_or(false, |n| n == "target" || n == ".git")
                    {
                        continue;
                    }
                    walk(&p, count);
                } else if p.extension().map_or(false, |x| x == "rs") {
                    *count += 1;
                }
            }
        }
    }
    walk(&root, &mut count);
    let elapsed = t0.elapsed().as_millis();
    ctx.edit(
        id,
        &format!(
            "✅ Indexed `{count}` plugins in `{elapsed}ms`.\nRun `.idxstats` to see the breakdown."
        ),
    )
    .await?;
    Ok(())
}

async fn send_as_file(ctx: &Ctx, content: &str, filename: &str) -> Result<()> {
    let path = std::env::temp_dir().join(filename);
    tokio::fs::write(&path, content).await?;
    ctx.send_file(&path, &format!("`{filename}`")).await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(())
}
