//! Port of `plugins/search.py` — web search helpers.
//!
//! `.github <username>` — GitHub user API (real HTTP).
//! `.google <query>` — web search via DuckDuckGo HTML (real HTTP);
//! the Python version scrapes Google, which blocks automated fetches,
//! so the Rust port uses the DDG HTML endpoint and extracts the same
//! title/link/description triples.
//! `.img <query> [; n]` — SerpAPI image search; needs the `SERPAPI` DB
//! key, else errors naming it (real HTTP; images are downloaded to a
//! temp file and sent with `send_file`).
//! `.reverse` (reply to an image) — reverse image search; needs the
//! replied media downloaded first (see the `TODO(responder-op)` below).

use crate::util::{db_get, def, reg_add};
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;
use ultroid_core::{Ctx, Error, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "github",
            "github (.*)",
            "GitHub profile info: `.github <username>`.",
        ),
        |ctx| Box::pin(github(ctx)),
    );
    reg_add(
        registry,
        def(
            "google",
            "google( (.*)|$)",
            "Google-style web search: `.google <query>`.",
        ),
        |ctx| Box::pin(google(ctx)),
    );
    reg_add(
        registry,
        def(
            "img",
            "img( (.*)|$)",
            "Image search: `.img <query>` or `.img <query> ; <n>`. Needs the SERPAPI key.",
        ),
        |ctx| Box::pin(goimg(ctx)),
    );
    reg_add(
        registry,
        def(
            "reverse",
            "reverse$",
            "Reverse image search: `.reverse` as a reply to an image/sticker.",
        ),
        |ctx| Box::pin(reverse(ctx)),
    );
}

#[derive(Debug, Deserialize)]
struct GhUser {
    login: Option<String>,
    id: Option<i64>,
    html_url: Option<String>,
    name: Option<String>,
    company: Option<String>,
    blog: Option<String>,
    location: Option<String>,
    bio: Option<String>,
    public_repos: Option<u64>,
    followers: Option<u64>,
    following: Option<u64>,
}

async fn github(ctx: &Ctx) -> Result<()> {
    let usrname = ctx.args.trim().to_string();
    if usrname.is_empty() {
        ctx.reply(&ctx.get_str("srch_1")).await?;
        return Ok(());
    }
    let url = format!("https://api.github.com/users/{usrname}");
    let resp = ctx
        .http
        .get(&url)
        .header("User-Agent", "Ultroid-RS")
        .send()
        .await?;
    if !resp.status().is_success() {
        ctx.reply(&ctx.get_str("srch_2")).await?;
        return Ok(());
    }
    let u: GhUser = resp.json().await?;
    let uid = u.id.unwrap_or(0);
    let uname = u.login.clone().unwrap_or(usrname);
    let full = format!(
        "**[GITHUB]({})**\n**Name** - {}\n**UserName** - {uname}\n**ID** - {uid}\n**Company** - {}\n**Blog** - {}\n**Location** - {}\n**Bio** - {}\n**Repos** - {}\n**Followers** - {}\n**Following** - {}",
        u.html_url.clone().unwrap_or_default(),
        u.name.clone().unwrap_or_default(),
        u.company.clone().unwrap_or_default(),
        u.blog.clone().unwrap_or_default(),
        u.location.clone().unwrap_or_default(),
        u.bio.clone().unwrap_or_default(),
        u.public_repos.unwrap_or(0),
        u.followers.unwrap_or(0),
        u.following.unwrap_or(0),
    );
    let tmp: PathBuf = std::env::temp_dir().join(format!("gh-{uid}.jpg"));
    let avatar = format!("https://avatars.githubusercontent.com/u/{uid}");
    let avatar_bytes = match ctx.http.get(&avatar).send().await {
        Ok(r) => r.bytes().await.ok(),
        Err(_) => None,
    };
    if let Some(bytes) = avatar_bytes {
        if tokio::fs::write(&tmp, &bytes).await.is_ok() {
            ctx.send_file(&tmp, &full).await?;
            let _ = tokio::fs::remove_file(&tmp).await;
            ctx.delete(ctx.message_id).await?;
            return Ok(());
        }
    }
    ctx.reply(&full).await?;
    Ok(())
}

struct SearchHit {
    title: String,
    link: String,
    description: String,
}

