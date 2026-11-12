//! Queue/player state machine. Port of `vc/_player.py` (413 LOC),
//! which is PyTgCalls-free: pure queue + clock.
//!
//! `PlayerState`: start / pause / resume with clock rebase, finish with
//! loop + rotate, history cap 50, stop, snapshot, position, progress.

use crate::dataclass::CachedTrack;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// The clock state of one chat's playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Playing,
    Paused,
    Finished,
}

/// Per-chat player (one `PlayerState` per chat in a `PlayerRegistry`).
#[derive(Debug)]
pub struct PlayerState {
    pub chat_id: i64,
    pub queue: VecDeque<CachedTrack>,
    pub current: Option<CachedTrack>,
    pub history: VecDeque<CachedTrack>,
    pub state: PlaybackState,
    pub loop_enabled: bool,
    /// Instant playback started (or resumed) at, minus elapsed offset.
    started_at: Option<Instant>,
    /// Seconds already played before the last pause.
    elapsed_before_pause: Duration,
}

impl PlayerState {
    pub fn new(chat_id: i64) -> Self {
        PlayerState {
            chat_id,
            queue: VecDeque::new(),
            current: None,
            history: VecDeque::new(),
            state: PlaybackState::Idle,
            loop_enabled: false,
            started_at: None,
            elapsed_before_pause: Duration::ZERO,
        }
    }

    pub fn enqueue(&mut self, track: CachedTrack) {
        self.queue.push_back(track);
    }

    /// Start the next track (loop rotates the current to the tail first).
    pub fn start_next(&mut self) -> Option<CachedTrack> {
        if let Some(cur) = self.current.take() {
            self.history.push_back(cur.clone());
            if self.history.len() > 50 {
                self.history.pop_front();
            }
            if self.loop_enabled {
                self.queue.push_back(cur);
            }
        }
        let next = self.queue.pop_front()?;
        self.current = Some(next.clone());
        self.started_at = Some(Instant::now());
        self.elapsed_before_pause = Duration::ZERO;
        self.state = PlaybackState::Playing;
        Some(next)
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.elapsed_before_pause = self.position();
            self.started_at = None;
            self.state = PlaybackState::Paused;
        }
    }

    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            self.started_at = Some(Instant::now());
            self.state = PlaybackState::Playing;
        }
    }

    /// Finish the current track and advance (loop + rotate handled in
    /// `start_next`).
    pub fn finish_current(&mut self) -> Option<CachedTrack> {
        self.start_next()
    }

    pub fn stop(&mut self) {
        self.current = None;
        self.queue.clear();
        self.started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
        self.state = PlaybackState::Idle;
    }

    /// Seconds played in the current track (clock rebase on pause/resume).
    pub fn position(&self) -> Duration {
        match self.started_at {
            Some(t) => self.elapsed_before_pause + t.elapsed(),
            None => self.elapsed_before_pause,
        }
    }

    /// 0.0-1.0 progress, or None when no duration is known.
    pub fn progress(&self) -> Option<f32> {
        let cur = self.current.as_ref()?;
        if cur.duration == 0 {
            return None;
        }
        Some((self.position().as_secs_f32() / cur.duration as f32).min(1.0))
    }

    pub fn is_playing(&self) -> bool {
        self.state == PlaybackState::Playing
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        PlayerSnapshot {
            chat_id: self.chat_id,
            current: self.current.clone(),
            queued: self.queue.iter().cloned().collect(),
            state: self.state,
            loop_enabled: self.loop_enabled,
            position_secs: self.position().as_secs(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlayerSnapshot {
    pub chat_id: i64,
    pub current: Option<CachedTrack>,
    pub queued: Vec<CachedTrack>,
    pub state: PlaybackState,
    pub loop_enabled: bool,
    pub position_secs: u64,
}

/// Registry of per-chat players.
#[derive(Debug, Default)]
pub struct PlayerRegistry {
    players: HashMap<i64, PlayerState>,
}

impl PlayerRegistry {
    pub fn new() -> Self {
        PlayerRegistry::default()
    }

    pub fn player(&mut self, chat_id: i64) -> &mut PlayerState {
        self.players.entry(chat_id).or_insert_with(|| PlayerState::new(chat_id))
    }

    pub fn get(&self, chat_id: i64) -> Option<&PlayerState> {
        self.players.get(&chat_id)
    }

    pub fn remove(&mut self, chat_id: i64) {
        self.players.remove(&chat_id);
    }
}
