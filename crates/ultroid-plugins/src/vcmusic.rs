//! Port of `plugins/vcmusic.py` — group-call plumbing + voice-chat music.
//!
//! `vplay <query|url>`, `vskip`, `vpause`, `vresume`, `vqueue`, `vnow`,
//! `vclear`, `vloop <n|off>`, `vcmute`, `vcunmute` (all groups-only),
//! plus `startvc`, `stopvc`, `vctitle <title>`, `vcinvite`
//! (group-call plumbing; `stopvc`/`vctitle` admins-only).
//!
//! Wired to the already-ported `ultroid-vc` crate: query classification
//! (`query::classify`), queue/player state (`player::PlayerRegistry` /
//! `PlayerState`, one per chat, in memory), track data
//! (`dataclass::CachedTrack`), and the user-facing texts
//! (`messages::*`). Track resolution is real `yt-dlp` over
//! `tokio::process::Command` (search, URL, or bare video id; no fake
//! results — a failed extraction reports the real cause).
//!
//! The audio path reaches the `ultroid-vc` `Engine` (ntgcalls +
//! grammers `phone.*` signalling) through `Ctx.vc` — the actor handle
//! the binary hands every `Ctx` (mpsc to the engine thread). Each
//! command does its local work (resolve, queue, state) and then sends
//! the matching actor command; when no engine is running (`ctx.vc`
//! is `None`, e.g. bot-only mode) the command errors honestly instead
//! of pretending the track is playing. Queue-only commands (`vqueue`,
//! `vnow`, `vloop`) work fully against the local state.

use crate::util::{def, reg_add};
use std::sync::OnceLock;
use tokio::process::Command as ProcCommand;
use tokio::sync::Mutex;
use ultroid_core::vc::{VcHandle, VcTrack};
use ultroid_core::{ChatType, Ctx, Error, Registry, Result};
use ultroid_vc::dataclass::{CachedTrack, ChannelPlay};
use ultroid_vc::messages as msg;
use ultroid_vc::player::{PlayerRegistry, PlayerState};
use ultroid_vc::query::{QueryKind, classify};

pub fn register(registry: &mut Registry) {
    let groups = |name: &'static str, pattern: &'static str, help: &'static str| {
        let mut d = def(name, pattern, help);
        d.groups_only = true;
        d
    };
    reg_add(
        registry,
        groups(
            "vplay",
            "vplay(?: |$)(.*)",
            "Play a track in the group voice chat: `.vplay <query or url>`.",
        ),
        |ctx| Box::pin(vplay(ctx)),
    );
    reg_add(
        registry,
        groups("vskip", "vskip$", "Skip the current track."),
        |ctx| Box::pin(vskip(ctx)),
    );
    reg_add(
        registry,
        groups("vpause", "vpause$", "Pause playback."),
        |ctx| Box::pin(vpause(ctx)),
    );
    reg_add(
        registry,
        groups("vresume", "vresume$", "Resume playback."),
        |ctx| Box::pin(vresume(ctx)),
    );
    reg_add(
        registry,
        groups("vqueue", "vqueue$", "Show the queue."),
        |ctx| Box::pin(vqueue(ctx)),
    );
    reg_add(
        registry,
        groups("vnow", "vnow$", "Show the current track with elapsed time."),
        |ctx| Box::pin(vnow(ctx)),
    );
    reg_add(
        registry,
        groups("vclear", "vclear$", "Clear the queue and stop playback."),
        |ctx| Box::pin(vclear(ctx)),
    );
    reg_add(
        registry,
        groups(
            "vloop",
            "vloop(?: |$)(.*)",
            "Loop the current track n times: `.vloop <n|off>`.",
        ),
        |ctx| Box::pin(vloop(ctx)),
    );
    reg_add(
        registry,
        groups(
            "vcmute",
            "vcmute$",
            "Mute the bot's stream in the group call.",
        ),
        |ctx| Box::pin(vcmute(ctx)),
    );
    reg_add(
        registry,
        groups(
            "vcunmute",
            "vcunmute$",
            "Unmute the bot's stream in the group call.",
        ),
        |ctx| Box::pin(vcunmute(ctx)),
    );

    let mut startvc_def = def("startvc", "startvc$", "Start a voice chat in this group.");
    startvc_def.groups_only = true;
    reg_add(registry, startvc_def, |ctx| Box::pin(startvc(ctx)));

    let mut stopvc_def = def("stopvc", "stopvc$", "Stop the voice chat in this group.");
    stopvc_def.groups_only = true;
    stopvc_def.admins_only = true;
    reg_add(registry, stopvc_def, |ctx| Box::pin(stopvc(ctx)));

    let mut vctitle_def = def(
        "vctitle",
        "vctitle(?: |$)(.*)",
        "Set the voice chat title: `.vctitle <title>`.",
    );
    vctitle_def.groups_only = true;
    vctitle_def.admins_only = true;
    reg_add(registry, vctitle_def, |ctx| Box::pin(vctitle(ctx)));

    let mut vcinvite_def = def(
        "vcinvite",
        "vcinvite$",
        "Invite all group members to the voice chat.",
    );
    vcinvite_def.groups_only = true;
    reg_add(registry, vcinvite_def, |ctx| Box::pin(vcinvite(ctx)));
}

