//! Shared native group Ludo with locked, revisioned, ephemeral table sessions.
//!
//! Port of `assistant/ludo_arcade.py` plus the engine it drives,
//! `pyUltroid/_core/ludo_arcade.py` (the engine is ported here in
//! full because `ultroid-core::games::LudoGame` is a simplified
//! variant: this file keeps the exact rules — 52-track, 6 home
//! lane, 4 tokens, safe star squares, blockades, captures, bonus
//! rolls, three-sixes forfeit, AFK skip after 90 s).
//!
//! Protocol (unchanged):
//! * `/ludo` — one table per group, 2–4 humans, no AI opponent.
//!   Tables expire after one hour idle; at most 64 tables.
//! * Controls are exact-revision callback data
//!   `lu:<token>:<revision>:<action>` (`join|leave|start|cancel|
//!   roll|move0-3|resign|skip|rematch|cancelok|resignok|back`);
//!   `lu:new` opens a fresh lobby. Stale revisions, copied
//!   controls (wrong chat/message), and illegal turns are rejected
//!   with the same messages Python uses.
//! * A turn is published before it is committed: the candidate
//!   board is edited into Telegram first, and only an accepted edit
//!   commits the state. A timed-out edit expires the table rather
//!   than replaying an uncertain turn. `cancel`/`resign` need a
//!   20-second confirmation (`cancelok`/`resignok`).
//!
//! Rendering: Python composes native Layer-230 blocks (board,
//! legend, player cards, nested details). Core `RichDoc` carries
//! HTML / Markdown, not PageBlock trees, so the board is rendered
//! as the engine's own `board_text` (15×15 cross board + token
//! ledger, copied from the core source) inside a Markdown code
//! block, with the same controls as an inline keyboard (the binary
//! crate attaches it; see `crate::cbs`).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use ultroid_core::error::{Error, Result};
use ultroid_core::rich::RichDoc;
use ultroid_core::tg::{ButtonStyle, InlineButton};
use ultroid_core::{Ctx, Registry};

use crate::cbs::{def, now_mono, reg_add, token_hex_6};

/// Callback-data pattern (Python `PATTERN`).
pub const LUDO_ACTIONS: &[&str] = &[
    "join", "leave", "start", "cancel", "roll", "move0", "move1", "move2", "move3", "resign",
    "skip", "rematch", "cancelok", "resignok", "back",
];

pub const TABLE_TTL_SECS: f64 = 60.0 * 60.0;
pub const TABLE_CAPACITY: usize = 64;
pub const EDIT_TIMEOUT_SECS: u64 = 8;

// ── Engine (pyUltroid/_core/ludo_arcade.py) ───────────────────────────────

pub const TRACK: i64 = 52;
pub const HOME: i64 = 6;
pub const TOKENS: usize = 4;
pub const FINISH: i64 = TRACK + HOME - 1; // 57
pub const AFK_SECONDS: f64 = 90.0;
pub const SAFE_SQUARES: &[i64] = &[0, 8, 13, 21, 26, 34, 39, 47];

#[derive(Debug, Clone)]
pub struct LudoPlayer {
    pub user: i64,
    pub offset: i64,
    pub tokens: [i64; TOKENS], // -1 = home yard
    pub resigned: bool,
    pub finished: bool,
}

