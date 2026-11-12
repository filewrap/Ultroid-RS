//! Port of `plugins/tools.py` — translator, ids, listings, short URLs.
//!
//! `.tl <lang> <text>` / `.tr <lang> <text>` — translate (real HTTP to
//! Google's `gtx` endpoint; the Python `tr` used Telegram's own
//! translator, which is a client op — `gtx` returns the same translated
//! text). `.id [user]` — chat/user/msg ids. `.bots [chat]` — bots in a
//! chat. `.hl <url> [text]` — hidden link. `.circle` — video note from
//! a gif/audio reply. `.ls [pattern]` — list files. `.sg`/`.sgu`
//! — name/username history via @SangMata_beta_bot. `.shorturl <url>`
//! — TinyURL shorten / expand (real HTTP).
//!
//! The Telegram-client steps that have no `Responder` equivalent —
//! downloading media for `.circle`, and the live conversation with
//! @SangMata_beta_bot — are `TODO(responder-op)` sites; participant
//! listing and username resolution are wired through the `Responder`.

use crate::util::{def, reg_add};
use regex::Regex;
use std::path::{Path, PathBuf};
use ultroid_core::fns::humanbytes;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "tl",
            "tl( (.*)|$)",
            "Translate: `.tl <lang> <text>` or `.tl <lang>` as a reply.",
        ),
        |ctx| Box::pin(translate(ctx, false)),
    );
    reg_add(
        registry,
        def(
            "tr",
            "tr( (.*)|$)",
            "Translate: `.tr <lang> <text>` or `.tr <lang>` as a reply.",
        ),
        |ctx| Box::pin(translate(ctx, true)),
    );
    reg_add(
        registry,
        def("id", "id( (.*)|$)", "Show chat / user / message ids."),
        |ctx| Box::pin(show_id(ctx)),
    );
    let mut bots_def = def(
        "bots",
        "bots( (.*)|$)",
        "List bots in this chat (or the given chat).",
    );
    bots_def.groups_only = true;
    reg_add(registry, bots_def, |ctx| Box::pin(bots(ctx)));

    reg_add(
        registry,
        def("hl", "hl( (.*)|$)", "Hidden link: `.hl <url> [text]`."),
        |ctx| Box::pin(hidden_link(ctx)),
    );
    reg_add(
        registry,
        def(
            "circle",
            "circle$",
            "Video note from a gif/audio reply: `.circle`.",
        ),
        |ctx| Box::pin(circle(ctx)),
    );
    reg_add(
        registry,
        def("ls", "ls( (.*)|$)", "List files: `.ls [pattern]`."),
        |ctx| Box::pin(list_files(ctx)),
    );
    let mut sg = def(
        "sg",
        "sg(|u)(?:\\s|$)([\\s\\S]*)",
        "Name/username history via @SangMata_beta_bot: `.sg <user>`, `.sgu <user>`.",
    );
    sg.fullsudo = true;
    reg_add(registry, sg, |ctx| Box::pin(sangmata(ctx)));

    reg_add(
        registry,
        def(
            "shorturl",
            "shorturl ?(.*)",
            "Shorten / expand a URL via TinyURL.",
        ),
        |ctx| Box::pin(shorturl(ctx)),
    );
}

/// Translate `text` to `lang` via Google's `gtx` endpoint (real HTTP).
async fn gtx_translate(ctx: &Ctx, text: &str, lang: &str) -> Result<String> {
    let out: serde_json::Value = ctx
        .http
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[
            ("client", "gtx"),
            ("sl", "auto"),
            ("tl", lang),
            ("dt", "t"),
            ("q", text),
        ])
        .send()
        .await?
        .json()
        .await?;
    let mut translated = String::new();
    if let Some(segs) = out.get(0).and_then(|v| v.as_array()) {
        for seg in segs {
            if let Some(t) = seg.get(0).and_then(|v| v.as_str()) {
                translated.push_str(t);
            }
        }
    }
    if translated.is_empty() {
        return Err(Error::Other("translation returned no text".to_string()));
    }
    Ok(translated)
}

fn split_lang_text(args: &str) -> (String, Option<String>) {
    let mut it = args.trim().splitn(2, char::is_whitespace);
    let first = it.next().unwrap_or("").to_string();
    let rest = it.next().map(str::to_string);
    (first, rest)
}

