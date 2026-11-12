//! Actor commands for the voice engine thread.
//!
//! The `Engine` is `Send` but not `Sync` and lives in its own OS
//! thread; plugins reach it through a `VcControl` handle in the
//! binary, which sends one of these commands over an mpsc channel
//! and awaits the oneshot reply. See `ultroid_core::vc`.

use crate::dataclass::CachedTrack;
use tokio::sync::oneshot;
use ultroid_core::{Error, Result};

/// One request to the engine thread. The `reply` carries the engine
/// method's result; senders ignore a dropped reply (caller gone).
pub enum VcCommand {
    Enqueue {
        chat_id: i64,
        track: CachedTrack,
        reply: oneshot::Sender<Result<bool>>,
    },
    PlayNext {
        chat_id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    Pause {
        chat_id: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    Resume {
        chat_id: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    Mute {
        chat_id: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    Unmute {
        chat_id: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    End {
        chat_id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
}

impl VcCommand {
    /// Which chat this command targets (for logging).
    pub fn chat_id(&self) -> i64 {
        match self {
            VcCommand::Enqueue { chat_id, .. }
            | VcCommand::PlayNext { chat_id, .. }
            | VcCommand::Pause { chat_id, .. }
            | VcCommand::Resume { chat_id, .. }
            | VcCommand::Mute { chat_id, .. }
            | VcCommand::Unmute { chat_id, .. }
            | VcCommand::End { chat_id, .. } => *chat_id,
        }
    }
}

/// Error returned to a caller when the engine thread's receiver was
/// dropped before the command could be answered.
pub fn engine_gone() -> Error {
    Error::Other("voice engine is not running (command channel closed)".to_string())
}
