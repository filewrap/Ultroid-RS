//! Owner-only native Layer 230 Matrix arcade in the assistant bot.
//!
//! Port of `assistant/matrix_arcade.py` plus the engine it drives,
//! `pyUltroid/_core/matrix_arcade.py`, ported here in full because
//! `ultroid-core::games::MatrixGame` is a simplified variant.
//! The engine below keeps the exact rules:
//! * Modes `menu/rain/ttt/lights/memory/lab/visuals`; `enter`
//!   resets moves/status/frame/running/board/revealed/matched and
//!   seeds the scene from `seed + revision` — lights puzzles are
//!   built by five legal toggles from the solved board (always
//!   solvable), memory decks are six shuffled pairs.
//! * `act`: a mode name re-enters that scene; `reset` re-enters
//!   the current scene; `step` advances the rain frame by one;
//!   `cellN` plays Tic-Tac-Toe (you are X; the local engine plays
//!   O via the exact minimax — win = +1 for O, −1 for X, draw 0,
//!   ties keep the lowest cell index, as Python's `max` does),
//!   toggles Lights Out, or reveals a Memory Vault tile (12
//!   tiles, a mismatch stays visible until the next pick).
//! * `rain_frame(seed, frame)`: 9×22 deterministic terminal art
//!   from `GLYPHS = "01アイウエオカキクケコサシスセソ⊕∆⋮│"`.
//!   One deliberate difference: Python seeds each column with
//!   CPython's `random.Random(seed + x*1009)` (Mersenne Twister);
//!   this crate has no `rand` dependency, so the same construction
//!   uses `SmallRng` (SplitMix64) — same shape and determinism,
//!   different glyph sequence.
//! * Sessions: TTL 20 minutes, at most 64 (oldest idle evicted
//!   at capacity), bound to one owner/chat/message, revisions
//!   increment only after Telegram accepts the edit, an 8-second
//!   edit timeout expires the session rather than replaying an
//!   uncertain move, and `stop` stays available across the
//!   playback's changing revisions.
//!
//! Rendering: Python composes native Layer-230 blocks (titles,
//! preformatted art, tables, nested details, footer). Core
//! `RichDoc` carries HTML / Markdown, not PageBlock trees, so
//! payloads are rendered as equivalent Markdown with the same
//! titles, labels, status lines and control data (the binary crate
//! attaches the keyboard; see `crate::cbs`). The `Download` flow
//! serves the interactive lab
//! (`resources/rich/matrix_lab.html` next to the assistant
//! package; Python: `Path(__file__).parents[1] / resources / rich
//! / matrix_lab.html`) as an HTML document, at most once every
//! ten seconds per session. That file exists in the source tree at
//! `~/workspace/ultroid-rs-work/source/resources/rich/matrix_lab.html`;
//! it is not present under the Rust crate, so `matrix_lab_path`
//! checks the source-tree location first and reports the exact
//! missing-file message when it is absent.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use ultroid_core::error::{Error, Result};
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonAction, ButtonStyle, InlineButton};
use ultroid_core::{Ctx, Registry};

use crate::cbs::{SmallRng, def, fmtmsg, now_mono, reg_add, token_hex_6};

pub const MATRIX_ACTIONS: &[&str] = &[
    "menu", "rain", "ttt", "lights", "memory", "lab", "visuals", "reset", "step", "play", "stop",
    "download", "cell0", "cell1", "cell2", "cell3", "cell4", "cell5", "cell6", "cell7", "cell8",
    "cell9", "cell10", "cell11",
];

pub const SESSION_TTL_SECS: f64 = 20.0 * 60.0;
pub const MAX_SESSIONS: usize = 64;
pub const FRAME_COUNT: usize = 4;
pub const FRAME_INTERVAL_SECS: f64 = 2.0;
pub const EDIT_TIMEOUT_SECS: u64 = 8;
pub const DOWNLOAD_THROTTLE_SECS: f64 = 10.0;

pub const MODES: &[&str] = &["menu", "rain", "ttt", "lights", "memory", "lab", "visuals"];
pub const GLYPHS: &str = "01アイウエオカキクケコサシスセソ⊕∆⋮│";
pub const SYMBOLS: &[&str] = &["◆", "●", "▲", "■", "✦", "⬡"];
pub const LINES: [[usize; 3]; 8] = [
    [0, 1, 2],
    [3, 4, 5],
    [6, 7, 8],
    [0, 3, 6],
    [1, 4, 7],
    [2, 5, 8],
    [0, 4, 8],
    [2, 4, 6],
];

