//! Inline-query results for the assistant bot.
//!
//! Port of `assistant/inlinestuff.py`.
//!
//! Patterns (all owner-only, all via `@botusername <pattern> ...`):
//! `ofox <codename>` (OrangeFox releases), `fl2lnk <chat>:<msg>`
//! (file→link service picker), `repo`, `go <query>` (Google),
//! `mods <app>` (mod APK search), `app <query>` (Play Store),
//! `run <lang> <code>` (Piston eval), `fdroid <query>`,
//! `twitter <query>`, `saavn` (defunct — notice only), `tl <name>`
//! (TL object search).
//!
//! Contract limits: `Ctx` has no inline-query builder or answer
//! channel, so every handler returns structured `InlineResult`
//! values (title / description / HTML text / thumb / URL /
//! keyboard) plus the `switch_pm` prompt, for the binary crate to
//! answer the query with. Nothing here is faked: each search is a
//! real HTTP call to the same endpoint Python uses. The `tl`
//! search needs Telethon's TL-object schema, which lives in
//! `grammers-tl-types` — not a dependency of this crate — so a
//! `tl` query returns an error naming that schema instead of
//! invented results; the empty-query help card is fully ported.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use ultroid_core::Ctx;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::{escape_html, humanbytes};
use ultroid_core::tg::{ButtonAction, InlineButton};

use crate::cbs::{SmallRng, cb, fmtmsg};

/// One inline result (the data an inline-query answer needs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineResult {
    pub title: String,
    pub description: String,
    pub text: String,
    pub thumb_url: Option<String>,
    pub url: Option<String>,
    pub buttons: Vec<Vec<InlineButton>>,
}

/// The answer envelope: results + optional switch-pm prompt.
#[derive(Debug, Clone, Default)]
pub struct InlineAnswer {
    pub results: Vec<InlineResult>,
    pub switch_pm: Option<String>,
    pub switch_pm_param: Option<String>,
}

fn switch_inline_btn(text: &str, query: &str, same_peer: bool) -> InlineButton {
    InlineButton {
        text: text.to_string(),
        action: if same_peer {
            ButtonAction::SwitchInline(query.to_string())
        } else {
            ButtonAction::SwitchInlineElsewhere(query.to_string())
        },
        style: ultroid_core::tg::ButtonStyle::Default,
    }
}