async fn translate(ctx: &Ctx, _telegram: bool) -> Result<()> {
    let (lang, text_arg) = split_lang_text(&ctx.args);
    let text = match text_arg.filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => match &ctx.reply_to_text {
            Some(t) => t.clone(),
            None => {
                ctx.reply(&format!(
                    "**Translate**\n\n`.tl <lang> <text>` — translate the given text.\n`.tl <lang>` as reply — translate the replied message.\n`.tl` as reply — translate to English (default).\n\n**Example:** `.tl es Hello World`"
                ))
                .await?;
                return Ok(());
            }
        },
    };
    let lang = if lang.is_empty() {
        "en".to_string()
    } else {
        lang
    };
    match gtx_translate(ctx, &text, &lang).await {
        Ok(tt) => {
            ctx.reply(&format!(
                "**Ⲧʀⲁⲛⲋⳑⲁⲧⲉⲇ**\n\n**Ⲋⲟυʀⲥⲉ**:\n`{text}`\n\n**Ⲧʀⲁⲛⲋⳑⲁⲧⲓⲟⲛ** (`{lang}`):\n`{tt}`"
            ))
            .await?;
        }
        Err(e) => {
            ctx.reply(&format!("`{e}`")).await?;
        }
    }
    Ok(())
}

async fn show_id(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim();
    if !arg.is_empty() {
        let ids = match arg.trim_start_matches('@').parse::<i64>() {
            Ok(id) => id,
            Err(_) => ctx.responder.resolve_user(arg).await?.id,
        };
        ctx.reply(&format!(
            "**Chat ID:**  `{}`\n**User ID:**  `{ids}`",
            ctx.chat_id
        ))
        .await?;
        return Ok(());
    }
    let mut data = format!("**Current Chat ID:**  `{}`", ctx.chat_id);
    if let Some(reply_id) = ctx.reply_to_id {
        // The replied sender's id is not visible on `Ctx`; the reply
        // message id is.
        data.push_str(&format!("\n**Msg ID:**  `{reply_id}`"));
    } else {
        data.push_str(&format!("\n**Msg ID:**  `{}`", ctx.message_id));
    }
    ctx.reply(&data).await?;
    Ok(())
}

async fn bots(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim();
    // NOTE: `resolve_user` only resolves users; a group/channel
    // username argument cannot be resolved to a chat id on the
    // `Responder` surface, so only a numeric chat id (or this chat)
    // is accepted here.
    let (chat, chat_label) = if input.is_empty() {
        (ctx.chat_id, String::new())
    } else {
        match input.trim_start_matches('@').parse::<i64>() {
            Ok(id) => (id, format!(" in **{input}**")),
            Err(_) => match ctx.responder.resolve_user(input).await {
                Ok(u) => (u.id, format!(" in **{input}**")),
                Err(e) => {
                    ctx.reply(&format!("`{e}`")).await?;
                    return Ok(());
                }
            },
        }
    };
    let mut mentions = format!("• **Bots in this Chat{chat_label}**: \n");
    // Page through participants (recent filter) and keep the bots —
    // the `Responder` surface has no bots-only filter.
    let mut count = 0usize;
    let mut offset = 0i32;
    loop {
        let (page, total) = ctx.responder.participants(chat, 200, offset).await?;
        if page.is_empty() {
            break;
        }
        for p in &page {
            if !p.user.bot {
                continue;
            }
            count += 1;
            let mention = format!("[{}](tg://user?id={})", p.user.first_name, p.user.id);
            if p.admin || p.owner {
                mentions.push_str(&format!("\n⚜️ {mention} `{}`", p.user.id));
            } else {
                mentions.push_str(&format!("\n• {mention} `{}`", p.user.id));
            }
        }
        offset += page.len() as i32;
        if offset >= total {
            break;
        }
    }
    if count == 0 {
        mentions.push_str("\n`No bots found in this chat.`");
    }
    ctx.reply(&mentions).await?;
    Ok(())
}

async fn hidden_link(ctx: &Ctx) -> Result<()> {
    let input = ctx.args.trim();
    if input.is_empty() {
        ctx.reply(
            "**Hidden Link**\n\n`.hl <url> <text>` — hide a link behind text.\n`.hl <url>` — hide a link behind an invisible spacer.\n\n**Example:** `.hl https://example.com click me`",
        )
        .await?;
        return Ok(());
    }
    let mut parts = input.splitn(2, char::is_whitespace);
    let url = parts.next().unwrap_or("");
    let text = parts.next().filter(|t| !t.trim().is_empty()).unwrap_or("ㅤㅤㅤㅤㅤㅤㅤ");
    if !url.starts_with("http://") && !url.starts_with("https://") {
        ctx.reply("`Please provide a valid URL starting with http:// or https://`")
            .await?;
        return Ok(());
    }
    ctx.reply(&format!("[{text}]({url})")).await?;
    Ok(())
}