/// Candidate locations for the interactive lab companion
/// (`resources/rich/matrix_lab.html`).
pub const MATRIX_LAB_CANDIDATES: &[&str] = &[
    "/home/hatch/workspace/ultroid-rs-work/source/resources/rich/matrix_lab.html",
    "resources/rich/matrix_lab.html",
    "ultroid-assistant/resources/rich/matrix_lab.html",
    "crates/ultroid-assistant/resources/rich/matrix_lab.html",
];

fn matrix_lab_path() -> Option<std::path::PathBuf> {
    MATRIX_LAB_CANDIDATES
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
}

// ── Engine (pyUltroid/_core/matrix_arcade.py) ─────────────────────────────

/// Toggle a Lights Out cell and its orthogonal neighbours, without wrapping.
pub fn toggle(board: &mut [i32; 9], cell: usize) -> Result<()> {
    if cell >= 9 {
        return Err(Error::Other("Cell outside grid".into()));
    }
    let (r, c) = (cell / 3, cell % 3);
    for (nr, nc) in [
        (r as i32, c as i32),
        (r as i32 - 1, c as i32),
        (r as i32 + 1, c as i32),
        (r as i32, c as i32 - 1),
        (r as i32, c as i32 + 1),
    ] {
        if (0..3).contains(&nr) && (0..3).contains(&nc) {
            let idx = (nr * 3 + nc) as usize;
            board[idx] ^= 1;
        }
    }
    Ok(())
}

/// The winning player (1 = X, 2 = O) or 0 for a draw/unfinished board.
pub fn winner(board: &[i32; 9]) -> i32 {
    for [a, b, c] in LINES {
        if board[a] != 0 && board[a] == board[b] && board[a] == board[c] {
            return board[a];
        }
    }
    0
}

fn minimax(board: [i32; 9], player: i32, cache: &mut HashMap<[i32; 9], i32>) -> i32 {
    if let Some(v) = cache.get(&board) {
        return *v;
    }
    let won = winner(&board);
    if won != 0 {
        let v = if won == 2 { 1 } else { -1 };
        cache.insert(board, v);
        return v;
    }
    if board.iter().all(|v| *v != 0) {
        cache.insert(board, 0);
        return 0;
    }
    let mut scores = Vec::new();
    for (i, v) in board.iter().enumerate() {
        if *v == 0 {
            scores.push(score_move(board, i, player, cache));
        }
    }
    let v = if player == 2 {
        *scores.iter().max().unwrap_or(&0)
    } else {
        *scores.iter().min().unwrap_or(&0)
    };
    cache.insert(board, v);
    v
}

fn score_move(
    board: [i32; 9],
    cell: usize,
    player: i32,
    cache: &mut HashMap<[i32; 9], i32>,
) -> i32 {
    let mut next = board;
    next[cell] = player;
    minimax(next, 3 - player, cache)
}

