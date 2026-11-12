# Responder-op wiring reference (for wiring workers)

The binary now implements the full `Responder` op surface (commit
`bcd9733`). Your job: replace `TODO(responder-op)` error sites in your
assigned plugin files with real calls. Read this whole file first.

## Contract

- Work ONLY in `crates/ultroid-plugins/src/<your files>`. Do not touch
  `lib.rs`, `Cargo.toml`, any other crate, or `~/workspace/ultroid-rs-work/source/`.
- For each `TODO(responder-op)` site in your files: if the op it names
  is on the trait below, wire it for real. If it is NOT on the trait
  (payments, VC engine, quick replies, ephemeral/BusinessConnection,
  forum topics, factcheck RPCs, block/unblock, create/delete chats,
  chat actions, reactions, effects list, `UpdateEmojiStatus`, ...),
  **leave the TODO exactly as it is** and list the file:line in your
  report under "left as TODO (no trait op)".
- Never invent a client call, never fake a result, never change a
  command's behaviour/pattern/help text. Keep the surrounding logic;
  only the error return becomes the real call (plus any small local
  shaping the call needs, e.g. building a `BannedRights` from parsed
  flags).
- Verify each file with `rustfmt --edition 2024 --check <file>`.
  Do NOT run `cargo check` (lib.rs is coordinator-owned); the
  coordinator checks after wiring.
- Report per file: sites wired (count + what), sites left (file:line +
  which missing op), any real bug you found in the surrounding code.

## What is already on `Ctx` (no call needed)

- `ctx.reply_to_sender_id: Option<i64>` — sender of the replied
  message. Use it directly wherever a site says "resolve replied
  message's sender". (Value is `None` when the command does not reply
  to anything — handle that with the same error/usage reply the
  handler already gives for a missing reply.)
