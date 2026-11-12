//! Port of `plugins/vctools.py` (21 LOC) — a thin note, not a command set.
//!
//! The Python file contains no commands at all: it only documents that
//! the group-call commands and the voice-chat music player live in
//! `plugins/vcmusic.py` (here: `crate::vcmusic`) and the `vc/` engine
//! (here: the `ultroid-vc` crate). This module ports that meaning
//! faithfully: it registers nothing and invents no commands. The command
//! list it points at is recorded in [`VCMUSIC_COMMANDS`] so the routing
//! stays discoverable.

use ultroid_core::Registry;

/// The group-call / music commands, defined once in `crate::vcmusic`
/// (Python: `plugins/vcmusic.py`).
pub const VCMUSIC_COMMANDS: &[&str] = &[
    "startvc", "stopvc", "vctitle", "vcinvite", // group-call plumbing
    "vplay", "vskip", "vpause", "vresume", // transport
    "vqueue", "vnow", "vclear", "vloop", // queue control
    "vcmute", "vcunmute",
];

/// Intentionally registers nothing: the group-call commands live in
/// `crate::vcmusic`, backed by the `ultroid-vc` engine.
pub fn register(_registry: &mut Registry) {}
