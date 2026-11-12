//! Arcade games — port of `pyUltroid/_core/ludo_arcade.py`,
//! `ludo_visuals.py`, `matrix_arcade.py` (game state machines; the
//! assistant-side UI lives in `ultroid-assistant`).

use rand::Rng;

/// Ludo game state (2-4 players, standard rules, simplified).
#[derive(Debug, Clone)]
pub struct LudoGame {
    pub players: Vec<i64>,
    pub positions: Vec<i64>, // -1 = home, 0..=56 track, 57 = finished
    pub turn: usize,
}

impl LudoGame {
    pub fn new(players: Vec<i64>) -> Self {
        LudoGame {
            positions: vec![-1; players.len()],
            players,
            turn: 0,
        }
    }

    pub fn roll_dice() -> u8 {
        rand::thread_rng().gen_range(1..=6)
    }

    /// Move the current player's token by `dice`. Returns true if the move won.
    pub fn play(&mut self, dice: u8) -> bool {
        let pos = &mut self.positions[self.turn];
        if *pos == -1 {
            if dice == 6 {
                *pos = 0;
            }
        } else {
            *pos += dice as i64;
            if *pos >= 57 {
                *pos = 57;
                return true;
            }
        }
        self.turn = (self.turn + 1) % self.players.len();
        false
    }
}

/// Matrix arcade: a simple falling-code score game state.
#[derive(Debug, Clone, Default)]
pub struct MatrixGame {
    pub score: u64,
    pub level: u32,
}

impl MatrixGame {
    pub fn new() -> Self {
        MatrixGame { score: 0, level: 1 }
    }

    pub fn tick(&mut self, hits: u64) {
        self.score += hits * self.level as u64;
        if self.score > (self.level as u64) * 1000 {
            self.level += 1;
        }
    }
}
