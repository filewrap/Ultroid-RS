//! Port of `plugins/words.py` — dictionary / thesaurus / Urban Dictionary.
//!
//! `.meaning <word>` — dictionaryapi.dev. `.synonym`/`.antonym <word>` —
//! Datamuse (`api.datamuse.com`). `.ud <word>` — Urban Dictionary API.
//! All three are real HTTP calls over `ctx.http`; over-long results are
//! sent as a text file, like the Python version.

use crate::util::{def, reg_add};
use std::path::PathBuf;
use ultroid_core::{Ctx, Registry, Result};

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "meaning",
            "meaning( (.*)|$)",
            "Get the meaning of a word: `.meaning <word>`.",
        ),
        |ctx| Box::pin(meaning(ctx)),
    );
    reg_add(
        registry,
        def(
            "synonym",
            "(syno|anto)nym",
            "Synonyms / antonyms: `.synonym <word>`, `.antonym <word>`.",
        ),
        |ctx| Box::pin(syn_ant(ctx)),
    );
    reg_add(
        registry,
        def(
            "ud",
            "ud (.*)",
            "Urban Dictionary definition: `.ud <word>`.",
        ),
        |ctx| Box::pin(urban(ctx)),
    );
}

async fn send_long(ctx: &Ctx, text: &str, filename: &str, caption: &str) -> Result<()> {
    let path: PathBuf = std::env::temp_dir().join(filename);
    tokio::fs::write(&path, text).await?;
    ctx.send_file(&path, caption).await?;
    ctx.delete(ctx.message_id).await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(())
}

async fn meaning(ctx: &Ctx) -> Result<()> {
    let wrd = ctx.args.trim().to_string();
    if wrd.is_empty() {
        ctx.reply(&ctx.get_str("wrd_4")).await?;
        return Ok(());
    }
    let url = format!("https://api.dictionaryapi.dev/api/v2/entries/en/{wrd}");
    let resp = ctx.http.get(&url).send().await?;
    let out: serde_json::Value = resp.json().await?;
    if let Some(title) = out.get("title").and_then(|t| t.as_str()) {
        ctx.reply(&format!("**{title}**")).await?;
        return Ok(());
    }
    let Some(entry) = out.get(0) else {
        ctx.reply(&ctx.get_str("wrd_4")).await?;
        return Ok(());
    };
    let Some(defi) = entry.pointer("/meanings/0/definitions/0") else {
        ctx.reply(&ctx.get_str("wrd_4")).await?;
        return Ok(());
    };
    let definition = defi
        .get("definition")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    let example = defi
        .get("example")
        .and_then(|e| e.as_str())
        .unwrap_or("None");
    let mut text = ctx
        .get_str("wrd_1")
        .replacen("{}", &wrd, 1)
        .replacen("{}", definition, 1)
        .replacen("{}", example, 1);
    if text == ctx.get_str("wrd_1") {
        // Template had no placeholders; fall back to a plain rendering.
        text = format!("**{wrd}**\n\n{definition}\n\nExample: {example}");
    }
    if let Some(syns) = defi.get("synonyms").and_then(|s| s.as_array()) {
        let list: Vec<&str> = syns.iter().filter_map(|s| s.as_str()).take(10).collect();
        if !list.is_empty() {
            text.push_str(&format!(
                "\n\n• **{} :** {}",
                ctx.get_str("wrd_5"),
                list.join(", ")
            ));
        }
    }
    if let Some(ants) = defi.get("antonyms").and_then(|a| a.as_array()) {
        let list: Vec<&str> = ants.iter().filter_map(|s| s.as_str()).take(10).collect();
        if !list.is_empty() {
            text.push_str(&format!(
                "\n\n**{} :** {}",
                ctx.get_str("wrd_6"),
                list.join(", ")
            ));
        }
    }
    if text.len() > 4096 {
        send_long(
            ctx,
            &text,
            &format!("{wrd}-meanings.txt"),
            &format!("Meanings of {wrd}"),
        )
        .await?;
    } else {
        ctx.reply(&text).await?;
    }
    Ok(())
}

async fn syn_ant(ctx: &Ctx) -> Result<()> {
    let task = format!("{}nyms", ctx.captures.get(1).cloned().unwrap_or_default());
    let wrd = match ctx.text.splitn(2, char::is_whitespace).nth(1) {
        Some(w) if !w.trim().is_empty() => w.trim().to_string(),
        _ => {
            ctx.reply("Give Something to search..").await?;
            return Ok(());
        }
    };
    let rel = if task == "synonyms" { "syn" } else { "ant" };
    let url = format!("https://api.datamuse.com/words?rel_{rel}={wrd}&max=50");
    let resp = match ctx.http.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let key = if task == "synonyms" { "wrd_7" } else { "wrd_8" };
            ctx.reply(&ctx.get_str(key).replacen("{}", &e.to_string(), 1))
                .await?;
            return Ok(());
        }
    };
    let out: serde_json::Value = resp.json().await?;
    let words: Vec<&str> = out
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w.get("word").and_then(|x| x.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let key = if task == "synonyms" { "wrd_2" } else { "wrd_3" };
    let mut x = ctx.get_str(key).replacen("{}", &wrd, 1);
    for (c, w) in words.iter().enumerate() {
        x.push_str(&format!("**{}.** `{w}`\n", c + 1));
    }
    if x.len() > 4096 {
        send_long(
            ctx,
            &x,
            &format!("{wrd}-{task}.txt"),
            &format!("{task} of {wrd}"),
        )
        .await?;
    } else {
        ctx.reply(&x).await?;
    }
    Ok(())
}

async fn urban(ctx: &Ctx) -> Result<()> {
    let word = ctx.args.trim().to_string();
    if word.is_empty() {
        ctx.reply(&ctx.get_str("autopic_1")).await?;
        return Ok(());
    }
    let out: serde_json::Value = ctx
        .http
        .get("http://api.urbandictionary.com/v0/define")
        .query(&[("term", word.as_str())])
        .send()
        .await?
        .json()
        .await?;
    let Some(first) = out.get("list").and_then(|l| l.get(0)) else {
        ctx.reply(&ctx.get_str("autopic_2").replacen("{}", &word, 1))
            .await?;
        return Ok(());
    };
    let w = first.get("word").and_then(|v| v.as_str()).unwrap_or(&word);
    let defn = first
        .get("definition")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ex = first.get("example").and_then(|v| v.as_str()).unwrap_or("");
    let rendered = ctx
        .get_str("wrd_1")
        .replacen("{}", w, 1)
        .replacen("{}", defn, 1)
        .replacen("{}", ex, 1);
    let text = if rendered == ctx.get_str("wrd_1") {
        format!("**{w}**\n\n{defn}\n\nExample: {ex}")
    } else {
        rendered
    };
    ctx.reply(&text).await?;
    Ok(())
}