/// Real web search over the DuckDuckGo HTML endpoint (the Python version
/// scrapes Google; the shape of the results is the same).
async fn web_search(ctx: &Ctx, query: &str) -> Result<Vec<SearchHit>> {
    let html = ctx
        .http
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64; rv:58.0) Gecko/20100101 Firefox/58.0",
        )
        .send()
        .await?
        .text()
        .await?;
    // `<a rel="nofollow" class="result__a" href="...">title</a>` followed
    // by `<a class="result__snippet" ...>description</a>`.
    let link_re = Regex::new(r#"class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
        .map_err(|e| Error::Other(e.to_string()))?;
    let snip_re = Regex::new(r#"class="result__snippet"[^>]*>(.*?)</a>"#)
        .map_err(|e| Error::Other(e.to_string()))?;
    let strip = |s: &str| -> String {
        let tag = Regex::new(r"<[^>]+>").expect("static regex");
        let t = tag.replace_all(s, "");
        t.replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&#x27;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .trim()
            .to_string()
    };
    let links: Vec<(String, String)> = link_re
        .captures_iter(&html)
        .map(|c| (strip(&c[2]), c[1].to_string()))
        .collect();
    let snips: Vec<String> = snip_re.captures_iter(&html).map(|c| strip(&c[1])).collect();
    Ok(links
        .into_iter()
        .enumerate()
        .map(|(i, (title, link))| SearchHit {
            title,
            link,
            description: snips.get(i).cloned().unwrap_or_default(),
        })
        .collect())
}

async fn google(ctx: &Ctx) -> Result<()> {
    let inp = ctx.args.trim().to_string();
    if inp.is_empty() {
        ctx.reply(&ctx.get_str("autopic_1")).await?;
        return Ok(());
    }
    let id = ctx.reply(&ctx.get_str("com_2")).await?;
    let gs = web_search(ctx, &inp).await?;
    if gs.is_empty() {
        ctx.edit(id, &ctx.get_str("autopic_2").replacen("{}", &inp, 1))
            .await?;
        return Ok(());
    }
    let mut out = String::new();
    for res in gs {
        out.push_str(&format!(
            " 👉🏻  [{}]({})\n`{}`\n\n",
            res.title, res.link, res.description
        ));
    }
    let omk = format!("**Google Search Query:**\n`{inp}`\n\n**Results:**\n{out}");
    ctx.edit(id, &omk).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SerpImage {
    original: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SerpResp {
    images_results: Option<Vec<SerpImage>>,
}

/// SerpAPI image search (Python `fns.get_google_images`).
async fn serpapi_images(ctx: &Ctx, query: &str) -> Result<Vec<String>> {
    let key = db_get(ctx, "SERPAPI").await?;
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return Err(Error::Config(
            "SERPAPI key not set in the DB (set it with .setredis SERPAPI <key> or .img needs it)"
                .to_string(),
        ));
    };
    let resp: SerpResp = ctx
        .http
        .get("https://serpapi.com/search.json")
        .query(&[("q", query), ("tbm", "isch"), ("api_key", key.as_str())])
        .send()
        .await?
        .json()
        .await?;
    Ok(resp
        .images_results
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| i.original)
        .collect())
}

async fn goimg(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim().to_string();
    if db_get(ctx, "SERPAPI").await?.is_none() {
        ctx.reply("Error: SERPAPI not set please set a SERP API")
            .await?;
        return Ok(());
    }
    let mut lmt = 5usize;
    let mut q = query.as_str();
    if let Some((a, b)) = query.split_once(';') {
        if let Ok(n) = b.trim().parse::<usize>() {
            lmt = n;
        }
        q = a.trim();
    }
    if q.is_empty() {
        ctx.reply(&ctx.get_str("autopic_1")).await?;
        return Ok(());
    }
    let nn = ctx.reply(&ctx.get_str("com_1")).await?;
    let images = serpapi_images(ctx, q).await?;
    for (i, url) in images.iter().take(lmt).enumerate() {
        let bytes = match ctx.http.get(url).send().await {
            Ok(r) => r.bytes().await.ok(),
            Err(_) => None,
        };
        if let Some(bytes) = bytes {
            let tmp = std::env::temp_dir().join(format!("ultroid-img-{i}.jpg"));
            if tokio::fs::write(&tmp, &bytes).await.is_ok() {
                let _ = ctx.send_file(&tmp, "").await;
                let _ = tokio::fs::remove_file(&tmp).await;
            }
        }
    }
    ctx.delete(nn).await?;
    Ok(())
}

async fn reverse(ctx: &Ctx) -> Result<()> {
    if ctx.reply_to_id.is_none() {
        ctx.reply("`Reply to an Image`").await?;
        return Ok(());
    }
    let _id = ctx.reply(&ctx.get_str("com_1")).await?;
    // TODO(responder-op): download the replied image and convert it to
    // PNG (Python: search.py:reverse, `reply.download_media()` +
    // `con.convert(dl, convert_to="png")`); the upload to
    // google.com/searchbyimage and the result parsing below are plain
    // HTTP and are implemented in `reverse_search`.
    Err(Error::Other(
        "download replied media for reverse image search requires a binary Responder op (Python: search.py:reverse)".to_string(),
    ))
}

/// Upload a local PNG to Google's search-by-image endpoint and return the
/// first "best guess" (title, link). Real HTTP; called once the replied
/// media has been downloaded (see `reverse`).
#[allow(dead_code)]
pub async fn reverse_search(
    ctx: &Ctx,
    png_path: &std::path::Path,
) -> Result<Option<(String, String, u32, u32)>> {
    let (w, h) = image::image_dimensions(png_path)
        .map_err(|e| Error::Other(format!("read image dimensions: {e}")))?;
    // Multipart bodies are built by hand: the workspace reqwest build has
    // no `multipart` feature.
    let bytes = tokio::fs::read(png_path).await?;
    let boundary = format!("----ultroid{}", uuid::Uuid::new_v4().as_simple());
    let filename = png_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image.png".to_string());
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"encoded_image\"; filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let resp = ctx
        .http
        .post("https://www.google.com/searchbyimage/upload")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await?;
    let Some(loc) = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let page = ctx
        .http
        .get(&loc)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64; rv:58.0) Gecko/20100101 Firefox/58.0",
        )
        .send()
        .await?
        .text()
        .await?;
    // The "best guess" block used to be `<div class="r5a77d"><a href=...>`.
    let re = Regex::new(r#"class="r5a77d"[^>]*>.*?<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
        .map_err(|e| Error::Other(e.to_string()))?;
    let hit = re.captures(&page).map(|c| {
        let strip = Regex::new(r"<[^>]+>").expect("static regex");
        (
            strip.replace_all(&c[2], "").trim().to_string(),
            c[1].to_string(),
            w,
            h,
        )
    });
    Ok(hit)
}