static PLAYERS: OnceLock<Mutex<PlayerRegistry>> = OnceLock::new();

fn players() -> &'static Mutex<PlayerRegistry> {
    PLAYERS.get_or_init(|| Mutex::new(PlayerRegistry::new()))
}

/// The engine actor handle, or an honest error when the bot runs
/// without a user/VC client (bot-only mode: no engine thread exists).
fn vc(ctx: &Ctx) -> Result<VcHandle> {
    ctx.vc.clone().ok_or_else(|| {
        Error::Other(
            "Voice engine is not running — this bot has no user/VC client connected (Python: vcmusic.py)"
                .to_string(),
        )
    })
}

/// Convert the plugin's `CachedTrack` to the actor's plain-data `VcTrack`.
fn to_vc_track(t: &CachedTrack) -> VcTrack {
    let mut vt = VcTrack::new(
        t.url.clone(),
        t.name.clone(),
        t.artist.clone(),
        t.user.clone(),
        t.file_path.clone(),
        t.thumbnail.clone(),
        t.track_id.clone(),
        t.duration,
        t.is_video,
        t.platform.clone(),
        t.channel.as_ref().map(|c| (c.chat_id, c.is_channel)),
    );
    vt.loop_count = t.loop_count;
    vt
}

fn requester(ctx: &Ctx) -> String {
    format!("@{}", ctx.sender_id)
}

/// Resolve a user query to a `CachedTrack` with real `yt-dlp`
/// (Python `vcmusic.py:_resolve_to_track` via `vc._youtube.YouTubeData`).
/// Returns `Ok(None)` when nothing matched.
async fn resolve_track(ctx: &Ctx, query: &str) -> Result<Option<CachedTrack>> {
    let kind = classify(query);
    let input = match kind {
        QueryKind::Search => format!("ytsearch1:{query}"),
        QueryKind::VideoId => format!("https://www.youtube.com/watch?v={query}"),
        QueryKind::Url => query.to_string(),
    };
    let out = ProcCommand::new("yt-dlp")
        .args(["-j", "--no-playlist", "--no-warnings", &input])
        .output()
        .await;
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return Err(Error::Other(format!(
                "yt-dlp failed to run ({e}); is yt-dlp installed?"
            )));
        }
    };
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        ctx.reply(&msg::error_msg(&format!(
            "extraction failed: {}",
            if detail.is_empty() {
                "no results".to_string()
            } else {
                detail
            }
        )))
        .await?;
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(line) = stdout.lines().next() else {
        return Ok(None);
    };
    let info: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let name = info
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let artist = info
        .get("uploader")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown artist")
        .to_string();
    let url = info
        .get("webpage_url")
        .and_then(|v| v.as_str())
        .unwrap_or(query)
        .to_string();
    let track_id = info
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let duration = info.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
    let thumbnail = info
        .get("thumbnail")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(Some(CachedTrack {
        url,
        name,
        artist,
        loop_count: 0,
        user: requester(ctx),
        file_path: None,
        thumbnail,
        track_id,
        duration,
        is_video: false,
        platform: "youtube".to_string(),
        channel: Some(ChannelPlay {
            chat_id: ctx.chat_id,
            is_channel: ctx.chat_type == ChatType::Channel,
        }),
    }))
}

