//! User-facing VC message texts. Port of `vc/_messages.py`
//! (`PARSEMODE="v2"` — Telegram MarkdownV2 escaping).
//!
//! `esc()` escapes `& < \` * _ ~ | [ ]` and a leading `>` as `&#62;`,
//! mirroring the Python helper exactly.

/// MarkdownV2-escape a literal, mirroring Python `esc()`.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' | '<' | '\\' | '*' | '_' | '~' | '|' | '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            '>' if out.is_empty() => out.push_str("&#62;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn fmt_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "LIVE".to_string();
    }
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn status_msg(track: &str, duration: u64, by: &str) -> String {
    format!(
        "🎵 **Now Playing**\n\n{} \\| {}\nRequested by {}",
        esc(track),
        fmt_duration(duration),
        esc(by)
    )
}

pub fn now_playing_msg(track: &str, duration: u64, position: u64, by: &str) -> String {
    format!(
        "🎵 **Now Playing**\n\n{} \\| {} \\/ {}\nRequested by {}",
        esc(track),
        fmt_duration(position),
        fmt_duration(duration),
        esc(by)
    )
}

pub fn queued_msg(track: &str, position: usize, by: &str) -> String {
    format!(
        "➕ **Queued** \\#{}: {}\nRequested by {}",
        position,
        esc(track),
        esc(by)
    )
}

pub fn finished_msg(track: &str) -> String {
    format!("✅ Finished: {}", esc(track))
}

pub fn nothing_playing_msg() -> String {
    "❌ Nothing is playing right now.".to_string()
}

pub fn queue_list_msg(tracks: &[(String, u64)]) -> String {
    if tracks.is_empty() {
        return "📋 Queue is empty.".to_string();
    }
    let mut out = String::from("📋 **Queue**\n");
    for (i, (title, dur)) in tracks.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} \\| {}",
            i + 1,
            esc(title),
            fmt_duration(*dur)
        ));
    }
    out
}

pub fn error_msg(detail: &str) -> String {
    format!("❌ {}", esc(detail))
}

pub fn paused_msg(track: &str) -> String {
    format!("⏸ Paused: {}", esc(track))
}

pub fn resumed_msg(track: &str) -> String {
    format!("▶️ Resumed: {}", esc(track))
}

pub fn loop_msg(enabled: bool) -> String {
    if enabled {
        "🔁 Loop enabled.".to_string()
    } else {
        "🔁 Loop disabled.".to_string()
    }
}

pub fn cleared_msg() -> String {
    "🗑 Queue cleared.".to_string()
}

pub fn mute_msg(muted: bool) -> String {
    if muted {
        "🔇 Muted.".to_string()
    } else {
        "🔊 Unmuted.".to_string()
    }
}
