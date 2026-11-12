//! Per-chat play queue cache. Port of `vc/_cacher.py` (`ChatCacher`).

use crate::dataclass::CachedTrack;
use std::collections::{HashMap, VecDeque};

/// A chat's cached queue: a deque of tracks + active/loop state.
#[derive(Debug, Default)]
pub struct ChatQueue {
    pub queue: VecDeque<CachedTrack>,
    pub is_active: bool,
    pub loop_count: u32,
}

/// In-memory per-chat queue cache (the DB holds durable state; this is
/// the hot cache the player reads).
#[derive(Debug, Default)]
pub struct ChatCacher {
    chats: HashMap<i64, ChatQueue>,
}

impl ChatCacher {
    pub fn new() -> Self {
        ChatCacher::default()
    }

    pub fn queue(&self, chat_id: i64) -> Option<&ChatQueue> {
        self.chats.get(&chat_id)
    }

    pub fn queue_mut(&mut self, chat_id: i64) -> &mut ChatQueue {
        self.chats.entry(chat_id).or_default()
    }

    pub fn push(&mut self, chat_id: i64, track: CachedTrack) {
        self.queue_mut(chat_id).queue.push_back(track);
    }

    pub fn pop_front(&mut self, chat_id: i64) -> Option<CachedTrack> {
        self.chats.get_mut(&chat_id)?.queue.pop_front()
    }

    pub fn peek(&self, chat_id: i64) -> Option<&CachedTrack> {
        self.chats.get(&chat_id)?.queue.front()
    }

    pub fn len(&self, chat_id: i64) -> usize {
        self.chats.get(&chat_id).map(|q| q.queue.len()).unwrap_or(0)
    }

    pub fn clear(&mut self, chat_id: i64) {
        if let Some(q) = self.chats.get_mut(&chat_id) {
            q.queue.clear();
            q.is_active = false;
        }
    }

    pub fn remove(&mut self, chat_id: i64) {
        self.chats.remove(&chat_id);
    }

    pub fn is_active(&self, chat_id: i64) -> bool {
        self.chats
            .get(&chat_id)
            .map(|q| q.is_active)
            .unwrap_or(false)
    }

    pub fn set_active(&mut self, chat_id: i64, active: bool) {
        self.queue_mut(chat_id).is_active = active;
    }
}
