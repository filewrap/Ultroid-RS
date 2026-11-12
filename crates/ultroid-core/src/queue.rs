//! Download/upload queue — port of `pyUltroid/_core/queue.py` (`QueueManager`).

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Download,
    Upload,
}

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: String,
    pub kind: QueueKind,
    pub chat_id: i64,
    pub url_or_path: String,
    pub status: String,
}

#[derive(Clone, Default)]
pub struct QueueManager {
    inner: Arc<Mutex<VecDeque<QueueItem>>>,
}

impl QueueManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn enqueue(&self, item: QueueItem) {
        self.inner.lock().await.push_back(item);
    }

    pub async fn dequeue(&self) -> Option<QueueItem> {
        self.inner.lock().await.pop_front()
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn list(&self) -> Vec<QueueItem> {
        self.inner.lock().await.iter().cloned().collect()
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let mut q = self.inner.lock().await;
        if let Some(pos) = q.iter().position(|i| i.id == id) {
            q.remove(pos);
            true
        } else {
            false
        }
    }
}
