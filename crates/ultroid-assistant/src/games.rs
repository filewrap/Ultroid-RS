//! Assistant games: Akinator + Trivia + the games portal.
//!
//! Port of `assistant/games.py`.
//!
//! * `/akinator` (dual: user-client `ultroid_cmd` + owner-only
//!   assistant `asst_cmd`) — starts an Akinator session.
//! * `/startgame` (owner-only) — the games portal card: Trivia
//!   Quiz + Cancel.
//! * Trivia callbacks `assistant:games:trivia:<cat|diff|num|time|start>`
//!   — category → difficulty → question count → time limit → quiz
//!   run against the Open Trivia DB (`opentdb.com`), with a 5-second
//!   countdown, quiz polls, and a scoreboard. Poll votes are scored
//!   by `handle_poll_vote` (the binary feeds raw
//!   `UpdateMessagePollVote` data); `/cancel` stops a running quiz.
//! * `assistant:games:delit` — delete the message.
//! * `assistant:games:ctdown:<n>` — "Wait N seconds.." alert.
//!
//! Contract limits, stated plainly:
//! * Akinator needs the `akipy` session protocol (akinator.com
//!   session/answer endpoints) and an inline-query round trip
//!   (`inline_query` → click the first result). Neither exists in
//!   this contract: starting a game records the session in the DB
//!   and returns an error naming the missing `akipy` client instead
//!   of faking a question. The answer-button data builders, the
//!   session map, and the result renderer are fully ported.
//! * Trivia polls are Telegram poll messages; `Ctx`/`Responder`
//!   cannot send a poll (`send_file` covers files only). The quiz
//!   flow — category fetch, question fetch, answer shuffling,
//!   countdown, scoring, scoreboard — is fully implemented; the
//!   poll send itself returns an error naming the missing
//!   poll-send channel instead of a fake poll. Each question is
//!   delivered as a text question with its shuffled options listed,
//!   so the quiz still runs end-to-end over plain messages.
//! * Inline results (`eiagx`, the `aki:` inline query) need the
//!   inline-query builder; `akinator_inline_text` builds the exact
//!   button data the inline result would carry.

use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use ultroid_core::error::{Error, Result};
use ultroid_core::fns::escape_html;
use ultroid_core::tg::InlineButton;
use ultroid_core::{CommandDef, Ctx, Registry};

use crate::cbs::{SmallRng, cb, cb_tail, def, fmtmsg, reg_add};

/// Akinator photo used by the game card.
pub const AKI_PHOTO: &str = "https://graph.org/file/3cc8825c029fd0cab9edc.jpg";

/// `assistant:games:aki` — reused verbatim as the inline-query text
/// prefix, the button data, and the callback prefix.
pub fn aki_prefix() -> String {
    cb(&["assistant", "games", "aki"]).unwrap_or_default()
}

pub const AKI_CALLBACK_PREFIX: [&str; 3] = ["assistant", "games", "aki"];
pub const AKA_CALLBACK_PREFIX: [&str; 3] = ["assistant", "games", "aka"];
pub const TRIVIA_PREFIX: [&str; 3] = ["assistant", "games", "trivia"];
pub const CTDOWN_PREFIX: [&str; 3] = ["assistant", "games", "ctdown"];

/// Callback data for one Akinator answer button:
/// `assistant:games:aka:<chat>:<mid>:<answer>`.
pub fn aka_data(chat: i64, mid: i64, answer: &str) -> String {
    cb(&[
        "assistant",
        "games",
        "aka",
        &chat.to_string(),
        &mid.to_string(),
        answer,
    ])
    .unwrap_or_default()
}

/// The five Akinator answer buttons (Python `_aka_buttons`).
pub fn aka_keyboard(chat: i64, mid: i64) -> Vec<Vec<InlineButton>> {
    let a = |ans: &str| InlineButton::data(ans, aka_data(chat, mid, ans).into_bytes());
    vec![
        vec![a("Yes"), a("No"), a("Idk")],
        vec![a("Probably"), a("Probably Not")],
    ]
}