/// The engine's Tic-Tac-Toe move: the lowest-index cell with the
/// best minimax score (Python `max(..., key=...)` keeps the first
/// maximum in iteration order).
pub fn best_ttt_move(board: [i32; 9]) -> Option<usize> {
    let mut cache: HashMap<[i32; 9], i32> = HashMap::new();
    let mut best: Option<(usize, i32)> = None;
    for (i, v) in board.iter().enumerate() {
        if *v == 0 {
            let score = score_move(board, i, 2, &mut cache);
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((i, score));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// The 9×22 rain frame (Python `rain_frame`).
pub fn rain_frame(seed: u64, frame: u32) -> String {
    let glyphs: Vec<char> = GLYPHS.chars().collect();
    let mut lines = Vec::new();
    for y in 0..9usize {
        let mut row = String::new();
        for x in 0..22usize {
            let mut rng = SmallRng::new(seed.wrapping_add((x as u64) * 1009));
            let head = (rng.below(15) as u64 + frame as u64 * (1 + (x as u64) % 2)) % 15;
            let age = (head + 15 - y as u64) % 15;
            let glyph = glyphs[(x * 7 + y * 13 + frame as usize) % glyphs.len()];
            row.push(if age < 5 { glyph } else { ' ' });
        }
        lines.push(row);
    }
    lines.join("\n")
}

/// One ephemeral arcade screen (Python `Arcade` dataclass).
#[derive(Debug, Clone)]
pub struct Arcade {
    pub token: String,
    pub mode: String,
    pub revision: u64,
    pub seed: u64,
    pub frame: u32,
    pub board: [i32; 9],
    pub deck: Vec<i32>,
    pub revealed: Vec<usize>,
    pub matched: Vec<usize>,
    pub moves: u32,
    pub status: String,
    pub running: bool,
}

impl Arcade {
    pub fn new(token: String, seed: u64) -> Self {
        Arcade {
            token,
            mode: "menu".to_string(),
            revision: 0,
            seed,
            frame: 0,
            board: [0; 9],
            deck: vec![],
            revealed: vec![],
            matched: vec![],
            moves: 0,
            status: String::new(),
            running: false,
        }
    }

    /// Initialise a fresh playable scene using a reproducible local seed.
    pub fn enter(&mut self, mode: &str) -> Result<()> {
        if !MODES.contains(&mode) {
            return Err(Error::Other("Unknown arcade scene".into()));
        }
        self.mode = mode.to_string();
        self.moves = 0;
        self.status = String::new();
        self.frame = 0;
        self.running = false;
        self.board = [0; 9];
        self.revealed = vec![];
        self.matched = vec![];
        let mut rng = SmallRng::new(self.seed.wrapping_add(self.revision));
        if mode == "lights" {
            // Construct by legal moves from the solved board: always solvable.
            let mut cells: Vec<usize> = (0..9).collect();
            rng.shuffle(&mut cells);
            for cell in cells.iter().take(5) {
                let _ = toggle(&mut self.board, *cell);
            }
            if !self.board.iter().any(|v| *v != 0) {
                let _ = toggle(&mut self.board, 4);
            }
        }
        if mode == "memory" {
            self.deck = (0..6).flat_map(|n| [n, n]).collect();
            rng.shuffle(&mut self.deck);
        }
        Ok(())
    }

    /// Apply a validated game action; reject illegal or completed moves.
    pub fn act(&mut self, action: &str) -> Result<()> {
        if MODES.contains(&action) {
            return self.enter(action);
        }
        if action == "reset" {
            let mode = self.mode.clone();
            return self.enter(&mode);
        }
        if action == "step" && self.mode == "rain" {
            self.frame += 1;
            return Ok(());
        }
        let Some(cell_str) = action.strip_prefix("cell") else {
            return Err(Error::Other(
                "That control is not available on this screen".into(),
            ));
        };
        let cell: usize = match cell_str.parse() {
            Ok(n) => n,
            Err(_) => {
                return Err(Error::Other(
                    "That control is not available on this screen".into(),
                ));
            }
        };
        match self.mode.as_str() {
            "ttt" => {
                if cell >= 9
                    || self.board[cell] != 0
                    || winner(&self.board) != 0
                    || self.board.iter().all(|v| *v != 0)
                {
                    return Err(Error::Other(
                        "Choose an empty cell in an unfinished game".into(),
                    ));
                }
                self.board[cell] = 1;
                self.moves += 1;
                if winner(&self.board) == 0 && self.board.iter().any(|v| *v == 0) {
                    if let Some(best) = best_ttt_move(self.board) {
                        self.board[best] = 2;
                    }
                }
                Ok(())
            }
            "lights" => {
                if cell >= 9 || !self.board.iter().any(|v| *v != 0) {
                    return Err(Error::Other("Puzzle complete; start a new grid".into()));
                }
                toggle(&mut self.board, cell)?;
                self.moves += 1;
                Ok(())
            }
            "memory" => {
                if cell >= 12 || self.matched.contains(&cell) || self.revealed.contains(&cell) {
                    return Err(Error::Other("Choose an unrevealed tile".into()));
                }
                if self.revealed.len() == 2 {
                    self.revealed = vec![];
                }
                self.revealed.push(cell);
                if self.revealed.len() == 2 {
                    self.moves += 1;
                    let (a, b) = (self.revealed[0], self.revealed[1]);
                    if self.deck[a] == self.deck[b] {
                        self.matched.extend([a, b]);
                        self.revealed = vec![];
                    }
                }
                Ok(())
            }
            _ => Err(Error::Other("No board on this screen".into())),
        }
    }

    pub fn title(&self) -> String {
        let scene = match self.mode.as_str() {
            "menu" => "THE CONSTRUCT",
            "rain" => "DIGITAL RAIN",
            "ttt" => "NEURAL DUEL",
            "lights" => "BLACKOUT",
            "memory" => "MEMORY VAULT",
            "lab" => "SOURCE LAB",
            "visuals" => "VISUAL FORGE",
            _ => "?",
        };
        format!("⌘ MATRIX / {scene}")
    }

    /// The matrix lobby (Python `lobby_payload`) — menu + header line.
    pub fn lobby_doc(&self) -> Result<RichDoc> {
        let text = fmtmsg(
            "⌘ MATRIX / NATIVE ARCADE",
            &[],
            "A playable pocket terminal. Group Ludo, three solo games, digital rain and a native visual forge.",
            "",
        );
        let text = format!(
            "{text}\n\n▶ Enter the construct — use the mx:new control.\n\nNative MTProto controls. The separate browser companion runs JavaScript; Telegram messages do not."
        );
        RichDoc::markdown(text)
    }
}

pub fn lobby_doc() -> Result<RichDoc> {
    Arcade::new(String::new(), 0).lobby_doc()
}

// ── Sessions ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct MatrixSession {
    pub state: Arcade,
    pub owner: i64,
    pub chat: i64,
    pub message: Option<i64>,
    pub touched: f64,
    pub downloaded: Option<f64>,
    pub stop: bool,
}

fn sessions() -> &'static Mutex<HashMap<String, Arc<Mutex<MatrixSession>>>> {
    static S: OnceLock<Mutex<HashMap<String, Arc<Mutex<MatrixSession>>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn prune_sessions() {
    let now = now_mono();
    let mut map = sessions().lock().await;
    let mut dead = Vec::new();
    for (token, s) in map.iter() {
        if let Ok(s) = s.try_lock() {
            if now - s.touched >= SESSION_TTL_SECS {
                dead.push(token.clone());
            }
        }
    }
    for token in dead {
        map.remove(&token);
    }
}

/// Render a session (Python `payload`) as Markdown plus its control
/// keyboard.
pub fn render_session(s: &MatrixSession) -> (RichDoc, Vec<Vec<InlineButton>>) {
    let state = &s.state;
    let ctl = |label: &str, action: &str, style: Option<ButtonStyle>| {
        let mut b = InlineButton::data(
            label,
            format!("mx:{}:{}:{}", state.token, state.revision, action).into_bytes(),
        );
        if let Some(style) = style {
            b = b.styled(style);
        }
        b
    };
    let mut text = fmtmsg(&state.title(), &[], "", "");
    match state.mode.as_str() {
        "menu" => {
            text.push_str("\n\nWake up. The message is the interface.\n\n```\n┌──────────────────────┐\n│  SARAH // CONSTRUCT   │\n│  [ native link: ON ]  │\n└──────────────────────┘\n```\n\n");
            text.push_str("\nNeural duel: you are X against a perfect local O opponent.\n\nBlackout: switch off a solvable 3×3 cross-toggle grid.\n\nMemory vault: uncover six matching symbol pairs.\n\nUnder the glass: Every move edits this message through native Layer-230 MTProto. Sessions are owner/chat/message-bound, expire after 20 minutes idle, and disappear on restart. Symbols are ordinary Unicode, not animated custom emoji. Digital rain is terminal text updated by bounded server edits, not JS, CSS or video.\n");
        }
        "rain" => {
            let running = if state.running {
                "4-frame transmission in progress"
            } else {
                "Transmission idle"
            };
            text.push_str(&format!(
                "\n\nSIGNAL {:03} · {running}\n\n```\n{}\n```\n\nA brief sequence of message edits, about two seconds apart. Telegram may throttle updates; a timed-out screen expires rather than replaying uncertain moves. Full canvas animation is in the browser companion.\n",
                state.frame,
                rain_frame(state.seed, state.frame)
            ));
        }
        "ttt" | "lights" | "memory" => {
            let status: String = match state.mode.as_str() {
                "ttt" => {
                    let won = winner(&state.board);
                    if won == 2 {
                        "O wins. Reset to try again.".to_string()
                    } else if won == 1 {
                        "X wins.".to_string()
                    } else if state.board.iter().all(|v| *v != 0) {
                        "Draw. You held the line.".to_string()
                    } else {
                        "Your move: X. The local engine plays O.".to_string()
                    }
                }
                "lights" => {
                    if !state.board.iter().any(|v| *v != 0) {
                        "BLACKOUT COMPLETE. You broke the circuit.".to_string()
                    } else {
                        "Turn every light off. A tap flips that cell and its orthogonal neighbors."
                            .to_string()
                    }
                }
                _ => {
                    if state.matched.len() == 12 {
                        "VAULT OPEN. All six pairs recovered.".to_string()
                    } else {
                        "Find six pairs. A mismatch stays visible until your next pick.".to_string()
                    }
                }
            };
            text.push_str(&format!("\n\n{status}\n\nMoves: {}\n", state.moves));
        }
        "visuals" => {
            text.push_str("\n\nDifferent materials, one native message. These are real text and structured blocks, not a web-icon canvas.\n\n");
            text.push_str("Geometry: ◆ ◇ ● ○ ▲ △ ■ □ ✦ ⬡\nSignals: ▁ ▂ ▃ ▄ ▅ ▆ ▇ █  ▰▰▰▱▱\nCircuit: ┌─┬─┐  ╔═╦═╗  ╱╲ ╳\nBraille: ⠁ ⠃ ⠇ ⠏ ⠟ ⠿ ⣿\nUnicode emoji: 🎲 🏁 🛡 🎮 ⚡\n\n");
            text.push_str("```\nRADAR     ╱│╲      CORE  ▰▰▰▰▱ 80%\n        ───◆───    LINK  ▰▰▰▱▱ 60%\n          ╲│╱      SYNC  [ ONLINE ]\n```\n\n");
            text.push_str("What is actually native? Native: Unicode labels, monospaced art, tables, nested details, emphasis, copy/URL/callback controls, and uploaded media. Custom emoji need real Telegram document IDs and applicable account/bot permissions; no invented IDs are sent. Browser only: Font Awesome/Lucide web fonts, arbitrary CSS, inline SVG rendering, HTML5 canvas, WebGL and running JavaScript. Exporting web artwork to an uploaded PNG makes a native image, not a running webpage. 38 visual methods are documented in docs/ARCADE_VISUAL_METHODS.md. The offline companion stays a downloadable local file; nothing is publicly deployed.\n");
        }
        _ => {
            // lab
            text.push_str("\n\nCode is displayed as code. The downloadable companion is an offline browser page, not an embedded Telegram app.\n\n");
            text.push_str("Nested source capsule:\n\n```html\n<details><summary>Follow the white rabbit</summary>\n  <p><b>Wake up.</b> Native rich HTML is a document format.</p>\n</details>\n```\n\nJavaScript + vector source:\n\n```javascript\nconst canvas = document.querySelector(\"canvas\");\nconst ctx = canvas.getContext(\"2d\");\n// requestAnimationFrame(draw): browser companion only\n```\n\n```xml\n<svg viewBox=\"0 0 64 64\">\n  <path d=\"M32 4 60 32 32 60 4 32Z\" fill=\"#00ff9d\"/>\n</svg>\n```\n\nNo claim that Telegram executes scripts, paints SVG, supports arbitrary CSS or accepts unuploaded media references.\n\n```\n       ◇\n      ╱ ╲\n     ◇───◇\n      ╲ ╱\n       ◇\n    CONSTRUCT\n```\n");
        }
    }
    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    match state.mode.as_str() {
        "menu" => {
            rows.push(vec![
                ctl("≋ Digital rain", "rain", None),
                ctl("✕ Neural duel", "ttt", None),
            ]);
            rows.push(vec![
                ctl("◈ Blackout", "lights", None),
                ctl("◆ Memory vault", "memory", None),
            ]);
            rows.push(vec![InlineButton::data(
                "♜ Human Ludo · group",
                b"lu:new".to_vec(),
            )]);
            rows.push(vec![
                ctl("✦ Visual forge", "visuals", None),
                ctl("⌘ Source lab", "lab", None),
            ]);
        }
        "rain" => {
            rows.push(vec![
                ctl("▸ Next frame", "step", None),
                ctl("▶ Play 4 frames", "play", Some(ButtonStyle::Success)),
                ctl("■ Stop", "stop", Some(ButtonStyle::Danger)),
            ]);
        }
        "ttt" | "lights" | "memory" => {
            let (labels, width): (Vec<String>, usize) = match state.mode.as_str() {
                "ttt" => (
                    state
                        .board
                        .iter()
                        .map(|v| match v {
                            0 => "·".to_string(),
                            1 => "✕".to_string(),
                            _ => "◯".to_string(),
                        })
                        .collect(),
                    3,
                ),
                "lights" => (
                    state
                        .board
                        .iter()
                        .map(|v| {
                            if *v != 0 {
                                "●".to_string()
                            } else {
                                "○".to_string()
                            }
                        })
                        .collect(),
                    3,
                ),
                _ => (
                    (0..12)
                        .map(|i| {
                            if state.revealed.contains(&i) || state.matched.contains(&i) {
                                SYMBOLS[state.deck[i] as usize].to_string()
                            } else {
                                "?".to_string()
                            }
                        })
                        .collect(),
                    4,
                ),
            };
            for start in (0..labels.len()).step_by(width) {
                rows.push(
                    (start..(start + width).min(labels.len()))
                        .map(|i| {
                            ctl(
                                &format!("{} {}", labels[i], i + 1),
                                &format!("cell{i}"),
                                None,
                            )
                        })
                        .collect(),
                );
            }
            rows.push(vec![ctl("↺ New game", "reset", None)]);
        }
        "lab" => {
            rows.push(vec![ctl(
                "⇩ Get browser companion",
                "download",
                Some(ButtonStyle::Success),
            )]);
            rows.push(vec![
                InlineButton {
                    text: "Copy HTML sample".to_string(),
                    action: ButtonAction::Copy("<p><b>Wake up.</b></p>".to_string()),
                    style: ButtonStyle::Default,
                },
                InlineButton::url(
                    "Native schema docs ↗",
                    "https://core.telegram.org/schema".to_string(),
                ),
            ]);
        }
        _ => {}
    }
    if state.mode != "menu" {
        rows.push(vec![ctl("⌂ Construct", "menu", None)]);
    }
    text.push_str("\n\nNative MTProto • ephemeral arcade • browser features labelled separately");
    let doc = RichDoc::markdown(text)
        .unwrap_or_else(|_| RichDoc::markdown("Complex Matrix").expect("static markdown"));
    (doc, rows)
}

fn valid_matrix_data(data: &str) -> bool {
    if data == "mx:new" {
        return true;
    }
    let parts: Vec<&str> = data.split(':').collect();
    if parts.len() != 4 || parts[0] != "mx" {
        return false;
    }
    let (token, rev, action) = (parts[1], parts[2], parts[3]);
    token.len() == 12
        && token.chars().all(|c| c.is_ascii_hexdigit())
        && !rev.is_empty()
        && rev.chars().all(|c| c.is_ascii_digit())
        && MATRIX_ACTIONS.contains(&action)
}

/// Open a session (Python `launch`).
pub async fn launch_session(ctx: &Ctx) -> Result<i64> {
    if !ctx.is_owner {
        return Err(Error::Other(
            "Only the owner can open a Matrix arcade".into(),
        ));
    }
    prune_sessions().await;
    let token = token_hex_6();
    let session = Arc::new(Mutex::new(MatrixSession {
        state: Arcade::new(token.clone(), now_mono() as u64),
        owner: ctx.sender_id,
        chat: ctx.chat_id,
        message: None,
        touched: now_mono(),
        downloaded: None,
        stop: false,
    }));
    {
        let mut map = sessions().lock().await;
        if map.len() >= MAX_SESSIONS {
            // Evict the oldest idle session; if every one is busy, refuse.
            let mut idle: Vec<(f64, String)> = Vec::new();
            for (t, s) in map.iter() {
                if let Ok(s) = s.try_lock() {
                    idle.push((s.touched, t.clone()));
                }
            }
            match idle
                .iter()
                .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            {
                Some((_, oldest)) => {
                    map.remove(oldest);
                }
                None => {
                    return Err(Error::Other(
                        "All arcade screens are busy; try again shortly".into(),
                    ));
                }
            }
        }
        map.insert(token.clone(), session.clone());
    }
    let (doc, _kb) = {
        let s = session.lock().await;
        render_session(&s)
    };
    match ctx.send_rich_or_plain(&doc).await {
        Ok(id) => {
            session.lock().await.message = Some(id);
            Ok(id)
        }
        Err(e) => {
            sessions().lock().await.remove(&token);
            Err(e)
        }
    }
}

/// `/matrix` (and `/arcade`) — open a native Matrix arcade session.
pub async fn matrix(ctx: &Ctx) -> Result<()> {
    match launch_session(ctx).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let doc = RichDoc::markdown(format!("Complex Matrix\n\n{e}"))?;
            ctx.send_rich_or_plain(&doc).await?;
            Err(e)
        }
    }
}