async fn vplay(ctx: &Ctx) -> Result<()> {
    let query = ctx.args.trim();
    if query.is_empty() {
        ctx.reply(&format!(
            "**Usage**\n\n`{}` <query or url>",
            ctx.text.split_whitespace().next().unwrap_or(".vplay")
        ))
        .await?;
        return Ok(());
    }
    let Some(track) = resolve_track(ctx, query).await? else {
        ctx.reply(&format!("**No results**\n\nQuery: `{query}`\nTry a YouTube URL, a video id, or a different search."))
            .await?;
        return Ok(());
    };
    let (was_idle, queued_pos) = {
        let mut reg = players().lock().await;
        let player: &mut PlayerState = reg.player(ctx.chat_id);
        let was_idle = player.current.is_none();
        player.enqueue(track.clone());
        if was_idle {
            player.start_next();
        }
        (was_idle, player.queue.len())
    };
    if was_idle {
        ctx.reply(&msg::status_msg(
            &track.display(),
            track.duration,
            &track.user,
        ))
        .await?;
    } else {
        ctx.reply(&msg::queued_msg(&track.display(), queued_pos, &track.user))
            .await?;
    }
    // Start the audio on the engine (joins the group call on first
    // play; `enqueue` starts immediately when the engine is idle).
    // Python: vcmusic.py:vplay, `call._play_song`.
    vc(ctx)?.enqueue(ctx.chat_id, to_vc_track(&track)).await?;
    Ok(())
}

async fn vskip(ctx: &Ctx) -> Result<()> {
    let done = {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        let Some(cur) = player.current.clone() else {
            ctx.reply(&msg::nothing_playing_msg()).await?;
            return Ok(());
        };
        player.finish_current();
        let next = player.current.clone();
        (cur, next)
    };
    let (done, next) = done;
    ctx.reply(&msg::finished_msg(&done.display())).await?;
    if let Some(n) = next {
        ctx.reply(&msg::status_msg(&n.display(), n.duration, &n.user))
            .await?;
    }
    // Start the next track's audio on the engine
    // (Python: vcmusic.py:vskip, `call.play_next`).
    vc(ctx)?.play_next(ctx.chat_id).await?;
    Ok(())
}

async fn vpause(ctx: &Ctx) -> Result<()> {
    let track = {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        if !player.is_playing() {
            ctx.reply(&msg::nothing_playing_msg()).await?;
            return Ok(());
        }
        let t = player.current.as_ref().map(|c| c.display());
        player.pause();
        t
    };
    if let Some(t) = track {
        ctx.reply(&msg::paused_msg(&t)).await?;
    }
    // Pause the audio stream on the engine
    // (Python: vcmusic.py:vpause, `call.pause`).
    vc(ctx)?.pause(ctx.chat_id).await?;
    Ok(())
}

async fn vresume(ctx: &Ctx) -> Result<()> {
    let track = {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        let paused = matches!(
            player.snapshot().state,
            ultroid_vc::player::PlaybackState::Paused
        );
        if !paused {
            ctx.reply(&msg::nothing_playing_msg()).await?;
            return Ok(());
        }
        let t = player.current.as_ref().map(|c| c.display());
        player.resume();
        t
    };
    if let Some(t) = track {
        ctx.reply(&msg::resumed_msg(&t)).await?;
    }
    // Resume the audio stream on the engine
    // (Python: vcmusic.py:vresume, `call.resume`).
    vc(ctx)?.resume(ctx.chat_id).await?;
    Ok(())
}

async fn vqueue(ctx: &Ctx) -> Result<()> {
    let tracks: Vec<(String, u64)> = {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        let mut out = Vec::new();
        if let Some(cur) = &player.current {
            out.push((format!("▶ {}", cur.display()), cur.duration));
        }
        for t in &player.queue {
            out.push((t.display(), t.duration));
        }
        out
    };
    ctx.reply(&msg::queue_list_msg(&tracks)).await?;
    Ok(())
}

async fn vnow(ctx: &Ctx) -> Result<()> {
    let snap = {
        let mut reg = players().lock().await;
        reg.player(ctx.chat_id).snapshot()
    };
    let Some(cur) = snap.current else {
        ctx.reply(&msg::nothing_playing_msg()).await?;
        return Ok(());
    };
    ctx.reply(&msg::now_playing_msg(
        &cur.display(),
        cur.duration,
        snap.position_secs,
        &cur.user,
    ))
    .await?;
    Ok(())
}