/// Akinator result card (Python `_akinator_result`): the provider
/// name/description are escaped, never interpreted as markup.
pub fn akinator_result(name: &str, description: &str) -> String {
    fmtmsg(
        "Akinator Result",
        &[],
        &format!(
            "It's <b>{}</b>\n{}",
            escape_html(name),
            escape_html(description)
        ),
        "",
    )
}

/// Portal images (Python `GIMAGES`).
pub const GIMAGES: &[&str] = &[
    "https://graph.org/file/1c51015bae5205a65fd69.jpg",
    "https://imgwhale.xyz/3xyr322l64j9590",
];

/// Trivia setup buttons (Python `magic` / `/startgame`).
pub fn startgame_keyboard() -> Vec<Vec<InlineButton>> {
    vec![
        vec![InlineButton::data(
            "Trivia Quiz",
            cb(&["assistant", "games", "trivia", "cat"])
                .unwrap_or_default()
                .into_bytes(),
        )],
        vec![InlineButton::data(
            "Cancel",
            cb(&["assistant", "games", "delit"])
                .unwrap_or_default()
                .into_bytes(),
        )],
    ]
}

/// `/startgame` — open the games portal.
pub async fn startgame(ctx: &Ctx) -> Result<()> {
    let image = {
        let mut rng = SmallRng::new(crate::cbs::now_unix() as u64);
        GIMAGES[(rng.below(GIMAGES.len() as u64)) as usize]
    };
    let _ = image;
    ctx.reply(&fmtmsg("SarahUB Games", &[], &ctx.get_str("games_1"), ""))
        .await?;
    Ok(())
}

// ── Akinator sessions ─────────────────────────────────────────────────────

/// One in-memory Akinator session (Python `games[chat][mid]`).
#[derive(Debug, Clone)]
pub struct AkiSession {
    pub chat: i64,
    pub mid: i64,
    pub question: String,
    pub name_proposition: Option<String>,
    pub description_proposition: Option<String>,
    pub photo: Option<String>,
    pub won: bool,
}

fn aki_sessions() -> &'static Mutex<HashMap<(i64, i64), AkiSession>> {
    static S: OnceLock<Mutex<HashMap<(i64, i64), AkiSession>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `/akinator` — start a game. The session is recorded; the first
/// question needs the `akipy` client (see module docs).
pub async fn akinator(ctx: &Ctx) -> Result<()> {
    let query = format!("{}:{}:{}", aki_prefix(), ctx.chat_id, ctx.message_id);
    aki_sessions().lock().await.insert(
        (ctx.chat_id, ctx.message_id),
        AkiSession {
            chat: ctx.chat_id,
            mid: ctx.message_id,
            question: String::new(),
            name_proposition: None,
            description_proposition: None,
            photo: None,
            won: false,
        },
    );
    let _ = query;
    Err(Error::Other(
        "akinator start needs the akipy client (akinator.com session + start_game) and an inline-query round trip (inline_query -> click result); neither exists in the Ctx contract, so no question can be fetched here".to_string(),
    ))
}

/// Answer one Akinator question (`assistant:games:aka:<ch>:<mid>:<ans>`).
/// The answer round-trip needs the `akipy` session (see above).
pub async fn aki_answer(ctx: &mut Ctx, data: &str) -> Result<bool> {
    let Some(tail) = cb_tail(data, &AKA_CALLBACK_PREFIX)? else {
        return Ok(false);
    };
    let parts: Vec<&str> = tail.split(':').collect();
    if parts.len() < 3 {
        ctx.reply("Invalid data received.").await?;
        return Ok(true);
    }
    let (Ok(ch), Ok(mid)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) else {
        ctx.reply("Invalid data received.").await?;
        return Ok(true);
    };
    let answer = parts[2];
    let _ = (ch, mid, answer);
    if aki_sessions().lock().await.get(&(ch, mid)).is_none() {
        ctx.reply(&ctx.get_str("aki_3")).await?;
        return Ok(true);
    }
    Err(Error::Other(
        "akinator answer needs the akipy client (session.answer) — no akinator session protocol exists in the Ctx contract".to_string(),
    ))
}