/// Commit game state only after Telegram accepts the candidate
/// message edit (Python `_commit`).
async fn commit_session(
    ctx: &Ctx,
    session: &Arc<Mutex<MatrixSession>>,
    mut candidate: Arcade,
) -> Result<bool> {
    let (doc, message) = {
        let s = session.lock().await;
        candidate.revision = s.state.revision + 1;
        let tmp = MatrixSession {
            state: candidate.clone(),
            owner: s.owner,
            chat: s.chat,
            message: s.message,
            touched: s.touched,
            downloaded: s.downloaded,
            stop: s.stop,
        };
        let (doc, _kb) = render_session(&tmp);
        (doc, s.message)
    };
    let Some(message) = message else {
        return Ok(false);
    };
    let text = doc.markdown.clone().unwrap_or_else(|| doc.plain_text());
    let edit = tokio::time::timeout(
        std::time::Duration::from_secs(EDIT_TIMEOUT_SECS),
        ctx.edit(message, &text),
    )
    .await;
    match edit {
        Ok(Ok(())) => {
            let mut s = session.lock().await;
            s.state = candidate;
            s.touched = now_mono();
            Ok(true)
        }
        Ok(Err(_)) => Ok(false), // MessageNotModified: do not commit an unseen revision.
        Err(_) => {
            let token = session.lock().await.state.token.clone();
            sessions().lock().await.remove(&token);
            Err(Error::Other(
                "Telegram did not confirm this update in time. The screen expired; send /matrix again".to_string(),
            ))
        }
    }
}