impl LudoPlayer {
    fn new(user: i64, offset: i64) -> Self {
        LudoPlayer {
            user,
            offset,
            tokens: [-1; TOKENS],
            resigned: false,
            finished: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Ludo {
    pub players: Vec<LudoPlayer>,
    pub status: String, // lobby | playing | finished | cancelled
    pub active: usize,
    pub winners: Vec<i64>,
    pub events: Vec<String>,
    pub last_activity: f64,
    pub turn_started: f64,
    pub rolls: Vec<u8>,
    pub sixes: u32,
    pub dice: Option<u8>,
}

impl Ludo {
    pub fn new(host: i64, now: f64) -> Self {
        let mut g = Ludo {
            players: vec![LudoPlayer::new(host, 0)],
            status: "lobby".to_string(),
            active: 0,
            winners: vec![],
            events: vec![],
            last_activity: now,
            turn_started: now,
            rolls: vec![],
            sixes: 0,
            dice: None,
        };
        g.event(&format!("{host} opened a lobby"));
        g
    }

    pub fn host(&self) -> i64 {
        self.players[0].user
    }

    pub fn current_player(&self) -> Option<i64> {
        if self.status == "playing" {
            Some(self.players[self.active].user)
        } else {
            None
        }
    }

    fn event(&mut self, text: &str) -> String {
        self.events.push(text.to_string());
        if self.events.len() > 100 {
            let excess = self.events.len() - 100;
            self.events.drain(..excess);
        }
        text.to_string()
    }

    pub fn scoreboard(&self) -> Vec<(i64, &'static str)> {
        let mut out: Vec<(i64, &'static str)> = Vec::new();
        for uid in &self.winners {
            if let Some(p) = self.players.iter().find(|p| p.user == *uid) {
                out.push((p.user, if p.finished { "finished" } else { "resigned" }));
            }
        }
        for p in &self.players {
            if !self.winners.contains(&p.user) {
                out.push((
                    p.user,
                    if p.resigned {
                        "resigned"
                    } else if self.status == "finished" {
                        "finished"
                    } else if self.status == "playing" {
                        "playing"
                    } else {
                        "remaining"
                    },
                ));
            }
        }
        out
    }

    fn require_host(&self, user: i64) -> Result<()> {
        if user != self.host() {
            return Err(Error::Other("Only the host can manage this game".into()));
        }
        Ok(())
    }

    fn seated(&self, user: i64) -> Result<usize> {
        self.players
            .iter()
            .position(|p| p.user == user && !p.resigned && !p.finished)
            .ok_or_else(|| Error::Other("Player is not an active seated player".into()))
    }

    fn check_turn(&self, user: i64) -> Result<()> {
        if self.status != "playing" || self.current_player() != Some(user) {
            return Err(Error::Other("Not your turn".into()));
        }
        self.seated(user)?;
        Ok(())
    }

    fn remaining(&self) -> usize {
        self.players
            .iter()
            .filter(|p| !p.resigned && !p.finished)
            .count()
    }

    fn advance(&mut self, message: &str, now: f64, extra: bool, force: bool) -> String {
        if self.status == "playing" && (!extra || force) {
            for step in 1..=self.players.len() {
                let idx = (self.active + step) % self.players.len();
                if !self.players[idx].resigned && !self.players[idx].finished {
                    self.active = idx;
                    break;
                }
            }
            self.sixes = 0;
        }
        self.turn_started = now;
        self.last_activity = now;
        self.event(message)
    }

    fn remove_active(&mut self, message: &str, now: f64) -> String {
        if self.remaining() <= 1 {
            self.status = "finished".to_string();
            for p in self.players.clone() {
                if !p.resigned && !p.finished && !self.winners.contains(&p.user) {
                    self.winners.push(p.user);
                }
            }
            return self.event(&format!("{message}; match complete"));
        }
        self.advance(message, now, false, true)
    }

    fn blocked(&self, player_idx: usize, progress: i64) -> bool {
        let player = &self.players[player_idx];
        let square = (player.offset + progress).rem_euclid(TRACK);
        self.players.iter().enumerate().any(|(oi, other)| {
            oi != player_idx
                && !other.resigned
                && !other.finished
                && other
                    .tokens
                    .iter()
                    .filter(|pos| {
                        **pos >= 0
                            && **pos < TRACK
                            && (other.offset + **pos).rem_euclid(TRACK) == square
                    })
                    .count()
                    >= 2
        })
    }

    pub fn legal_moves(&self) -> Vec<usize> {
        if self.status != "playing" {
            return vec![];
        }
        let Some(die) = self.dice else {
            return vec![];
        };
        let player = &self.players[self.active];
        let mut result = Vec::new();
        for (i, pos) in player.tokens.iter().enumerate() {
            if *pos == -1 {
                if die == 6 && !self.blocked(self.active, 0) {
                    result.push(i);
                }
            } else if *pos >= 0
                && *pos < FINISH
                && *pos + die as i64 <= FINISH
                && (*pos + 1..=(*pos + die as i64).min(TRACK - 1))
                    .all(|step| !self.blocked(self.active, step))
            {
                result.push(i);
            }
        }
        result
    }

    /// Apply a lobby or turn action; returns the event description.
    /// `roll_die` is the injected die (tests) — `None` rolls randomly.
    pub fn apply(
        &mut self,
        action: &str,
        user: i64,
        now: f64,
        roll_die: Option<u8>,
    ) -> Result<String> {
        match action {
            "join" => {
                if self.status != "lobby"
                    || self.players.iter().any(|p| p.user == user)
                    || self.players.len() >= 4
                {
                    return Err(Error::Other("Cannot join this lobby".into()));
                }
                let offset = (self.players.len() as i64) * 13;
                self.players.push(LudoPlayer::new(user, offset));
                return Ok(self.event(&format!("{user} joined")));
            }
            "leave" => {
                if self.status != "lobby"
                    || user == self.host()
                    || !self.players.iter().any(|p| p.user == user)
                {
                    return Err(Error::Other("Only a guest can leave a lobby".into()));
                }
                self.players.retain(|p| p.user != user);
                for (i, p) in self.players.iter_mut().enumerate() {
                    p.offset = (i as i64) * 13;
                }
                return Ok(self.event(&format!("{user} left")));
            }
            "cancel" => {
                self.require_host(user)?;
                if self.status != "lobby" && self.status != "playing" {
                    return Err(Error::Other("Cannot cancel this game".into()));
                }
                self.status = "cancelled".to_string();
                self.dice = None;
                return Ok(self.event("Game cancelled by host"));
            }
            "start" => {
                self.require_host(user)?;
                if self.status != "lobby" || self.players.len() < 2 {
                    return Err(Error::Other("At least two players are required".into()));
                }
                self.status = "playing".to_string();
                self.active = 0;
                self.last_activity = now;
                self.turn_started = now;
                return Ok(self.event("Game started"));
            }
            "rematch" => {
                if (self.status != "finished" && self.status != "cancelled") || user != self.host()
                {
                    return Err(Error::Other(
                        "Only the host can reopen a finished or closed table".into(),
                    ));
                }
                let host = self.host();
                self.players = vec![LudoPlayer::new(host, 0)];
                self.active = 0;
                self.winners = vec![];
                self.status = "lobby".to_string();
                self.dice = None;
                self.sixes = 0;
                self.rolls = vec![];
                self.last_activity = now;
                self.turn_started = now;
                return Ok(self.event("Rematch lobby opened; players must rejoin"));
            }
            _ => {}
        }
        if self.status != "playing" {
            return Err(Error::Other("Game is not active".into()));
        }
        match action {
            "resign" | "skip" => {
                let idx = self.seated(user)?;
                if action == "skip" {
                    if now - self.last_activity < AFK_SECONDS {
                        return Err(Error::Other("AFK skip threshold has not elapsed".into()));
                    }
                    self.dice = None;
                    return Ok(self.advance(
                        &format!("{user} skipped inactive turn"),
                        now,
                        false,
                        true,
                    ));
                }
                let was_current = self.players[idx].user == self.current_player().unwrap_or(-1);
                self.players[idx].resigned = true;
                self.players[idx].tokens = [-1; TOKENS];
                if self.remaining() <= 1 || was_current {
                    self.dice = None;
                    self.sixes = 0;
                    return Ok(self.remove_active(&format!("{user} resigned"), now));
                }
                Ok(self.event(&format!("{user} resigned; current turn continues")))
            }
            "roll" => {
                self.check_turn(user)?;
                if self.dice.is_some() {
                    return Err(Error::Other("Move a token before rolling again".into()));
                }
                let die = match roll_die {
                    Some(d) if (1..=6).contains(&d) => d,
                    Some(_) => return Err(Error::Other("Dice must be in 1..6".into())),
                    None => (crate::cbs::SmallRng::new(now as u64).below(6) + 1) as u8,
                };
                self.rolls.push(die);
                if self.rolls.len() > 100 {
                    let excess = self.rolls.len() - 100;
                    self.rolls.drain(..excess);
                }
                self.dice = Some(die);
                self.last_activity = now;
                if die == 6 {
                    self.sixes += 1;
                    if self.sixes == 3 {
                        self.dice = None;
                        self.sixes = 0;
                        return Ok(self.advance(
                            "Three sixes: third roll forfeited",
                            now,
                            false,
                            true,
                        ));
                    }
                } else {
                    self.sixes = 0;
                }
                if self.legal_moves().is_empty() {
                    self.dice = None;
                    if die != 6 {
                        self.sixes = 0;
                    }
                    return Ok(self.advance("No legal move; turn passed", now, die == 6, false));
                }
                Ok(self.event(&format!("{user} rolled {die}")))
            }
            _ if action.starts_with("move") => {
                self.check_turn(user)?;
                if self.dice.is_none() {
                    return Err(Error::Other("Roll first".into()));
                }
                let token: usize = action[4..]
                    .parse()
                    .map_err(|_| Error::Other("Use move:0 through move:3".into()))?;
                let moves = self.legal_moves();
                if !moves.contains(&token) {
                    return Err(Error::Other("That token cannot make this move".into()));
                }
                let die = self.dice.unwrap();
                let active = self.active;
                let old = self.players[active].tokens[token];
                let dest = if old == -1 { 0 } else { old + die as i64 };
                self.players[active].tokens[token] = dest;
                let mut captured: Vec<i64> = Vec::new();
                if dest < TRACK {
                    let square = (self.players[active].offset + dest).rem_euclid(TRACK);
                    if !SAFE_SQUARES.contains(&square) {
                        for oi in 0..self.players.len() {
                            if oi == active
                                || self.players[oi].resigned
                                || self.players[oi].finished
                            {
                                continue;
                            }
                            for i in 0..TOKENS {
                                let pos = self.players[oi].tokens[i];
                                if pos >= 0
                                    && pos < TRACK
                                    && (self.players[oi].offset + pos).rem_euclid(TRACK) == square
                                {
                                    self.players[oi].tokens[i] = -1;
                                    captured.push(self.players[oi].user);
                                }
                            }
                        }
                    }
                }
                self.dice = None;
                self.last_activity = now;
                let user_id = self.players[active].user;
                if self.players[active].tokens.iter().all(|p| *p == FINISH) {
                    self.players[active].finished = true;
                    self.winners.push(user_id);
                    return Ok(self.remove_active(&format!("{user_id} finished"), now));
                }
                if !captured.is_empty() {
                    self.turn_started = now;
                    let names: Vec<String> = captured.iter().map(|u| u.to_string()).collect();
                    return Ok(self.event(&format!(
                        "{user_id} captured {}; bonus turn",
                        names.join(", ")
                    )));
                }
                if dest == FINISH {
                    self.turn_started = now;
                    return Ok(self.event(&format!("{user_id} finished a token; bonus turn")));
                }
                if die == 6 {
                    self.turn_started = now;
                    return Ok(self.event(&format!("{user_id} moved token {token}; bonus turn")));
                }
                self.sixes = 0;
                Ok(self.advance(&format!("{user_id} moved token {token}"), now, false, true))
            }
            _ => Err(Error::Other("Unknown action".into())),
        }
    }

    /// Phone-width cross board + numbered token ledger (Python `board_text`).
    pub fn board_text(&self) -> String {
        let path: [(usize, usize); 52] = [
            (6, 1),
            (6, 2),
            (6, 3),
            (6, 4),
            (6, 5),
            (5, 6),
            (4, 6),
            (3, 6),
            (2, 6),
            (1, 6),
            (0, 6),
            (0, 7),
            (0, 8),
            (1, 8),
            (2, 8),
            (3, 8),
            (4, 8),
            (5, 8),
            (6, 9),
            (6, 10),
            (6, 11),
            (6, 12),
            (6, 13),
            (6, 14),
            (7, 14),
            (8, 14),
            (8, 13),
            (8, 12),
            (8, 11),
            (8, 10),
            (8, 9),
            (9, 8),
            (10, 8),
            (11, 8),
            (12, 8),
            (13, 8),
            (14, 8),
            (14, 7),
            (14, 6),
            (13, 6),
            (12, 6),
            (11, 6),
            (10, 6),
            (9, 6),
            (8, 5),
            (8, 4),
            (8, 3),
            (8, 2),
            (8, 1),
            (8, 0),
            (7, 0),
            (6, 0),
        ];
        let lanes: [[(usize, usize); 6]; 4] = [
            [(7, 1), (7, 2), (7, 3), (7, 4), (7, 5), (7, 6)],
            [(1, 7), (2, 7), (3, 7), (4, 7), (5, 7), (6, 7)],
            [(7, 13), (7, 12), (7, 11), (7, 10), (7, 9), (7, 8)],
            [(13, 7), (12, 7), (11, 7), (10, 7), (9, 7), (8, 7)],
        ];
        let yards: [[(usize, usize); 4]; 4] = [
            [(2, 2), (2, 4), (4, 2), (4, 4)],
            [(2, 10), (2, 12), (4, 10), (4, 12)],
            [(10, 10), (10, 12), (12, 10), (12, 12)],
            [(10, 2), (10, 4), (12, 2), (12, 4)],
        ];
        let mut grid = vec![vec!["  ".to_string(); 15]; 15];
        for (square, (r, c)) in path.iter().enumerate() {
            grid[*r][*c] = if SAFE_SQUARES.contains(&(square as i64)) {
                "* ".to_string()
            } else {
                ". ".to_string()
            };
        }
        for (seat, lane) in lanes.iter().enumerate() {
            for (r, c) in lane {
                grid[*r][*c] = format!("{} ", "abcd".chars().nth(seat).unwrap());
            }
        }
        let mut occupied: HashMap<(usize, usize), Vec<String>> = HashMap::new();
        for (seat, p) in self.players.iter().enumerate() {
            if p.resigned {
                continue;
            }
            for (token, pos) in p.tokens.iter().enumerate() {
                let cell = if *pos == -1 {
                    yards[seat][token]
                } else if *pos < TRACK {
                    path[((p.offset + *pos).rem_euclid(TRACK)) as usize]
                } else {
                    lanes[seat][(*pos - TRACK) as usize]
                };
                occupied.entry(cell).or_default().push(format!(
                    "{}{}",
                    "ABCD".chars().nth(seat).unwrap(),
                    token + 1
                ));
            }
        }
        for ((r, c), tokens) in &occupied {
            grid[*r][*c] = if tokens.len() == 1 {
                tokens[0].clone()
            } else if tokens
                .iter()
                .map(|t| t.chars().next().unwrap())
                .collect::<std::collections::HashSet<_>>()
                .len()
                == 1
            {
                format!("{}+", tokens[0].chars().next().unwrap())
            } else {
                "++".to_string()
            };
        }
        grid[7][7] = "HH".to_string();
        let mut rows: Vec<String> = grid
            .iter()
            .map(|line| line.concat().trim_end().to_string())
            .collect();
        rows.push("* safe | + stack | HH home".to_string());
        for (seat, p) in self.players.iter().enumerate() {
            let positions: Vec<String> = p
                .tokens
                .iter()
                .map(|x| {
                    if *x == -1 {
                        "Y".to_string()
                    } else if *x == FINISH {
                        "F".to_string()
                    } else if *x >= TRACK {
                        format!("H{}", *x - TRACK + 1)
                    } else {
                        ((p.offset + *x).rem_euclid(TRACK)).to_string()
                    }
                })
                .collect();
            rows.push(format!(
                "{}: {}",
                "ABCD".chars().nth(seat).unwrap(),
                positions
                    .iter()
                    .enumerate()
                    .map(|(i, v)| format!("{}={}", i + 1, v))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        rows.join("\n")
    }

    pub fn status_text(&self) -> String {
        match self.status.as_str() {
            "lobby" => format!("Ludo lobby: {}/4; host {}", self.players.len(), self.host()),
            "playing" => {
                let mut s = format!("Ludo: {}'s turn", self.current_player().unwrap_or(0));
                if let Some(d) = self.dice {
                    s.push_str(&format!("; rolled {d}"));
                }
                s.push_str(&format!("; rankings {:?}", self.winners));
                s
            }
            _ => format!(
                "Ludo {}: rankings {:?}; seats {:?}",
                self.status,
                self.winners,
                self.scoreboard()
            ),
        }
    }

    pub fn seat_label(seat: usize) -> &'static str {
        match seat {
            0 => "Red",
            1 => "Green",
            2 => "Yellow",
            3 => "Blue",
            _ => "?",
        }
    }
}

// ── Tables ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct Table {
    pub token: String,
    pub chat: i64,
    pub game: Ludo,
    pub message: Option<i64>,
    pub revision: u64,
    pub touched: f64,
    pub confirmation: Option<(i64, String, f64)>,
}

impl Table {
    pub fn control(&self, label: &str, action: &str) -> InlineButton {
        let data = format!("lu:{}:{}:{}", self.token, self.revision, action);
        let mut b = InlineButton::data(label, data.into_bytes());
        if matches!(action, "start" | "join" | "rematch") {
            b = b.styled(ButtonStyle::Success);
        } else if matches!(action, "cancelok" | "resignok") {
            b = b.styled(ButtonStyle::Danger);
        } else if action == "roll" || action.starts_with("move") {
            b = b.styled(ButtonStyle::Primary);
        }
        b
    }
}

fn tables() -> &'static Mutex<HashMap<String, Arc<Mutex<Table>>>> {
    static T: OnceLock<Mutex<HashMap<String, Arc<Mutex<Table>>>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn prune_tables() {
    let now = now_mono();
    let mut map = tables().lock().await;
    let mut dead = Vec::new();
    for (token, table) in map.iter() {
        if let Ok(t) = table.try_lock() {
            if now - t.touched >= TABLE_TTL_SECS {
                dead.push(token.clone());
            }
        }
    }
    for token in dead {
        map.remove(&token);
    }
}

/// Render one table as Markdown (board + status + player cards) and
/// its control keyboard (Python `render`).
pub fn render_table(table: &Table) -> (RichDoc, Vec<Vec<InlineButton>>) {
    let game = &table.game;
    let mut text = format!("🎲 **LUDO / THE COLOUR COURT**\n\n{}\n", game.status_text());
    if game.status == "lobby" {
        text.push_str("\n2–4 humans · four tokens each · no AI opponent\n");
    }
    text.push_str(&format!("\n```\n{}\n```\n", game.board_text()));
    for (seat, p) in game.players.iter().enumerate() {
        let state = if p.resigned {
            "resigned"
        } else if p.finished {
            "finished"
        } else {
            "playing"
        };
        text.push_str(&format!(
            "\n{} {} ({}): tokens {:?} — {state}",
            Ludo::seat_label(seat),
            p.user,
            seat + 1,
            p.tokens
        ));
    }
    if !game.events.is_empty() {
        text.push_str("\n\nArena feed (last six):\n");
        for e in game.events.iter().rev().take(6).rev() {
            text.push_str(&format!("\n- {e}"));
        }
    }
    if let Some((actor, action, _)) = &table.confirmation {
        text.push_str(&format!(
            "\n\nPlayer {actor}: confirm {action}. This confirmation expires in 20 seconds."
        ));
    }
    text.push_str(&format!(
        "\n\nNative Layer 230 · table {} · revision {} · /ludo",
        table.token, table.revision
    ));
    let doc = RichDoc::markdown(text)
        .unwrap_or_else(|_| RichDoc::markdown("Ludo table").expect("static markdown"));

    let mut rows: Vec<Vec<InlineButton>> = Vec::new();
    let ctl = |t: &Table, label: &str, action: &str| t.control(label, action);
    match game.status.as_str() {
        "lobby" => {
            rows.push(vec![
                ctl(table, "＋ Join table", "join"),
                ctl(table, "− Leave seat", "leave"),
            ]);
            rows.push(vec![ctl(table, "▶ Start · host", "start")]);
            rows.push(vec![ctl(table, "■ Close table · host", "cancel")]);
        }
        "playing" => {
            let seat = game.active;
            if game.dice.is_none() {
                rows.push(vec![ctl(
                    table,
                    &format!("🎲 Roll · {}", Ludo::seat_label(seat)),
                    "roll",
                )]);
            } else {
                let moves = game.legal_moves();
                for chunk in moves.chunks(2) {
                    rows.push(
                        chunk
                            .iter()
                            .map(|i| {
                                ctl(
                                    table,
                                    &format!("Move {}{}", (b'A' + seat as u8) as char, i + 1),
                                    &format!("move{i}"),
                                )
                            })
                            .collect(),
                    );
                }
            }
            rows.push(vec![ctl(table, "◷ Skip AFK turn", "skip")]);
            rows.push(vec![ctl(table, "⚑ Resign", "resign")]);
            rows.push(vec![ctl(table, "■ Close · host", "cancel")]);
        }
        _ => {
            rows.push(vec![ctl(table, "↺ Rematch lobby · host", "rematch")]);
        }
    }
    if let Some((_, action, _)) = &table.confirmation {
        rows.push(vec![
            ctl(table, &format!("Confirm {action}"), &format!("{action}ok")),
            ctl(table, "Keep playing", "back"),
        ]);
    }
    (doc, rows)
}

/// Validate callback data against the pattern (Python `PATTERN`).
fn valid_ludo_data(data: &str) -> bool {
    if data == "lu:new" {
        return true;
    }
    let parts: Vec<&str> = data.split(':').collect();
    if parts.len() != 4 || parts[0] != "lu" {
        return false;
    }
    let (token, rev, action) = (parts[1], parts[2], parts[3]);
    token.len() == 12
        && token.chars().all(|c| c.is_ascii_hexdigit())
        && !rev.is_empty()
        && rev.chars().all(|c| c.is_ascii_digit())
        && LUDO_ACTIONS.contains(&action)
}

/// Open a new table in a group (Python `launch`).
pub async fn launch_table(ctx: &Ctx) -> Result<i64> {
    if !matches!(
        ctx.chat_type,
        ultroid_core::ChatType::Group | ultroid_core::ChatType::Supergroup
    ) || ctx.sender_id <= 0
    {
        return Err(Error::Other(
            "Ludo is for human players in group chats. Add the assistant to your group and send /ludo there".to_string(),
        ));
    }
    prune_tables().await;
    {
        let map = tables().lock().await;
        if map
            .values()
            .any(|t| t.try_lock().map(|t| t.chat == ctx.chat_id).unwrap_or(false))
        {
            return Err(Error::Other(
                "This group already has a Ludo table. Use its current controls, or let it expire after one hour idle".to_string(),
            ));
        }
        if map.len() >= TABLE_CAPACITY {
            return Err(Error::Other(
                "All Ludo tables are occupied. Try later".to_string(),
            ));
        }
    }
    let token = token_hex_6();
    let table = Arc::new(Mutex::new(Table {
        token: token.clone(),
        chat: ctx.chat_id,
        game: Ludo::new(ctx.sender_id, now_mono()),
        message: None,
        revision: 0,
        touched: now_mono(),
        confirmation: None,
    }));
    tables().lock().await.insert(token.clone(), table.clone());
    let result = {
        let t = table.lock().await;
        let (doc, _kb) = render_table(&t);
        let sent = ctx.send_rich_or_plain(&doc).await;
        match sent {
            Ok(id) => Ok(id),
            Err(e) => Err(e),
        }
    };
    match result {
        Ok(id) => {
            table.lock().await.message = Some(id);
            Ok(id)
        }
        Err(e) => {
            tables().lock().await.remove(&token);
            Err(e)
        }
    }
}

/// `/ludo` — open a native shared Ludo lobby for this group's human players.
pub async fn ludo(ctx: &Ctx) -> Result<()> {
    match launch_table(ctx).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let doc = RichDoc::markdown(format!("Ludo table\n\n{e}"))?;
            ctx.send_rich_or_plain(&doc).await?;
            Err(e)
        }
    }
}

/// Publish a candidate before committing state (Python `commit`):
/// the edit must be accepted by Telegram first; a timeout expires
/// the table rather than replaying an uncertain turn.
async fn commit_table(
    ctx: &Ctx,
    table: &Arc<Mutex<Table>>,
    candidate_game: Ludo,
    candidate_confirmation: Option<(i64, String, f64)>,
) -> Result<bool> {
    let (doc, message) = {
        let t = table.lock().await;
        let tmp = Table {
            token: t.token.clone(),
            chat: t.chat,
            game: candidate_game.clone(),
            message: t.message,
            revision: t.revision + 1,
            touched: t.touched,
            confirmation: candidate_confirmation.clone(),
        };
        let (doc, _kb) = render_table(&tmp);
        (doc, t.message)
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
            let mut t = table.lock().await;
            t.game = candidate_game;
            t.revision += 1;
            t.confirmation = candidate_confirmation;
            t.touched = now_mono();
            Ok(true)
        }
        Ok(Err(_)) => Ok(false), // MessageNotModified equivalent: do not commit.
        Err(_) => {
            // Timeout: an edit may or may not have landed — expire,
            // never replay.
            let token = table.lock().await.token.clone();
            tables().lock().await.remove(&token);
            Err(Error::Other(
                "Telegram did not confirm the edit. This table expired rather than replaying an uncertain turn".to_string(),
            ))
        }
    }
}

/// Dispatch `lu:*` callback data. Returns `true` when the data
/// belonged here. Alert answers are delivered as replies (see
/// `crate::cbs` — `Ctx` has no callback-answer channel).
pub async fn handle_callback(ctx: &mut Ctx, data: &str) -> Result<bool> {
    if !valid_ludo_data(data) {
        return Ok(false);
    }
    if !matches!(
        ctx.chat_type,
        ultroid_core::ChatType::Group | ultroid_core::ChatType::Supergroup
    ) || ctx.sender_id <= 0
    {
        ctx.reply("Use Ludo controls as a human player inside the original group")
            .await?;
        return Ok(true);
    }
    if data == "lu:new" {
        match launch_table(ctx).await {
            Ok(_) => {
                ctx.reply("Ludo lobby opened").await?;
            }
            Err(e) => {
                ctx.reply(&e.to_string()).await?;
            }
        }
        return Ok(true);
    }
    prune_tables().await;
    let parts: Vec<&str> = data.split(':').collect();
    let (token, revision, action) = (parts[1], parts[2], parts[3]);
    let table = match tables().lock().await.get(token).cloned() {
        Some(t) => t,
        None => {
            ctx.reply("Table expired or the bot restarted. Send /ludo for a new lobby")
                .await?;
            return Ok(true);
        }
    };
    let revision: u64 = revision.parse().unwrap_or(u64::MAX);
    // The per-table lock: if another turn is updating, answer and stop.
    // Bind the cloned Arc first: `table.clone().try_lock()` drops the
    // temporary Arc while its guard is still borrowed (E0716).
    let table = table.clone();
    let Ok(t) = table.try_lock() else {
        ctx.reply("A turn is updating; use the newest controls in a moment")
            .await?;
        return Ok(true);
    };
    if (t.chat, t.message) != (ctx.chat_id, Some(ctx.message_id)) {
        ctx.reply("These controls belong to another group or message")
            .await?;
        return Ok(true);
    }
    if revision != t.revision {
        ctx.reply("Old board. Use the newest controls on this table")
            .await?;
        return Ok(true);
    }
    let now = now_mono();
    let mut candidate = t.game.clone();
    let mut confirmation = t.confirmation.clone();
    let outcome: Result<String> = (|| match action {
        "cancel" | "resign" => {
            if action == "cancel" && ctx.sender_id != candidate.host() {
                return Err(Error::Other("Only the host may close this table".into()));
            }
            if action == "resign"
                && (candidate.status != "playing"
                    || !candidate
                        .players
                        .iter()
                        .any(|p| p.user == ctx.sender_id && !p.resigned && !p.finished))
            {
                return Err(Error::Other(
                    "Only a seated player in a running game can resign".into(),
                ));
            }
            confirmation = Some((ctx.sender_id, action.to_string(), now + 20.0));
            Ok(String::new())
        }
        _ if action.ends_with("ok") => {
            let base = &action[..action.len() - 2];
            let expected = (ctx.sender_id, base.to_string());
            match &confirmation {
                Some((actor, act, deadline))
                    if (*actor, act.clone()) == expected && now <= *deadline => {}
                _ => {
                    return Err(Error::Other(
                        "That confirmation expired or belongs to another player".into(),
                    ));
                }
            }
            let msg = candidate.apply(base, ctx.sender_id, now, None)?;
            confirmation = None;
            Ok(msg)
        }
        "back" => {
            match &confirmation {
                Some((actor, _, _)) if *actor == ctx.sender_id => {}
                _ => {
                    return Err(Error::Other(
                        "Only the player who requested confirmation may dismiss it".into(),
                    ));
                }
            }
            confirmation = None;
            Ok(String::new())
        }
        _ => {
            let msg = candidate.apply(action, ctx.sender_id, now, None)?;
            confirmation = None;
            Ok(msg)
        }
    })();
    match outcome {
        Err(e) => {
            ctx.reply(&e.to_string()).await?;
            Ok(true)
        }
        Ok(_) => {
            drop(t);
            let committed = commit_table(ctx, &table, candidate, confirmation).await;
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
            "ludo",
            "ludo",
            "Open a native shared Ludo lobby for this group's human players.",
        ),
        |ctx| Box::pin(ludo(ctx)),
    );
}