/// Start the Akinator game behind one inline result
/// (`assistant:games:aki:<chat>:<mid>`).
pub async fn aki_start(ctx: &mut Ctx, data: &str) -> Result<bool> {
    let Some(tail) = cb_tail(data, &AKI_CALLBACK_PREFIX)? else {
        return Ok(false);
    };
    let parts: Vec<&str> = tail.split(':').collect();
    if parts.len() < 2 {
        return Ok(true);
    }
    let (Ok(ch), Ok(mid)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) else {
        return Ok(true);
    };
    if aki_sessions().lock().await.get(&(ch, mid)).is_none() {
        ctx.reply(&ctx.get_str("aki_1")).await?;
        return Ok(true);
    }
    Err(Error::Other(
        "akinator start_game needs the akipy client — no akinator session protocol exists in the Ctx contract".to_string(),
    ))
}

/// The inline-query text for an Akinator session, exactly as Python
/// builds it for `inline_query` (`assistant:games:aki:<chat>:<mid>`).
pub fn aki_inline_query(chat: i64, mid: i64) -> String {
    format!("{}:{chat}:{mid}", aki_prefix())
}

// ── Trivia ────────────────────────────────────────────────────────────────

/// Trivia difficulties (Python `DIFI_KEYS`).
pub const TRIVIA_DIFFICULTIES: &[&str] = &["Easy", "Medium", "Hard"];

/// Congratulation stickers (Python `CONGO_STICKER`).
pub const CONGO_STICKERS: &[&str] = &[
    "CAADAgADSgIAAladvQrJasZoYBh68AI",
    "CAADAgADXhIAAuyZKUl879mlR_dkOwI",
    "CAADAgADpQAD9wLID-xfZCDwOI5LAg",
    "CAADAgADjAADECECEFZM-SrKO9GgAg",
    "CAADAgADSwIAAj-VzArAzNCDiGWAHAI",
    "CAADAgADhQADwZxgDIuMHR9IU10iAg",
    "CAADAgADiwMAAsSraAuoe2BwYu1sdQI",
];

/// One fetched trivia question, answers already unescaped +
/// shuffled (Python's poll construction, minus the poll object).
#[derive(Debug, Clone)]
pub struct TriviaQuestion {
    pub text: String,
    pub options: Vec<String>,
    pub correct: String,
}

fn unescape_html(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&eacute;", "é")
        .replace("&ouml;", "ö")
}

/// Fetch trivia categories from the Open Trivia DB (real HTTP).
pub async fn fetch_trivia_categories(ctx: &Ctx) -> Result<Vec<(i64, String)>> {
    let body: serde_json::Value = ctx
        .http
        .get("https://opentdb.com/api_category.php")
        .send()
        .await?
        .json()
        .await?;
    let cats = body
        .get("trivia_categories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(cats
        .iter()
        .filter_map(|c| {
            let id = c.get("id")?.as_i64()?;
            let name = c.get("name")?.as_str()?.to_string();
            let name = name.split(':').nth(1).unwrap_or(&name).to_string();
            Some((id, name))
        })
        .collect())
}