async fn vclear(ctx: &Ctx) -> Result<()> {
    {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        if player.current.is_none() {
            ctx.reply(&msg::nothing_playing_msg()).await?;
            return Ok(());
        }
        player.stop();
    }
    ctx.reply(&msg::cleared_msg()).await?;
    // Leave/end the group call on the engine
    // (Python: vcmusic.py:vclear, `call.end`).
    vc(ctx)?.end(ctx.chat_id).await?;
    Ok(())
}

async fn vloop(ctx: &Ctx) -> Result<()> {
    let arg = ctx.args.trim().to_lowercase();
    let enabled = {
        let mut reg = players().lock().await;
        let player = reg.player(ctx.chat_id);
        if player.current.is_none() {
            ctx.reply(&msg::nothing_playing_msg()).await?;
            return Ok(());
        }
        if arg.is_empty()
            || matches!(
                arg.as_str(),
                "0" | "off" | "none" | "no" | "disable" | "stop"
            )
        {
            if let Some(cur) = &mut player.current {
                cur.loop_count = 0;
            }
            player.loop_enabled = false;
            false
        } else if let Ok(n) = arg.parse::<u32>() {
            if let Some(cur) = &mut player.current {
                cur.loop_count = n;
            }
            player.loop_enabled = true;
            true
        } else {
            ctx.reply(&format!("**Usage**\n\n`.vloop <n|off>`")).await?;
            return Ok(());
        }
    };
    ctx.reply(&msg::loop_msg(enabled)).await?;
    Ok(())
}

async fn vcmute(ctx: &Ctx) -> Result<()> {
    ctx.reply(&msg::mute_msg(true)).await?;
    // Mute the stream on the engine (Python: vcmusic.py:vcmute, `call.mute`).
    vc(ctx)?.mute(ctx.chat_id).await?;
    Ok(())
}

async fn vcunmute(ctx: &Ctx) -> Result<()> {
    ctx.reply(&msg::mute_msg(false)).await?;
    // Unmute the stream on the engine
    // (Python: vcmusic.py:vcunmute, `call.unmute`).
    vc(ctx)?.unmute(ctx.chat_id).await?;
    Ok(())
}

async fn startvc(ctx: &Ctx) -> Result<()> {
    // phone.CreateGroupCallRequest(chat) (Python: vcmusic.py:startvc).
    ctx.responder.create_group_call(ctx.chat_id).await?;
    ctx.reply("Voice chat created.").await?;
    Ok(())
}

async fn stopvc(ctx: &Ctx) -> Result<()> {
    {
        let mut reg = players().lock().await;
        reg.remove(ctx.chat_id);
    }
    // phone.DiscardGroupCallRequest(call) (Python: vcmusic.py:stopvc).
    ctx.responder.discard_group_call(ctx.chat_id).await?;
    ctx.reply("Voice chat ended.").await?;
    Ok(())
}

async fn vctitle(ctx: &Ctx) -> Result<()> {
    let title = ctx.args.trim();
    if title.is_empty() {
        ctx.reply(&ctx.get_str("vct_6")).await?;
        return Ok(());
    }
    // phone.EditGroupCallTitleRequest(call, title)
    // (Python: vcmusic.py:vctitle).
    ctx.responder
        .edit_group_call_title(ctx.chat_id, title)
        .await?;
    ctx.reply("Voice chat title updated.").await?;
    Ok(())
}

async fn vcinvite(ctx: &Ctx) -> Result<()> {
    let _id = ctx.reply(&ctx.get_str("vct_3")).await?;
    // Invite every non-bot participant (Python: vcmusic.py:vcinvite;
    // the Responder batches 6 per call).
    let (participants, _total) = ctx.responder.participants(ctx.chat_id, 200, 0).await?;
    let ids: Vec<i64> = participants
        .iter()
        .filter(|p| !p.user.bot)
        .map(|p| p.user.id)
        .collect();
    if ids.is_empty() {
        ctx.reply("No members to invite.").await?;
        return Ok(());
    }
    ctx.responder.invite_to_group_call(ctx.chat_id, ids).await?;
    ctx.reply("Invites sent.").await?;
    Ok(())
}