/// Play at most four native message edits, then restore idle
/// controls (Python `_play`).
async fn play_session(ctx: &Ctx, session: &Arc<Mutex<MatrixSession>>) -> Result<()> {
    session.lock().await.stop = false;
    for index in 0..FRAME_COUNT {
        if session.lock().await.stop {
            break;
        }
        let candidate = {
            let mut c = session.lock().await.state.clone();
            c.frame += 1;
            c.running = index < FRAME_COUNT - 1;
            c
        };
        let running = candidate.running;
        if !commit_session(ctx, session, candidate).await? {
            break;
        }
        if running {
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(FRAME_INTERVAL_SECS)).await;
        }
    }
    if session.lock().await.state.running {
        let candidate = {
            let mut c = session.lock().await.state.clone();
            c.running = false;
            c
        };
        let _ = commit_session(ctx, session, candidate).await;
    }
    Ok(())
}

/// Dispatch `mx:*` callback data. Returns `true` when the data
/// belonged here. Python alert answers are delivered as replies
/// (see `crate::cbs` — `Ctx` has no callback-answer channel).
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if !valid_matrix_data(data) {
        return Ok(false);
    }
    if !ctx.is_owner || ctx.sender_id <= 0 {
        ctx.reply("Use Matrix controls as the owner inside the original chat")
            .await?;
        return Ok(true);
    }
    if data == "mx:new" {
        match launch_session(ctx).await {
            Ok(_) => {
                ctx.reply("Matrix session opened").await?;
            }
            Err(e) => {
                ctx.reply(&e.to_string()).await?;
            }
        }
        return Ok(true);
    }
    prune_sessions().await;
    let parts: Vec<&str> = data.split(':').collect();
    let (token, revision, action) = (parts[1], parts[2], parts[3]);
    let session = match sessions().lock().await.get(token).cloned() {
        Some(s) => s,
        None => {
            ctx.reply("This screen expired or the bot restarted. Send /matrix again")
                .await?;
            return Ok(true);
        }
    };
    let revision: u64 = revision.parse().unwrap_or(u64::MAX);
    // Stop remains available across the finite playback's changing
    // frame revisions.
    if action == "stop" {
        let s = session.lock().await;
        if s.state.mode == "rain" && s.state.running {
            drop(s);
            session.lock().await.stop = true;
            ctx.reply("Stopping after the current frame").await?;
            return Ok(true);
        }
    }
    // Bind the cloned Arc first: `session.clone().try_lock()` drops the
    // temporary Arc while its guard is still borrowed (E0716).
    let session = session.clone();
    let Ok(mut s) = session.try_lock() else {
        ctx.reply("This screen is updating. Try again in a moment")
            .await?;
        return Ok(true);
    };
    if (s.owner, s.chat, s.message) != (ctx.sender_id, ctx.chat_id, Some(ctx.message_id)) {
        ctx.reply("This control belongs to a different owner, chat or message")
            .await?;
        return Ok(true);
    }
    if revision != s.state.revision {
        ctx.reply("That frame is old. Use the newest controls")
            .await?;
        return Ok(true);
    }
    let now = now_mono();
    match action {
        "play" => {
            if s.state.mode != "rain" {
                ctx.reply("Playback belongs to digital rain").await?;
                return Ok(true);
            }
            ctx.reply("Requesting up to four native MTProto frames")
                .await?;
            drop(s);
            match play_session(ctx, &session).await {
                Ok(()) => {}
                Err(e) => {
                    ctx.reply(&format!(
                        "Matrix playback stopped\n\n{e}\n\nNo move was replayed. The last accepted frame may remain visible."
                    ))
                    .await?;
                }
            }
            return Ok(true);
        }
        "download" => {
            if s.state.mode != "lab" {
                ctx.reply("Open Source lab to download the companion")
                    .await?;
                return Ok(true);
            }
            if let Some(last) = s.downloaded {
                if now - last < DOWNLOAD_THROTTLE_SECS {
                    ctx.reply("The browser companion was just sent; wait ten seconds")
                        .await?;
                    return Ok(true);
                }
            }
            match matrix_lab_path() {
                Some(path) => {
                    s.downloaded = Some(now);
                    s.touched = now;
                    drop(s);
                    match ctx
                        .send_file(
                            &path,
                            "Matrix browser companion. Download and open in a browser. JavaScript, canvas and SVG run there, not inside Telegram messages.",
                        )
                        .await
                    {
                        Ok(_) => {
                            ctx.reply("Browser companion sent as an HTML document").await?;
                        }
                        Err(e) => {
                            ctx.reply(&e.to_string()).await?;
                        }
                    }
                }
                None => {
                    ctx.reply("The browser companion is unavailable on this deployment")
                        .await?;
                }
            }
            return Ok(true);
        }
        _ => {}
    }
    let mut candidate = s.state.clone();
    let outcome: Result<()> = (|| {
        if action == "stop" {
            if candidate.mode != "rain" {
                return Err(Error::Other("Stop belongs to digital rain".into()));
            }
            candidate.running = false;
            candidate.frame += 1;
            Ok(())
        } else {
            candidate.act(action)
        }
    })();
    match outcome {
        Err(e) => {
            ctx.reply(&format!(
                "Matrix playback stopped\n\n{e}\n\nNo move was replayed. The last accepted frame may remain visible."
            ))
            .await?;
            Ok(true)
        }
        Ok(()) => {
            drop(s);
            let committed = commit_session(ctx, &session, candidate).await;
            match committed {
                Ok(_) => Ok(true),
                Err(e) => {
                    ctx.reply(&e.to_string()).await?;
                    Ok(true)
                }
            }
        }
    }
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        def(
            "matrix",
            "matrix",
            "Open the interactive Layer 230 Matrix arcade.",
        ),
        |ctx| Box::pin(matrix(ctx)),
    );
    reg_add(
        registry,
        def(
            "arcade",
            "arcade",
            "Open the interactive Layer 230 Matrix arcade.",
        ),
        |ctx| Box::pin(matrix(ctx)),
    );
}