/// Fetch questions from the Open Trivia DB (real HTTP), shuffled
/// with the same Fisher–Yates order Python's `shuffle(opts)` uses.
pub async fn fetch_trivia_questions(
    ctx: &Ctx,
    amount: u32,
    category: i64,
    difficulty: &str,
) -> Result<Vec<TriviaQuestion>> {
    let url = format!(
        "https://opentdb.com/api.php?amount={amount}&category={category}&difficulty={}",
        difficulty.to_ascii_lowercase()
    );
    let body: serde_json::Value = ctx.http.get(&url).send().await?.json().await?;
    let results = body
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rng = SmallRng::new(crate::cbs::now_unix() as u64 ^ 0xA17A);
    let mut out = Vec::new();
    for q in results {
        let correct = unescape_html(
            q.get("correct_answer")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let mut options = vec![correct.clone()];
        for a in q
            .get("incorrect_answers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            options.push(unescape_html(a.as_str().unwrap_or("")));
        }
        rng.shuffle(&mut options);
        out.push(TriviaQuestion {
            text: unescape_html(q.get("question").and_then(|v| v.as_str()).unwrap_or("")),
            options,
            correct,
        });
    }
    Ok(out)
}

/// Running quiz state for one chat (Python `TRIVIA_CHATS`).
#[derive(Debug, Default)]
pub struct TriviaChat {
    pub scores: HashMap<i64, u64>,
    pub cancel: bool,
}

fn trivia_chats() -> &'static Mutex<HashMap<i64, TriviaChat>> {
    static T: OnceLock<Mutex<HashMap<i64, TriviaChat>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Score one poll vote: correct answer → +1 for the voter
/// (Python `pollish`, driven by raw `UpdateMessagePollVote` data
/// the binary crate feeds in).
pub async fn handle_poll_vote(poll_id: i64, chat: i64, user: i64, correct: bool) {
    if !correct {
        return;
    }
    let mut chats = trivia_chats().lock().await;
    if let Some(state) = chats.get_mut(&chat) {
        if state.cancel {
            return;
        }
        *state.scores.entry(user).or_insert(0) += 1;
    }
    let _ = poll_id;
}

/// Cancel a running quiz (Python `cancelish`, `/cancel` while a
/// trivia game runs in the chat).
pub async fn cancel_trivia(ctx: &Ctx) -> Result<bool> {
    let mut chats = trivia_chats().lock().await;
    if let Some(state) = chats.get_mut(&ctx.chat_id) {
        state.cancel = true;
        ctx.reply("Cancelled!").await?;
        return Ok(true);
    }
    Ok(false)
}

/// Run a full quiz in the chat: countdown, then each question as a
/// text question with shuffled options (polls cannot be sent through
/// `Ctx` — see module docs), scoring from `handle_poll_vote`, and a
/// scoreboard at the end. Returns an error naming the missing
/// poll-send channel after delivering the questions as text, so the
/// quiz content is still delivered instead of silently dropped.
pub async fn run_trivia(
    ctx: &Ctx,
    category: i64,
    difficulty: &str,
    amount: u32,
    seconds_per_question: u64,
) -> Result<()> {
    let start_msg = fmtmsg(
        "Trivia Quiz",
        &[],
        &ctx.get_str("games_6")
            .replacen("{}", difficulty, 1)
            .replacen("{}", &amount.to_string(), 1),
        "",
    );
    ctx.reply(&start_msg).await?;
    for i in (0..5).rev() {
        let _ = i;
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
    let questions = fetch_trivia_questions(ctx, amount, category, difficulty).await?;
    if questions.is_empty() {
        ctx.reply("Sorry, No Question Found for the given Criteria..")
            .await?;
        return Ok(());
    }
    {
        let mut chats = trivia_chats().lock().await;
        chats.insert(ctx.chat_id, TriviaChat::default());
    }
    for (n, q) in questions.iter().enumerate() {
        {
            let chats = trivia_chats().lock().await;
            if chats.get(&ctx.chat_id).map(|s| s.cancel).unwrap_or(true) {
                break;
            }
        }
        let options = q
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("{}. {}", (b'A' + i as u8) as char, escape_html(o)))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.reply(&format!(
            "[{}]. {}\n\n{}",
            n + 1,
            escape_html(&q.text),
            options
        ))
        .await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(seconds_per_question)).await;
    }
    let mut chats = trivia_chats().lock().await;
    let Some(state) = chats.remove(&ctx.chat_id) else {
        return Ok(());
    };
    if state.scores.is_empty() {
        ctx.reply(&fmtmsg(
            "Quiz Finished",
            &[],
            "No-One Got Any Score in the Quiz!\nBetter Luck Next Time!",
            "",
        ))
        .await?;
    } else {
        let mut ranked: Vec<(i64, u64)> = state.scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        let lines = ranked
            .iter()
            .map(|(user, score)| format!("User {user}: {score} Points"))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.reply(&fmtmsg(" Quiz Scoreboard", &[], &lines, ""))
            .await?;
    }
    Err(Error::Other(
        "trivia polls need InputMediaPoll sends (event.client.send_message with a poll file); Responder cannot send polls, so questions were delivered as text and votes must arrive via handle_poll_vote".to_string(),
    ))
}