async fn circle(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("**Video Note (Circle)**\n\n`Reply to a gif or audio file only.`")
            .await?;
        return Ok(());
    }
    // TODO(responder-op): download the replied gif/audio, convert with
    // ffmpeg (audio → 512×512 still + audio; gif → webm→mp4), and send as
    // a video note (Python: tools.py:_, `reply.download_media()` +
    // `fast_uploader` + `DocumentAttributeVideo(round_message=True)`).
    Err(Error::Other(
        "download replied media for .circle requires a binary Responder op (Python: tools.py:_)"
            .to_string(),
    ))
}

fn file_emoji(path: &Path) -> &'static str {
    let name = path.to_string_lossy().to_lowercase();
    let ext = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "py" => "🐍",
        "json" => "🔮",
        "sh" | "bat" => "⌨️",
        "mkv" | "mp4" | "avi" | "gif" | "webm" => "🎥",
        "mp3" | "ogg" | "m4a" | "opus" => "🔊",
        "jpg" | "jpeg" | "png" | "webp" | "ico" => "🖼",
        "txt" | "text" | "log" => "📄",
        "apk" | "xapk" => "📲",
        "pdf" | "epub" => "📗",
        "zip" | "rar" => "🗜",
        "exe" | "iso" => "⚙",
        _ => {
            if name.contains('.') {
                "🏷"
            } else {
                "📒"
            }
        }
    }
}

/// Segment matcher: `*` matches any run, `?` matches one char.
fn segment_match(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn glob_collect(base: &Path, segments: &[&str], out: &mut Vec<PathBuf>) {
    let Some((head, rest)) = segments.split_first() else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(base) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !segment_match(head, &name) {
            continue;
        }
        let path = entry.path();
        if rest.is_empty() {
            out.push(path);
        } else if path.is_dir() {
            glob_collect(&path, rest, out);
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = e.metadata() {
                total = total.saturating_add(m.len());
            }
        }
    }
    total
}

async fn list_files(ctx: &Ctx) -> Result<()> {
    let mut pattern = ctx.args.trim().to_string();
    if pattern.is_empty() {
        pattern = "*".to_string();
    } else if pattern.ends_with('/') {
        pattern.push('*');
    } else if !pattern.contains('*') {
        pattern.push_str("/*");
    }
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let mut files = Vec::new();
    if pattern.starts_with('/') {
        glob_collect(Path::new("/"), &segments, &mut files);
    } else {
        glob_collect(Path::new("."), &segments, &mut files);
    }
    if files.is_empty() {
        ctx.reply("`Directory Empty or Incorrect.`").await?;
        return Ok(());
    }
    files.sort();
    let mut folders: Vec<String> = Vec::new();
    let mut allfiles: Vec<String> = Vec::new();
    for f in &files {
        if f.is_dir() {
            folders.push(format!("📂 {}", f.display()));
        } else {
            allfiles.push(format!("{} {}", file_emoji(f), f.display()));
        }
    }
    let mut text = String::new();
    let (mut foc, mut flc) = (0u64, 0u64);
    let (mut fos, mut fls) = (0u64, 0u64);
    for item in folders.iter().chain(allfiles.iter()) {
        let Some(name) = item.splitn(2, ' ').nth(1) else {
            continue;
        };
        let path = Path::new(name);
        let nam = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let emoji = item.split(' ').next().unwrap_or("");
        if path.is_dir() {
            let size = dir_size(path);
            if size > 0 {
                text.push_str(&format!("{emoji} `{nam}`  `{}`\n", humanbytes(size)));
                fos += size;
            } else {
                text.push_str(&format!("{emoji} `{nam}`\n"));
            }
            foc += 1;
        } else {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if size > 0 {
                text.push_str(&format!("{emoji} `{nam}`  `{}`\n", humanbytes(size)));
                fls += size;
            } else {
                text.push_str(&format!("{emoji} `{nam}`\n"));
            }
            flc += 1;
        }
    }
    text.push_str(&format!(
        "\n\n`Folders` :  `{foc}` :   `{}`\n`Files` :       `{flc}` :   `{}`\n`Total` :       `{}` :   `{}`",
        humanbytes(fos),
        humanbytes(fls),
        flc + foc,
        humanbytes(fos + fls)
    ));
    if (flc + foc) > 100 {
        text = text.replace('`', "");
    }
    if text.len() > 4096 {
        let path: PathBuf = std::env::temp_dir().join("output.txt");
        tokio::fs::write(&path, text.as_bytes()).await?;
        ctx.send_file(&path, &format!("`{}`", ctx.text)).await?;
        let _ = tokio::fs::remove_file(&path).await;
        ctx.delete(ctx.message_id).await?;
    } else {
        ctx.reply(&text).await?;
    }
    Ok(())
}