- `ctx.asst_dialog_id: Option<i64>` — the assistant bot's dialog id
  (for `inline_query_click`'s `bot_dialog_id`).
- `ctx.asst: Option<Arc<dyn Responder>>` — a responder bound to the
  assistant-bot client for the same peer. For "send as the assistant
  bot" sites: `if let Some(asst) = &ctx.asst { asst.send_to(id, text).await?; }`
  (when `None`, return an honest error saying the assistant is not
  connected — do NOT fall back to sending as the user).
- `ctx.reply_to_id: Option<i64>` and `ctx.chat_id: i64` — the ids the
  ops below take. NOTE: `reply_to_id` is `i64`; the ops take `msg_id: i32`
  — cast with `as i32`.

## The trait (`ultroid_core::Responder`; all methods `async`, all return
## `Result<...>` with `Error::Telegram` on server failure)

All dialog/chat ids are bot API dialog ids (`i64`). All `msg_id`s are `i32`.

```rust
// media / messages
async fn download_media(&self, chat_id: i64, msg_id: i32, dest: &Path) -> Result<PathBuf>;
//   Download the media attached to a message into `dest` (parent dir must exist).
//   Errors when the message has no downloadable media. Returns the written path.
async fn message_summary(&self, chat_id: i64, msg_id: i32) -> Result<Option<MessageSummary>>;
//   { id: i32, sender_id: Option<i64>, text: String, has_media: bool }
//   None when the message was deleted.
async fn resolve_user(&self, query: &str) -> Result<UserSummary>;
//   Username (with/without @) or numeric id. Errors if not a user / not found.
async fn user_full(&self, dialog_id: i64) -> Result<UserSummary>;
async fn chat_info(&self, dialog_id: i64) -> Result<ChatSummary>;
//   { id, title, username: Option<String>, kind: User|Group|Supergroup|Channel }
async fn me(&self) -> Result<UserSummary>;
//   { id, first_name: String, last_name: Option<String>, username: Option<String>,
//     premium: bool, bot: bool }
async fn dialogs(&self) -> Result<Vec<DialogInfo>>;
//   { id, title, unread_count: i32, pinned: bool, archived: bool }

// sends (return the new message id)
async fn send_to(&self, dialog_id: i64, text: &str) -> Result<i64>;
async fn send_html_to(&self, dialog_id: i64, html: &str) -> Result<i64>;
async fn send_file_to(&self, dialog_id: i64, path: &Path, caption: &str) -> Result<i64>;
async fn send_markup_to(&self, dialog_id: i64, text: &str, buttons: Vec<Vec<InlineButton>>) -> Result<i64>;
//   InlineButton from ultroid_core::tg: InlineButton::data(text, bytes) /
//   ::url(text, url) — build rows with Vec<Vec<InlineButton>>.
async fn send_poll(&self, dialog_id: i64, question: &str, options: Vec<String>, multiple: bool, quiz: bool) -> Result<i64>;
async fn send_scheduled(&self, dialog_id: i64, text: &str, delay_secs: u64) -> Result<i64>;
async fn send_effect(&self, dialog_id: i64, text: &str, effect_id: i64) -> Result<i64>;
async fn send_geo_live(&self, dialog_id: i64, latitude: f64, longitude: f64, period_secs: i32) -> Result<i64>;
async fn forward_to(&self, from_dialog_id: i64, msg_id: i32, to_dialog_id: i64) -> Result<i64>;
async fn delete_from(&self, dialog_id: i64, msg_ids: &[i32]) -> Result<()>;
async fn pin_message(&self, dialog_id: i64, msg_id: i32, silent: bool) -> Result<()>;
async fn unpin_message(&self, dialog_id: i64, msg_id: i32) -> Result<()>;
async fn unpin_all(&self, dialog_id: i64) -> Result<()>;

// admin
async fn ban_user(&self, chat_id: i64, user_id: i64, rights: BannedRights) -> Result<()>;
async fn unban_user(&self, chat_id: i64, user_id: i64) -> Result<()>;
async fn promote_user(&self, chat_id: i64, user_id: i64, rights: AdminRights, rank: &str) -> Result<()>;
async fn edit_default_rights(&self, chat_id: i64, rights: BannedRights) -> Result<()>;
//   BannedRights/AdminRights are plain structs in ultroid_core::ops with
//   one bool per TL right, all Default(false). BannedRights::all_banned()
//   bans everything (until_date None = forever); set `until_date: Some(ts)`
//   for a temp ban/mute. For a MUTE set the send_* flags true and leave
//   view_messages false. AdminRights::all() is the full-admin set.
async fn participants(&self, chat_id: i64, limit: i32, offset: i32) -> Result<(Vec<ParticipantInfo>, i32)>;
//   (page, total). ParticipantInfo { user: UserSummary, admin: bool, owner: bool }.
   // Page through with offset += page.len() while offset < total.

// folders / profile
async fn set_archived(&self, dialog_id: i64, archived: bool) -> Result<()>;
async fn update_profile(&self, first_name: Option<&str>, last_name: Option<&str>, about: Option<&str>) -> Result<()>;
async fn upload_profile_photo(&self, path: &Path) -> Result<()>;
async fn delete_profile_photos(&self, limit: i32) -> Result<i32>;
//   limit <= 0 means all. Returns how many were deleted.
async fn download_profile_photos(&self, dialog_id: i64, dir: &Path, limit: i32) -> Result<Vec<PathBuf>>;
//   Downloads into dir as profile-<n>.jpg, most recent first.

// inline
async fn inline_query_click(&self, bot_dialog_id: i64, query: &str, chat_id: i64, reply_to: Option<i32>) -> Result<()>;
//   Runs the inline query against the bot and clicks the FIRST result into
//   chat_id (replying to reply_to when given). Use ctx.asst_dialog_id for
//   the assistant bot; resolve other bots (e.g. "gif") via resolve_user first.

// boosts
async fn boost_status(&self, dialog_id: i64) -> Result<BoostStatus>;
//   { level: i32, boosts: i32, gift_boosts: Option<i32>, next_level_boosts: Option<i32> }
async fn apply_boost(&self, dialog_id: i64) -> Result<()>;

// stories
async fn delete_stories(&self, story_ids: Vec<i32>) -> Result<()>;
async fn set_story_stealth(&self, enabled: bool) -> Result<()>;
async fn post_story(&self, chat_id: i64, msg_id: i32, caption: &str) -> Result<()>;
//   Posts the replied message's photo/document as a story (public).

// stickers
async fn my_sticker_sets(&self) -> Result<Vec<StickerSetInfo>>;
async fn sticker_set(&self, short_name: &str) -> Result<StickerSetInfo>;
//   StickerSetInfo { id, access_hash, title, short_name, count, emojis: bool,
//                    stickers: Vec<StickerItem { emoji, document_id }> }
async fn add_document_to_sticker_set(&self, chat_id: i64, msg_id: i32, short_name: &str, emoji: &str) -> Result<()>;

// wallpapers / themes
async fn list_wallpapers(&self) -> Result<Vec<WallpaperInfo>>;
//   { id, title: Option<String> (the slug), dark: bool, pattern: bool }
async fn upload_wallpaper(&self, path: &Path) -> Result<()>;
async fn list_themes(&self) -> Result<Vec<ThemeInfo>>;
//   { id, access_hash, slug, title, emoticon: Option<String> }
async fn set_chat_theme(&self, dialog_id: i64, theme_slug: &str) -> Result<()>;
//   Layer 233 applies a theme by its emoticon; errors honestly if the
//   chosen theme has none.

// group calls (raw `phone.*` invokes; the audio path itself lives on
// the `VcControl` actor handle on `Ctx.vc` — see core::vc)
async fn create_group_call(&self, chat_id: i64) -> Result<()>;
async fn discard_group_call(&self, chat_id: i64) -> Result<()>;
async fn edit_group_call_title(&self, chat_id: i64, title: &str) -> Result<()>;
async fn invite_to_group_call(&self, chat_id: i64, user_ids: Vec<i64>) -> Result<()>;
   // Invites in batches of 6; users that fail to resolve are skipped,
   // error only when nothing could be invited.
```

## Calling convention in handlers

```rust
// Before (TODO site):
// TODO(responder-op): download replied media to a local file.
return Err(Error::Other("download replied media requires a binary Responder op ...".into()));

// After:
let reply_id = ctx.reply_to_id.ok_or_else(|| Error::Other("reply to a message first".into()))?;
let path = ctx.responder.download_media(ctx.chat_id, reply_id as i32, &dest).await?;
```

- The responder is `ctx.responder: Arc<dyn Responder>` — call methods
  directly on it (`ctx.responder.download_media(...)`); `Ctx` has no
  convenience wrappers for these (only reply/edit/delete/send_file/
  send_rich/send_rich_or_plain are wrapped).
- `ctx.asst` is `Option<Arc<dyn Responder>>` — same method calls on the
  inner value when present.
- Keep the existing `responder_op(...)` helper for sites you LEAVE as
  TODO (it builds the honest error); remove it ONLY at sites you wire.
  If the helper becomes unused in a file after your wiring, delete it
  (or leave it if other sites still use it — check with grep).
- Where Python iterated (participants, dialogs, sticker sets) — page or
  loop as the method's docs say; do not fetch only the first page and
  present it as the whole list unless the command is explicitly a
  preview/first-N command.

## The VC actor handle (`Ctx.vc: Option<VcHandle>`)

For `vcmusic.rs` only (not part of the 5-worker wave). `VcControl`
(`ultroid_core::vc`) is an async trait — call it exactly like the
responder methods, on `ctx.vc` when `Some`:

```rust
// `VcTrack` mirrors `ultroid_vc::dataclass::CachedTrack` field for
// field; build it from the plugin's track with `VcTrack::new(url,
// name, artist, user, file_path, thumbnail, track_id, duration,
// is_video, platform, channel)` where `channel` is
// `Some((chat_id, is_channel))` or `None`.
let Some(vc) = &ctx.vc else {
    return Err(Error::Other("voice engine is not running".into()));
};
let started = vc.enqueue(ctx.chat_id, track).await?;
vc.play_next(ctx.chat_id).await?;
let paused = vc.pause(ctx.chat_id).await?;   // bool
let resumed = vc.resume(ctx.chat_id).await?; // bool
let muted = vc.mute(ctx.chat_id).await?;     // bool
let unmuted = vc.unmute(ctx.chat_id).await?; // bool
vc.end(ctx.chat_id).await?;
```

The engine's own queue mirrors the plugin's local queue: `enqueue`
starts playback when idle (`Engine::enqueue` → `play_next` →
`song_download` → `play_media`), `play_next` advances the ENGINE
queue. Keep the plugin's local `PlayerRegistry` mutations exactly
as they are (display state) and put the actor call where the
`vc_op(...)` error is today. The four group-call plumbing commands
use the `Responder` methods above, not the actor.

## Known NOT-covered ops (leave the TODO, do not work around)

171 sites remain (after the wiring wave wired ~200). The VC engine
and group-call plumbing are now COVERED (`Ctx.vc` actor +
`create/discard_group_call`, `edit_group_call_title`,
`invite_to_group_call`; `vcmusic.rs` fully wired), as is the
replied message's sender (`ctx.reply_to_sender_id`).
- Payments/Stars: `payments.*` (gifts, giveaways, stars wallet/top-up/
  revenue/refund, gift codes, premium gift options).
- Quick replies (`messages.GetQuickReplies*`).
- Ephemeral / business-connection RPCs (`ephemeral.*`, `InvokeWithBusinessConnection`).
- Forum topics (`GetForumTopics`/`CreateForumTopic`/`EditForumTopic`).
- FactCheck RPCs (`messages.GetFactCheck`/`EditFactCheck`/`DeleteFactCheck`).
- Block/unblock/report-spam (`contacts.Block/Unblock`, `GetBlocked`,
  `messages.ReportSpam`).
- Create/delete chats, add/invite users, export invite, edit group photo,
  `GetAdminedPublicChannels`, `GetAllStickers`, chat actions
  (`SetTyping`), reactions (`SendReaction`), effects list
  (`GetAvailableEffects`), `UpdateEmojiStatus`, `GetMyStickers` is
  covered (`my_sticker_sets`) but per-pack sticker upload via
  `@Stickers` conversation is not.
- Anything whose Python source drives a *conversation with another bot*
  (`@Stickers`, `@SangMata_beta_bot`, `@gif` inline is covered via
  `inline_query_click` after `resolve_user("gif")` — that one IS wired).