fn fmt_utc_date(ts: i64) -> String {
    // `%Y-%m-%d` in UTC (Python `datetime.utcfromtimestamp(...).strftime`).
    crate::cbs::fmt_utc(ts)
        .split(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Base64 decode (the `mods` handler stores its Google API keys
/// base64-encoded, exactly as in Python; no `base64` crate in this
/// crate's dependencies).
pub fn b64decode(input: &str) -> Result<String> {
    let mut table = [255u8; 256];
    for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[*c as usize] = i as u8;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in input.as_bytes() {
        if b == b'=' {
            break;
        }
        let v = table[b as usize];
        if v == 255 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    String::from_utf8(out).map_err(|e| Error::Other(format!("base64 decode: {e}")))
}

const OFOX_THUMB: &str = "https://graph.org/file/231f0049fcd722824f13b.jpg";
const GUGIRL: &str = "https://graph.org/file/0df54ae4541abca96aa11.jpg";
const ULTPIC: &str = "https://graph.org/file/4136aa1650bc9d4109cc5.jpg";

/// `ofox <codename>` — OrangeFox recovery search (real API:
/// `api.orangefox.download/v3/devices/get` + `releases`).
pub async fn inline_ofox(ctx: &Ctx, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        let text = fmtmsg("OFoxRecovery", &[], "You didn't search anything", "");
        return Ok(InlineAnswer {
            results: vec![InlineResult {
                title: "Enter Device Codename".into(),
                description: String::new(),
                text,
                thumb_url: Some(OFOX_THUMB.into()),
                url: None,
                buttons: vec![vec![switch_inline_btn("Search Again", "ofox ", true)]],
            }],
            switch_pm: None,
            switch_pm_param: None,
        });
    }
    let base = "https://api.orangefox.download/v3/";
    let device: serde_json::Value = ctx
        .http
        .get(format!("{base}devices/get?codename={query}"))
        .send()
        .await?
        .json()
        .await?;
    if device.get("detail").is_some() {
        return Ok(InlineAnswer {
            results: vec![],
            switch_pm: Some("OrangeFox Recovery Search.".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let releases: serde_json::Value = ctx
        .http
        .get(format!("{base}releases?codename={query}"))
        .send()
        .await?
        .json()
        .await?;
    let fullname = device
        .get("full_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let codename = device
        .get("codename")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let maintainer = device
        .get("maintainer")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let link = format!("https://orangefox.download/device/{codename}");
    let mut results = Vec::new();
    let empty = Vec::new();
    for data in releases
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
    {
        let release = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let version = data.get("version").and_then(|v| v.as_str()).unwrap_or("");
        let size = humanbytes(data.get("size").and_then(|v| v.as_u64()).unwrap_or(0));
        let date = fmt_utc_date(data.get("date").and_then(|v| v.as_i64()).unwrap_or(0));
        let text = fmtmsg(
            &format!("OrangeFox Recovery For {fullname}"),
            &[
                ("Full Name", escape_html(fullname)),
                ("Codename", escape_html(codename)),
                ("Maintainer", escape_html(maintainer)),
                ("Build Type", escape_html(release)),
                ("Version", escape_html(version)),
                ("Size", size),
                ("Build Date", date.clone()),
            ],
            "",
            "",
        );
        results.push(InlineResult {
            title: fullname.to_string(),
            description: format!("{version}\n{date}"),
            text,
            thumb_url: Some(OFOX_THUMB.into()),
            url: None,
            buttons: vec![vec![
                InlineButton::url("Download", &link),
                switch_inline_btn("Search Again", "ofox ", true),
            ]],
        });
    }
    Ok(InlineAnswer {
        results,
        switch_pm: Some("OrangeFox Recovery Search.".into()),
        switch_pm_param: Some("start".into()),
    })
}

/// `repo` — the Ultroid repo card.
pub fn inline_repo() -> InlineAnswer {
    let text = fmtmsg("Ultroid Userbot", &[], "  **ULTROID USERBOT**  ", "");
    InlineAnswer {
        results: vec![InlineResult {
            title: "Ultroid Userbot".into(),
            description: "Userbot | Telethon".into(),
            text,
            thumb_url: Some(ULTPIC.into()),
            url: None,
            buttons: vec![vec![
                InlineButton::url("  Repo  ", "https://github.com/TeamUltroid/Ultroid"),
                InlineButton::url("  Support  ", "t.me/UltroidSupportChat"),
            ]],
        }],
        switch_pm: Some("Ultroid Repo.".into()),
        switch_pm_param: Some("start".into()),
    }
}

/// `go <query>` — Google search by scraping `google.com/search`
/// (Python `fns.google_search`).
pub async fn inline_google(ctx: &Ctx, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        return Ok(InlineAnswer {
            results: vec![],
            switch_pm: Some("Google Search. Enter a query!".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let url = format!("https://google.com/search?q={}", query.replace(' ', "+"));
    let html = ctx
        .http
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .send()
        .await?
        .text()
        .await?;
    let mut results = Vec::new();
    // Python: every <a href="...url=..."> containing a <div>.
    let mut rest = html.as_str();
    while let Some(a) = rest.find("<a href=\"/url?q=") {
        let seg = &rest[a..];
        let Some(href_end) = seg.find('"') else { break };
        let _ = href_end;
        let href = &seg["<a href=\"/url?q=".len()..seg.find('"').unwrap_or(seg.len())];
        let link = href.split('&').next().unwrap_or(href);
        let after = &seg[seg.find('>').map(|i| i + 1).unwrap_or(0)..];
        let title = after
            .find("</a>")
            .map(|e| strip_tags(&after[..e]))
            .unwrap_or_default();
        if !title.is_empty() && !link.is_empty() {
            let text =
                format!("**Google Search**\n\n**  Title  **\n{title}\n\n**  Description  **\n");
            results.push(InlineResult {
                title: title.clone(),
                description: String::new(),
                text,
                thumb_url: Some(GUGIRL.into()),
                url: Some(link.to_string()),
                buttons: vec![
                    vec![InlineButton::url("Link", link)],
                    vec![
                        switch_inline_btn("Search Again", "go ", true),
                        switch_inline_btn("Share", &format!("go {query}"), false),
                    ],
                ],
            });
        }
        rest = &rest[a + 1..];
        if results.len() >= 10 {
            break;
        }
    }
    Ok(InlineAnswer {
        results,
        switch_pm: Some("Google Search.".into()),
        switch_pm_param: Some("start".into()),
    })
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Google Custom Search API keys for `mods` (base64 in the source).
pub const MODS_API_KEYS_B64: &[&str] = &[
    "QUl6YVN5QXlEQnNZM1dSdEI1WVBDNmFCX3c4SkF5NlpkWE5jNkZV",
    "QUl6YVN5QkYwenhMbFlsUE1wOXh3TVFxVktDUVJxOERnZHJMWHNn",
    "QUl6YVN5RGRPS253blB3VklRX2xiSDVzWUU0Rm9YakFLSVFWMERR",
];

/// `mods <app>` — mod APK search via Google Custom Search.
pub async fn inline_mods(ctx: &Ctx, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        return Ok(InlineAnswer {
            results: vec![],
            switch_pm: Some("Mod Apps Search. Enter app name!".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let key = {
        let mut rng = SmallRng::new(crate::cbs::now_unix() as u64);
        let pick = MODS_API_KEYS_B64[(rng.below(MODS_API_KEYS_B64.len() as u64)) as usize];
        b64decode(pick)?
    };
    let url = format!(
        "https://www.googleapis.com/customsearch/v1?key={key}&cx=25b3b50edb928435b&q={query}&start=1"
    );
    let data: serde_json::Value = ctx.http.get(&url).send().await?.json().await?;
    let mut results = Vec::new();
    let empty = Vec::new();
    for a in data
        .get("items")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
    {
        let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let desc = a.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
        let link = a.get("link").and_then(|v| v.as_str()).unwrap_or("");
        let text = format!("**  Title  ** {title}\n\n**Description** {desc}");
        results.push(InlineResult {
            title: title.to_string(),
            description: desc.to_string(),
            text,
            thumb_url: None,
            url: None,
            buttons: vec![
                vec![InlineButton::url("Download", link)],
                vec![
                    switch_inline_btn("More Mods", "mods ", true),
                    switch_inline_btn("Share", &format!("mods {query}"), false),
                ],
            ],
        });
    }
    Ok(InlineAnswer {
        results,
        switch_pm: Some("Search Mod Applications.".into()),
        switch_pm_param: Some("start".into()),
    })
}

/// Play Store search cache (Python `APP_CACHE` / `RECENTS`).
fn app_cache() -> &'static Mutex<HashMap<String, Vec<InlineResult>>> {
    static A: OnceLock<Mutex<HashMap<String, Vec<InlineResult>>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

fn recents() -> &'static Mutex<HashMap<i64, Vec<String>>> {
    static R: OnceLock<Mutex<HashMap<i64, Vec<String>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `app <query>` — Play Store search via `googleplay.onrender.com`.
pub async fn inline_app(ctx: &Ctx, sender_id: i64, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        let mut res = Vec::new();
        let rec = recents().lock().await;
        if let Some(keys) = rec.get(&sender_id) {
            let cache = app_cache().lock().await;
            for k in keys {
                if let Some(v) = cache.get(k) {
                    res.extend(v.iter().take(1).cloned());
                }
            }
        }
        return Ok(InlineAnswer {
            results: res,
            switch_pm: Some(ctx.get_str("instu_2")),
            switch_pm_param: Some("start".into()),
        });
    }
    let key = query.to_ascii_lowercase();
    if let Some(cached) = app_cache().lock().await.get(&key).cloned() {
        return Ok(InlineAnswer {
            results: cached,
            switch_pm: Some("Application Searcher.".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let url = format!(
        "https://googleplay.onrender.com/api/apps?q={}",
        key.replace(' ', "+")
    );
    let data: serde_json::Value = ctx.http.get(&url).send().await?.json().await?;
    let mut results = Vec::new();
    let empty = Vec::new();
    for z in data
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty)
        .iter()
        .take(50)
    {
        let app_id = z.get("appId").and_then(|v| v.as_str()).unwrap_or("");
        let name = z.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let summary = z.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let desc: String = summary.chars().take(300).collect::<String>() + "...";
        let dev = z
            .get("developer")
            .and_then(|v| v.get("devId"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let icon = z.get("icon").and_then(|v| v.as_str()).unwrap_or("");
        let store = format!("https://play.google.com/store/apps/details?id={app_id}");
        let text = fmtmsg(
            name,
            &[("Developer", escape_html(dev))],
            &strip_tags(&desc),
            "",
        );
        results.push(InlineResult {
            title: name.to_string(),
            description: dev.to_string(),
            text,
            thumb_url: Some(icon.to_string()),
            url: None,
            buttons: vec![
                vec![InlineButton::url("Link", &store)],
                vec![
                    switch_inline_btn("More Apps", "app ", true),
                    switch_inline_btn("Share", &format!("app {key}"), false),
                ],
            ],
        });
    }
    app_cache()
        .lock()
        .await
        .insert(key.clone(), results.clone());
    recents()
        .lock()
        .await
        .entry(sender_id)
        .or_default()
        .push(key);
    Ok(InlineAnswer {
        results,
        switch_pm: Some("Application Searcher.".into()),
        switch_pm_param: Some("start".into()),
    })
}

/// Piston runtimes cache (Python `PISTON_LANGS`).
fn piston_langs() -> &'static Mutex<HashMap<String, String>> {
    static P: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

const PISTON_URI: &str = "https://emkc.org/api/v2/piston/";

fn piston_bad_query(bot_username: &str, title: &str, desc: &str) -> InlineResult {
    let text = fmtmsg(
        title,
        &[],
        &format!(
            "Usage: [Language] [code]\n`@{bot_username} run python print(\"hello world\")`\nLanguage List: https://graph.org/Ultroid-09-01-6"
        ),
        "",
    );
    InlineResult {
        title: title.to_string(),
        description: desc.to_string(),
        text,
        thumb_url: Some("https://graph.org/file/e33c57fc5f1044547e4d8.jpg".to_string()),
        url: None,
        buttons: vec![],
    }
}

/// `run <lang> <code>` — Piston code execution.
pub async fn inline_run(ctx: &Ctx, bot_username: &str, query: &str) -> Result<InlineAnswer> {
    let mut parts = query.splitn(3, ' ');
    let lang = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    if lang.is_empty() || code.is_empty() {
        return Ok(InlineAnswer {
            results: vec![piston_bad_query(
                bot_username,
                "Bad Query",
                "Usage: [Language] [code]",
            )],
            switch_pm: None,
            switch_pm_param: None,
        });
    }
    if piston_langs().lock().await.is_empty() {
        let runtimes: serde_json::Value = ctx
            .http
            .get(format!("{PISTON_URI}runtimes"))
            .send()
            .await?
            .json()
            .await?;
        let mut map = piston_langs().lock().await;
        if let Some(arr) = runtimes.as_array() {
            for r in arr {
                if let (Some(l), Some(v)) = (
                    r.get("language").and_then(|v| v.as_str()),
                    r.get("version").and_then(|v| v.as_str()),
                ) {
                    map.insert(l.to_string(), v.to_string());
                }
            }
        }
    }
    let Some(version) = piston_langs().lock().await.get(lang).cloned() else {
        return Ok(InlineAnswer {
            results: vec![piston_bad_query(
                bot_username,
                "Unsupported Language",
                "Usage: [Language] [code]",
            )],
            switch_pm: None,
            switch_pm_param: None,
        });
    };
    let output: serde_json::Value = ctx
        .http
        .post(format!("{PISTON_URI}execute"))
        .json(&serde_json::json!({
            "language": lang,
            "version": version,
            "files": [{"content": code}],
        }))
        .send()
        .await?
        .json()
        .await?;
    let mut out = output
        .get("run")
        .and_then(|r| r.get("output"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if out.is_empty() {
        out = ctx.get_str("instu_4");
    }
    if out.chars().count() > 3000 {
        out = format!("{}...", out.chars().take(3000).collect::<String>());
    }
    let text = fmtmsg(
        "Result",
        &[("Language", escape_html(lang))],
        &format!("```{code}```\n\nOutput:\n```{out}```"),
        "",
    );
    Ok(InlineAnswer {
        results: vec![InlineResult {
            title: "Result".into(),
            description: out,
            text,
            thumb_url: Some("https://graph.org/file/871ee4a481f58117dccc4.jpg".to_string()),
            url: None,
            buttons: vec![vec![switch_inline_btn("Fork", query, true)]],
        }],
        switch_pm: Some("  Piston  ".into()),
        switch_pm_param: Some("start".into()),
    })
}

/// F-Droid search cache (Python `FDROID_`).
fn fdroid_cache() -> &'static Mutex<HashMap<String, Vec<InlineResult>>> {
    static F: OnceLock<Mutex<HashMap<String, Vec<InlineResult>>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `fdroid <query>` — scrape `search.f-droid.org` package headers.
pub async fn inline_fdroid(ctx: &Ctx, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        return Ok(InlineAnswer {
            results: vec![],
            switch_pm: Some("Enter Query to Search".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let key = query.to_ascii_lowercase();
    if let Some(cached) = fdroid_cache().lock().await.get(&key).cloned() {
        return Ok(InlineAnswer {
            results: cached,
            switch_pm: Some(format!("  Results for {key}")),
            switch_pm_param: Some("start".into()),
        });
    }
    let link = format!("https://search.f-droid.org/?q={}", key.replace(' ', "+"));
    let html = ctx.http.get(&link).send().await?.text().await?;
    let mut results = Vec::new();
    // Each result is an <a class="package-header" href="..."> block.
    let mut rest = html.as_str();
    while let Some(a) = rest.find("package-header") {
        let seg = &rest[a..];
        let Some(end) = seg.find("</a>") else { break };
        let block = &seg[..end];
        let href = block
            .split("href=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        let pkg = href.split("packages/").nth(1).unwrap_or(href);
        let image = block
            .split("src=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        let image = if image.is_empty() || image.ends_with('/') {
            "https://graph.org/file/a8dd4a92c5a53a89d0eff.jpg".to_string()
        } else {
            image.to_string()
        };
        let title = block
            .split("package-name\">")
            .nth(1)
            .and_then(|s| s.split('<').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let desc = block
            .split("package-summary\">")
            .nth(1)
            .and_then(|s| s.split('<').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let license = block
            .split("package-license\">")
            .nth(1)
            .and_then(|s| s.split('<').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let text = fmtmsg(&title, &[("License", escape_html(&license))], &desc, "");
        let dl_data = cb(&["assistant", "fdroid", pkg]).unwrap_or_default();
        results.push(InlineResult {
            title: title.clone(),
            description: desc,
            text,
            thumb_url: Some(image.clone()),
            url: None,
            buttons: vec![vec![
                InlineButton::data("  Download  ", dl_data.into_bytes()),
                switch_inline_btn("  Share  ", &format!("fdroid {key}"), false),
            ]],
        });
        rest = &rest[a + end..];
        if results.len() >= 10 {
            break;
        }
    }
    let msg = if results.is_empty() {
        "No Results Found".to_string()
    } else {
        format!("Showing {} Results!", results.len())
    };
    fdroid_cache().lock().await.insert(key, results.clone());
    Ok(InlineAnswer {
        results,
        switch_pm: Some(msg),
        switch_pm_param: Some("start".into()),
    })
}

/// Twitter bearer tokens (shared, from the Python source).
pub const TWITTER_BEARERS: &[&str] = &[
    "AAAAAAAAAAAAAAAAAAAAALIKKgEAAAAA1DRuS%2BI7ZRKiagD6KHYmreaXomo%3DP5Vaje4UTtEkODg0fX7nCh5laSrchhtLxeyEqxXpv0w9ZKspLD",
    "AAAAAAAAAAAAAAAAAAAAAL5iUAEAAAAAmo6FYRjqdKlI3cNziIm%2BHUQB9Xs%3DS31pj0mxARMTOk2g9dvQ1yP9wknvY4FPBPUlE00smJcncw4dPR",
    "AAAAAAAAAAAAAAAAAAAAAN6sVgEAAAAAMMjMMWrwgGyv7YQOWN%2FSAsO5SGM%3Dg8MG9Jq93Rlllaok6eht7HvRCruN4Vpzp4NaVsZaaHHWSTzKI8",
];

/// `twitter <query>` — Twitter user search (API 1.1).
pub async fn inline_twitter(ctx: &Ctx, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        return Ok(InlineAnswer {
            results: vec![],
            switch_pm: Some("Enter Query to Search".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    let key = query.to_ascii_lowercase();
    let bearer = {
        let mut rng = SmallRng::new(crate::cbs::now_unix() as u64);
        TWITTER_BEARERS[(rng.below(TWITTER_BEARERS.len() as u64)) as usize]
    };
    let users: serde_json::Value = ctx
        .http
        .get(format!(
            "https://api.twitter.com/1.1/users/search.json?q={key}"
        ))
        .header("Authorization", format!("bearer {bearer}"))
        .send()
        .await?
        .json()
        .await?;
    let mut results = Vec::new();
    let empty = Vec::new();
    for user in users.as_array().unwrap_or(&empty) {
        let name = user.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let screen = user
            .get("screen_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let followers = user
            .get("followers_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let following = user
            .get("friends_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let desc = user
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let thumb = user
            .get("profile_image_url_https")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pro = format!("https://twitter.com/{screen}");
        let text = fmtmsg(
            name,
            &[
                ("Username", format!("@{screen}")),
                ("Followers", followers.to_string()),
                ("Following", following.to_string()),
            ],
            desc,
            "",
        );
        results.push(InlineResult {
            title: name.to_string(),
            description: desc.to_string(),
            text,
            thumb_url: Some(thumb.to_string()),
            url: Some(pro),
            buttons: vec![],
        });
    }
    let swi = if results.is_empty() {
        "No User Found :(".to_string()
    } else {
        format!(" Showing {} Results!", results.len())
    };
    Ok(InlineAnswer {
        results,
        switch_pm: Some(swi),
        switch_pm_param: Some("start".into()),
    })
}

/// `saavn` — Saavn is defunct; the inline handler answers a notice
/// (Python `savn_s`).
pub fn inline_saavn() -> InlineAnswer {
    let text = fmtmsg(
        " Saavn Search",
        &[],
        "This inline search is no longer available.",
        "",
    );
    InlineAnswer {
        results: vec![InlineResult {
            title: "Saavn is no longer available".into(),
            description: String::new(),
            text,
            thumb_url: None,
            url: None,
            buttons: vec![],
        }],
        switch_pm: Some(" Saavn is no longer available".into()),
        switch_pm_param: Some("start".into()),
    }
}

/// `tl <name>` — TL object search. The empty-query help card is
/// ported; a query needs the TL schema (see module docs).
pub fn inline_tl(_bot_username: &str, query: &str) -> Result<InlineAnswer> {
    if query.trim().is_empty() {
        let text = format!(
            "**Telegram TlObjects Searcher.**\n__(Don't use if you don't know what it is!)__\n\n  Example Usage\n@{} tl GetFullUserRequest",
            _bot_username
        );
        return Ok(InlineAnswer {
            results: vec![InlineResult {
                title: "How to Use?".into(),
                description: "Tl Searcher by Ultroid".into(),
                text,
                thumb_url: None,
                url: Some("https://t.me/TeamUltroid".into()),
                buttons: vec![],
            }],
            switch_pm: Some("Tl Search ".into()),
            switch_pm_param: Some("start".into()),
        });
    }
    Err(Error::Other(
        "tl search needs Telethon's tlobjects schema (all TL functions/types with signatures); the equivalent grammers-tl-types schema is not a dependency of this crate, so no TL results can be produced here".to_string(),
    ))
}

/// The inline-plugin help map (Python `InlinePlugin.update`).
pub const INLINE_PLUGINS: &[(&str, &str)] = &[
    ("Play Store Apps", "app telegram"),
    ("Modded Apps", "mods minecraft"),
    ("Search On Google", "go TeamUltroid"),
    ("WhiSper", "wspr @username Hello"),
    ("YouTube Downloader", "yt Ed Sheeran Perfect"),
    ("Piston Eval", "run javascript console.log('Hello Ultroid')"),
    ("OrangeFox", "ofox beryllium"),
    ("Twitter User", "twitter theultroid"),
    ("Fdroid Search", "fdroid telegram"),
    ("Saavn search", "saavn"),
    ("Tl Search", "tl"),
];

/// Route one inline query (`pattern` is the first word, `query` the
/// rest) to its handler. Returns `None` for unknown patterns.
pub async fn handle_inline(
    ctx: &Ctx,
    pattern: &str,
    query: &str,
    bot_username: &str,
) -> Result<Option<InlineAnswer>> {
    match pattern {
        "ofox" => Ok(Some(inline_ofox(ctx, query).await?)),
        "repo" => Ok(Some(inline_repo())),
        "go" => Ok(Some(inline_google(ctx, query).await?)),
        "mods" => Ok(Some(inline_mods(ctx, query).await?)),
        "app" => Ok(Some(inline_app(ctx, ctx.sender_id, query).await?)),
        "run" => Ok(Some(inline_run(ctx, bot_username, query).await?)),
        "fdroid" => Ok(Some(inline_fdroid(ctx, query).await?)),
        "twitter" => Ok(Some(inline_twitter(ctx, query).await?)),
        "saavn" => Ok(Some(inline_saavn())),
        "tl" => Ok(Some(inline_tl(bot_username, query)?)),
        _ => Ok(None),
    }
}

// ── fl2lnk (file → link) ─────────────────────────────────────────────────

/// Upload services offered by the picker (Python `MENU_SERVICES`).
pub const MENU_SERVICES: &[&str] = &[
    "catbox",
    "litterbox",
    "file.io",
    "0x0.st",
    "transfer",
    "uguu",
    "filebin",
    "siasky",
];

/// Display labels (Python `SERVICE_LABELS`).
pub fn service_label(key: &str) -> &str {
    match key {
        "transfer" => "transfer.sh",
        other => other,
    }
}

/// The webupload file cache (Python `_webupload_cache`): the binary
/// registers a downloaded file path per `(chat_id, message_id)`.
fn webupload_cache() -> &'static Mutex<HashMap<(i64, i64), String>> {
    static W: OnceLock<Mutex<HashMap<(i64, i64), String>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn register_webupload_file(chat_id: i64, msg_id: i64, path: &str) {
    webupload_cache()
        .lock()
        .await
        .insert((chat_id, msg_id), path.to_string());
}

/// One upload result (Python `UploadResult`).
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub url: String,
    pub delete_url: Option<String>,
    pub delete_note: Option<String>,
    pub expires: Option<String>,
}

fn multipart_body(
    boundary: &str,
    fields: &[(&str, &str)],
    file_field: &str,
    filename: &str,
    file_bytes: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Upload a cached file to one service (Python `fns.webupload_result`
/// → `upload_to`). Multipart bodies are built by hand: the reqwest
/// build in this workspace has no `multipart` feature.
pub async fn webupload_result(
    ctx: &Ctx,
    chat_id: i64,
    msg_id: i64,
    host: &str,
) -> Result<UploadResult> {
    let Some(path) = webupload_cache()
        .lock()
        .await
        .get(&(chat_id, msg_id))
        .cloned()
    else {
        return Err(Error::Other("File not found in cache.".to_string()));
    };
    let bytes = tokio::fs::read(&path).await?;
    let filename = path.rsplit('/').next().unwrap_or("file").to_string();
    let boundary = format!("----ultroid{}", crate::cbs::now_unix());
    let ct = format!("multipart/form-data; boundary={boundary}");
    let text = match host {
        "catbox" | "litterbox" => {
            let time = if host == "litterbox" { "1h" } else { "" };
            let mut fields: Vec<(&str, &str)> = vec![("reqtype", "fileupload")];
            if host == "litterbox" {
                fields.push(("time", time));
            }
            let body = multipart_body(&boundary, &fields, "fileToUpload", &filename, &bytes);
            let resp = ctx
                .http
                .post("https://catbox.moe/user/api.php")
                .header("Content-Type", ct)
                .body(body)
                .send()
                .await?
                .text()
                .await?;
            let base = "https://files.catbox.moe/";
            let url = resp.trim().to_string();
            if url.starts_with("http") {
                url
            } else {
                format!("{base}{url}")
            }
        }
        "file.io" => {
            let body = multipart_body(&boundary, &[], "file", &filename, &bytes);
            let resp: serde_json::Value = ctx
                .http
                .post("https://file.io/")
                .header("Content-Type", ct)
                .body(body)
                .send()
                .await?
                .json()
                .await?;
            resp.get("link")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Other("file.io upload failed".to_string()))?
                .to_string()
        }
        "0x0.st" => {
            let body = multipart_body(&boundary, &[], "file", &filename, &bytes);
            ctx.http
                .post("https://0x0.st")
                .header("Content-Type", ct)
                .body(body)
                .send()
                .await?
                .text()
                .await?
                .trim()
                .to_string()
        }
        "uguu" => {
            let body = multipart_body(&boundary, &[], "files[]", &filename, &bytes);
            let resp: serde_json::Value = ctx
                .http
                .post("https://uguu.se/upload.php")
                .header("Content-Type", ct)
                .body(body)
                .send()
                .await?
                .json()
                .await?;
            resp.get("files")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|f| f.get("url"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Other("uguu upload failed".to_string()))?
                .to_string()
        }
        "transfer" => ctx
            .http
            .put(format!("https://transfer.sh/{filename}"))
            .body(bytes)
            .send()
            .await?
            .text()
            .await?
            .trim()
            .to_string(),
        "filebin" => {
            let bin = format!("{:x}", crate::cbs::now_unix());
            let resp: serde_json::Value = ctx
                .http
                .put(format!("https://filebin.net/{bin}/{filename}"))
                .body(bytes)
                .send()
                .await?
                .json()
                .await?;
            resp.get("url")
                .and_then(|v| v.as_str())
                .map(|u| format!("https://filebin.net{u}"))
                .unwrap_or_else(|| format!("https://filebin.net/{bin}/{filename}"))
        }
        "siasky" => {
            let resp: serde_json::Value = ctx
                .http
                .post("https://siasky.net/skynet/skyfile")
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await?
                .json()
                .await?;
            let skylink = resp
                .get("skylink")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Other("siasky upload failed".to_string()))?;
            format!("https://siasky.net/{skylink}")
        }
        other => {
            return Err(Error::Other(format!("Unknown upload service: {other}")));
        }
    };
    Ok(UploadResult {
        url: text,
        delete_url: None,
        delete_note: None,
        expires: None,
    })
}

/// `fl2lnk <chat_id>:<msg_id>` — the service-picker card.
pub async fn fl2lnk_card(ctx: &Ctx, chat_id: i64, msg_id: i64) -> Result<Option<InlineAnswer>> {
    let Some(filename) = webupload_cache()
        .lock()
        .await
        .get(&(chat_id, msg_id))
        .cloned()
    else {
        ctx.reply("File not found or invalid chat ID/message ID.")
            .await?;
        return Ok(None);
    };
    let disp = filename.rsplit('/').next().unwrap_or(&filename).to_string();
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let mut row: Vec<InlineButton> = Vec::new();
    for key in MENU_SERVICES {
        let data = cb(&[
            "assistant",
            "fl2lnk",
            key,
            &chat_id.to_string(),
            &msg_id.to_string(),
        ])
        .unwrap_or_default();
        row.push(InlineButton::data(
            format!(" {}", service_label(key)),
            data.into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let text = fmtmsg(" Upload File", &[], &disp, "");
    Ok(Some(InlineAnswer {
        results: vec![InlineResult {
            title: format!("Upload {disp}"),
            description: String::new(),
            text,
            thumb_url: None,
            url: None,
            buttons: rows,
        }],
        switch_pm: Some("File to Link.".into()),
        switch_pm_param: Some("start".into()),
    }))
}

/// `assistant:fl2lnk:<host>:<chat>:<msg>` — upload to the service.
pub async fn fl_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    let Some(tail) = crate::cbs::cb_tail(data, &["assistant", "fl2lnk"])? else {
        return Ok(false);
    };
    let parts: Vec<&str> = tail.split(':').collect();
    if parts.len() != 3 {
        ctx.reply("Invalid callback data format.").await?;
        return Ok(true);
    }
    let (host, chat_id, msg_id) = (
        parts[0],
        parts[1].parse::<i64>().unwrap_or(0),
        parts[2].parse::<i64>().unwrap_or(0),
    );
    let Some(filename) = webupload_cache()
        .lock()
        .await
        .get(&(chat_id, msg_id))
        .cloned()
    else {
        ctx.reply("File not found or invalid chat ID/message ID.")
            .await?;
        return Ok(true);
    };
    let disp = filename.rsplit('/').next().unwrap_or(&filename).to_string();
    let label = service_label(host);
    ctx.reply(&format!(" Uploading {disp} on {label}..."))
        .await?;
    match webupload_result(ctx, chat_id, msg_id, host).await {
        Ok(result) => {
            let mut text = format!("**Uploaded** {disp} **on** {label}.");
            if let Some(d) = &result.delete_url {
                text.push_str(&format!("\n Delete: {d}"));
                if let Some(n) = &result.delete_note {
                    text.push_str(&format!(" ({n})"));
                }
            }
            if let Some(e) = &result.expires {
                text.push_str(&format!("\n Expires: {e}"));
            }
            ctx.reply(&format!("{text}\n{}", result.url)).await?;
            let _ = tokio::fs::remove_file(format!("resources/downloads/{disp}")).await;
        }
        Err(e) => {
            ctx.reply(&format!("Failed to upload {disp} on {label}. Error: {e}"))
                .await?;
        }
    }
    Ok(true)
}