/// Split SangMata's combined name/username history dump (Python
/// `sanga_seperator`).
pub fn sanga_separator(sanga_list: &[String]) -> std::result::Result<(String, String), String> {
    let mut string = String::new();
    for info in sanga_list {
        if let Some(idx) = info.find('\n') {
            string.push_str(&info[idx + 1..]);
        }
    }
    let blank = Regex::new(r"^$\n").map_err(|e| e.to_string())?;
    let string = blank.replace_all(&string, "").into_owned();
    let (names_part, username) = string
        .split_once("Usernames**")
        .ok_or_else(|| "could not split on 'Usernames**'".to_string())?;
    let name = names_part
        .split_once("Names")
        .map(|(_, n)| n.to_string())
        .ok_or_else(|| "could not split on 'Names'".to_string())?;
    Ok((name, username.to_string()))
}

async fn sangmata(ctx: &Ctx) -> Result<()> {
    // captures[1] = "" or "u"; captures[2] = rest.
    let cmd = ctx.captures.get(1).cloned().unwrap_or_default();
    let user_arg = ctx.captures.get(2).cloned().unwrap_or_default();
    let user = if !user_arg.trim().is_empty() {
        user_arg.trim().to_string()
    } else if ctx.reply_to_id.is_some() {
        match ctx.reply_to_sender_id {
            Some(id) => id.to_string(),
            None => {
                ctx.reply("`Reply to a user's message or provide their ID/username to fetch name/username history.`")
                    .await?;
                return Ok(());
            }
        }
    } else {
        ctx.reply("`Reply to a user's message or provide their ID/username to fetch name/username history.`")
            .await?;
        return Ok(());
    };
    let _id = ctx.reply("`Processing...`").await?;
    // A numeric token resolves directly; a username resolves through
    // the `Responder` (the SangMata conversation itself is still a
    // TODO — bot conversations are not on the surface).
    let user_id = match user.trim_start_matches('@').parse::<i64>() {
        Ok(id) => id,
        Err(_) => match ctx
            .responder
            .resolve_user(user.trim_start_matches('@'))
            .await
        {
            Ok(u) => u.id,
            Err(e) => {
                return Err(Error::Other(format!(
                    "resolve user '{user}' requires a binary Responder op (Python: tools.py:sangmata): {e}"
                )));
            }
        },
    };
    let _ = (cmd, user_id);
    // TODO(responder-op): conversation with @SangMata_beta_bot — send the
    // user id, collect its replies, parse with `sanga_separator`
    // (Python: tools.py:sangmata).
    Err(Error::Other(
        "SangMata conversation requires a binary Responder op (Python: tools.py:sangmata)"
            .to_string(),
    ))
}

async fn shorturl(ctx: &Ctx) -> Result<()> {
    let mut input_url = ctx.args.trim().to_string();
    if input_url.is_empty() {
        if let Some(rep) = &ctx.reply_to_text {
            input_url = rep.trim().to_string();
        } else {
            ctx.reply(
                "**Short / Expand URL**\n\n`.shorturl <url>` — shorten a long URL via TinyURL.\n`.shorturl https://tinyurl.com/xxxx` — expand a TinyURL link.\n`.shorturl` as reply — use the replied message's URL.\n\n**Example:** `.shorturl https://example.com/very/long/link`",
            )
            .await?;
            return Ok(());
        }
    }
    if input_url.is_empty()
        || !input_url.starts_with("http://") && !input_url.starts_with("https://")
    {
        ctx.reply("`Please provide a valid URL starting with http:// or https://`")
            .await?;
        return Ok(());
    }
    if input_url.to_lowercase().starts_with("https://tinyurl.com/") {
        // Expanding = following the redirect to the destination.
        let resp = ctx.http.get(&input_url).send().await?;
        if !resp.status().is_success() {
            ctx.reply(&format!("`Failed to expand URL (HTTP {}).`", resp.status()))
                .await?;
            return Ok(());
        }
        let expanded = resp.url().to_string();
        ctx.reply(&format!(
            "**Expanded URL**\n**Given Link** ➠ **{input_url}**\n**Expanded Link** ➠ **{expanded}**"
        ))
        .await?;
    } else {
        let short = ctx
            .http
            .get("https://tinyurl.com/api-create.php")
            .query(&[("url", input_url.as_str())])
            .send()
            .await?
            .text()
            .await?;
        let short = short.trim().to_string();
        if !short.starts_with("http") {
            ctx.reply(&format!("`An error occurred: TinyURL returned {short}`"))
                .await?;
            return Ok(());
        }
        let output = format!(
            "**Shortened URL**\n**Given Link** ➠ **{input_url}**\n**Shortened Link** ➠ **{short}**"
        );
        if ctx.reply_to_id.is_some() {
            ctx.delete(ctx.message_id).await?;
            ctx.reply(&output).await?;
        } else {
            ctx.reply(&output).await?;
        }
    }
    Ok(())
}