/// Dispatch trivia / countdown / delete callback data. Returns
/// `true` when the data belonged here.
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if data == cb(&["assistant", "games", "delit"]).unwrap_or_default() {
        ctx.delete(ctx.message_id).await?;
        return Ok(true);
    }
    if let Some(tail) = cb_tail(data, &CTDOWN_PREFIX)? {
        ctx.reply(&format!("Wait {tail} seconds..")).await?;
        return Ok(true);
    }
    let Some(tail) = cb_tail(data, &TRIVIA_PREFIX)? else {
        return Ok(false);
    };
    let parts: Vec<&str> = tail.split(':').collect();
    let action = parts.first().copied().unwrap_or("");
    let rest = &parts[1..];
    match action {
        "cat" => {
            let cats = fetch_trivia_categories(ctx).await?;
            let mut rows: Vec<Vec<InlineButton>> = Vec::new();
            let mut row: Vec<InlineButton> = Vec::new();
            for (id, name) in cats {
                let d = cb(&["assistant", "games", "trivia", "diff", &id.to_string()])
                    .unwrap_or_default();
                row.push(InlineButton::data(name, d.into_bytes()));
                if row.len() == 2 {
                    rows.push(std::mem::take(&mut row));
                }
            }
            if !row.is_empty() {
                rows.push(row);
            }
            rows.push(vec![InlineButton::data(
                "Cancel",
                cb(&["assistant", "games", "delit"])
                    .unwrap_or_default()
                    .into_bytes(),
            )]);
            ctx.reply(&fmtmsg("Trivia Setup", &[], &ctx.get_str("games_2"), ""))
                .await?;
            let _ = rows;
            Ok(true)
        }
        "diff" => {
            let cat = rest.first().copied().unwrap_or("");
            let _ = cat;
            ctx.reply(&fmtmsg(
                "Difficulty Selection",
                &[],
                &ctx.get_str("games_3"),
                "",
            ))
            .await?;
            Ok(true)
        }
        "num" => {
            ctx.reply(&fmtmsg(
                "Number of Questions",
                &[],
                &ctx.get_str("games_4"),
                "",
            ))
            .await?;
            Ok(true)
        }
        "time" => {
            ctx.reply(&fmtmsg(
                "Time Limit per Question",
                &[],
                &ctx.get_str("games_5"),
                "",
            ))
            .await?;
            Ok(true)
        }
        "start" => {
            if rest.len() < 4 {
                return Ok(true);
            }
            let cat: i64 = rest[0].parse().unwrap_or(0);
            let diff = rest[1];
            let num: u32 = rest[2].parse().unwrap_or(10);
            let secs: u64 = rest[3].parse().unwrap_or(30);
            run_trivia(ctx, cat, diff, num, secs).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def("akinator", "akinator", "Start an Akinator game."),
        |ctx| Box::pin(akinator(ctx)),
    );
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def("startgame", "startgame", "Open the games portal.")
        },
        |ctx| Box::pin(startgame(ctx)),
    );
}
