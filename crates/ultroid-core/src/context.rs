//! Handler context + responder trait — the per-update object plugins see.
//! Port of the Telethon event + `ultroid_cmd` wrapper surface.

use crate::config::Config;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::ops::{
    AdminRights, BannedRights, BoostStatus, ChatSummary, DialogInfo, MessageSummary,
    ParticipantInfo, StickerSetInfo, ThemeInfo, UserSummary, WallpaperInfo,
};
use crate::rich::RichDoc;
use crate::router::Registry;
use crate::strings::Strings;
use crate::tg::InlineButton;
use crate::vc::VcHandle;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatType {
    Private,
    Group,
    Supergroup,
    Channel,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// The user's own account (ultroid_bot).
    User,
    /// The assistant bot (asst) — the ONLY client that may send rich.
    AsstBot,
    /// Extra user accounts (SESSION2/3) or the VC account.
    ExtraUser,
}

/// What a handler can do back to Telegram. Implemented in the binary crate
/// over grammers `Client`s; the user-client implementation MUST refuse
/// `send_rich` with `Error::RichOnlyBot`.
#[async_trait]
pub trait Responder: Send + Sync {
    async fn reply(&self, text: &str) -> Result<i64>;
    async fn reply_html(&self, html: &str) -> Result<i64>;
    async fn edit(&self, msg_id: i64, text: &str) -> Result<()>;
    /// Edit a message with Telegram-safe HTML (`<b> <i> <code> <a>`,
    /// same subset as `reply_html`).
    async fn edit_html(&self, msg_id: i64, html: &str) -> Result<()>;
    /// Edit a message's text (HTML) and inline buttons in one call
    /// (the inline help system edits text + keyboard together).
    async fn edit_markup(
        &self,
        msg_id: i64,
        html: &str,
        buttons: Vec<Vec<InlineButton>>,
    ) -> Result<()>;
    async fn delete(&self, msg_id: i64) -> Result<()>;
    async fn send_file(&self, path: &Path, caption: &str) -> Result<i64>;
    async fn send_rich(&self, doc: &RichDoc) -> Result<i64>;
    /// Chat id this responder targets.
    fn chat_id(&self) -> i64;

    // -- Extended Telegram operations (the "responder-op" surface) --------
    // Every method below is a real Telegram operation implemented in the
    // binary's `GrammersResponder` over grammers raw invokes / high-level
    // calls. Dialog ids are bot API dialog ids (`PeerId::bot_api_dialog_id`).
    // `chat_id`/`msg_id` pairs identify messages; `dest` paths are local
    // filesystem paths owned by the caller.

    /// Download the media attached to a message into `dest` (parent dir
    /// must exist). Returns the written path. Errors when the message has
    /// no downloadable media.
    async fn download_media(&self, chat_id: i64, msg_id: i32, dest: &Path) -> Result<PathBuf>;
    /// Fetch one message's sender / text / media presence.
    async fn message_summary(&self, chat_id: i64, msg_id: i32) -> Result<Option<MessageSummary>>;
    /// Resolve a user by username (with or without `@`) or numeric id.
    async fn resolve_user(&self, query: &str) -> Result<UserSummary>;
    /// Full user info (`users.GetFullUser`).
    async fn user_full(&self, dialog_id: i64) -> Result<UserSummary>;
    /// Chat / channel info (title, username, kind).
    async fn chat_info(&self, dialog_id: i64) -> Result<ChatSummary>;
    /// The logged-in account itself.
    async fn me(&self) -> Result<UserSummary>;
    /// All dialogs (chat list), in server order.
    async fn dialogs(&self) -> Result<Vec<DialogInfo>>;

    /// Send a plain-text message to an arbitrary peer. Returns the new id.
    async fn send_to(&self, dialog_id: i64, text: &str) -> Result<i64>;
    /// Send an HTML message to an arbitrary peer. Returns the new id.
    async fn send_html_to(&self, dialog_id: i64, html: &str) -> Result<i64>;
    /// Send a local file to an arbitrary peer. Returns the new id.
    async fn send_file_to(&self, dialog_id: i64, path: &Path, caption: &str) -> Result<i64>;
    /// Send a text message with inline buttons to an arbitrary peer.
    async fn send_markup_to(
        &self,
        dialog_id: i64,
        text: &str,
        buttons: Vec<Vec<InlineButton>>,
    ) -> Result<i64>;
    /// Send a poll (`InputMediaPoll`). Returns the new message id.
    async fn send_poll(
        &self,
        dialog_id: i64,
        question: &str,
        options: Vec<String>,
        multiple: bool,
        quiz: bool,
    ) -> Result<i64>;
    /// Send a plain-text message scheduled `delay_secs` in the future.
    async fn send_scheduled(&self, dialog_id: i64, text: &str, delay_secs: u64) -> Result<i64>;
    /// Send a plain-text message with a message effect (`effect_id` is the
    /// numeric effect id from `availableEffects`).
    async fn send_effect(&self, dialog_id: i64, text: &str, effect_id: i64) -> Result<i64>;
    /// Send a live location (`InputMediaGeoLive`).
    async fn send_geo_live(
        &self,
        dialog_id: i64,
        latitude: f64,
        longitude: f64,
        period_secs: i32,
    ) -> Result<i64>;
    /// Forward one message to another peer. Returns the new message id.
    async fn forward_to(&self, from_dialog_id: i64, msg_id: i32, to_dialog_id: i64) -> Result<i64>;
    /// Delete messages in an arbitrary peer.
    async fn delete_from(&self, dialog_id: i64, msg_ids: &[i32]) -> Result<()>;
    /// Pin a message (`messages.UpdatePinnedMessage`).
    async fn pin_message(&self, dialog_id: i64, msg_id: i32, silent: bool) -> Result<()>;
    /// Unpin one message.
    async fn unpin_message(&self, dialog_id: i64, msg_id: i32) -> Result<()>;
    /// Unpin all messages in a peer.
    async fn unpin_all(&self, dialog_id: i64) -> Result<()>;

    /// Ban a participant (`channels.EditBanned`).
    async fn ban_user(&self, chat_id: i64, user_id: i64, rights: BannedRights) -> Result<()>;
    /// Unban a participant (empty banned rights).
    async fn unban_user(&self, chat_id: i64, user_id: i64) -> Result<()>;
    /// Promote/demote a participant (`channels.EditAdmin`).
    async fn promote_user(
        &self,
        chat_id: i64,
        user_id: i64,
        rights: AdminRights,
        rank: &str,
    ) -> Result<()>;
    /// Set a chat's default banned rights.
    async fn edit_default_rights(&self, chat_id: i64, rights: BannedRights) -> Result<()>;
    /// One page of participants (`channels.GetParticipants`, recent filter);
    /// returns the page and the server-reported total count.
    async fn participants(
        &self,
        chat_id: i64,
        limit: i32,
        offset: i32,
    ) -> Result<(Vec<ParticipantInfo>, i32)>;

    /// Move a dialog in/out of the archive folder.
    async fn set_archived(&self, dialog_id: i64, archived: bool) -> Result<()>;

    /// Update own profile (`account.UpdateProfile`; `None` = leave as is).
    async fn update_profile(
        &self,
        first_name: Option<&str>,
        last_name: Option<&str>,
        about: Option<&str>,
    ) -> Result<()>;
    /// Upload a local image as the own profile photo.
    async fn upload_profile_photo(&self, path: &Path) -> Result<()>;
    /// Delete own profile photos, most recent first; returns how many
    /// were deleted. `limit <= 0` means all.
    async fn delete_profile_photos(&self, limit: i32) -> Result<i32>;
    /// Download a user's profile photos (most recent first) into `dir`
    /// as `profile-<n>.jpg`; returns the written paths.
    async fn download_profile_photos(
        &self,
        dialog_id: i64,
        dir: &Path,
        limit: i32,
    ) -> Result<Vec<PathBuf>>;

    /// Run an inline query against a bot and click the first result into
    /// `chat_id` (`messages.GetInlineBotResults` + `SendInlineBotResult`
    /// with `reply_to`, mirroring Telethon's `result.click(...)`).
    /// `bot_dialog_id` is the bot's bot API dialog id.
    async fn inline_query_click(
        &self,
        bot_dialog_id: i64,
        query: &str,
        chat_id: i64,
        reply_to: Option<i32>,
    ) -> Result<()>;

    /// Boost status of a channel (`premium.GetBoostsStatus`).
    async fn boost_status(&self, dialog_id: i64) -> Result<BoostStatus>;
    /// Give one boost slot to a channel (`premium.ApplyBoost`).
    async fn apply_boost(&self, dialog_id: i64) -> Result<()>;

    /// Delete own stories (`stories.DeleteStories`).
    async fn delete_stories(&self, story_ids: Vec<i32>) -> Result<()>;
    /// Toggle story stealth mode (`stories.ActivateStealthMode`).
    async fn set_story_stealth(&self, enabled: bool) -> Result<()>;
    /// Post the replied message's media as a story from the own account
    /// (`stories.SendStory`, public visibility).
    async fn post_story(&self, chat_id: i64, msg_id: i32, caption: &str) -> Result<()>;

    /// The own account's sticker sets (`messages.GetMyStickers`).
    async fn my_sticker_sets(&self) -> Result<Vec<StickerSetInfo>>;
    /// One sticker set by short name (`messages.GetStickerSet`).
    async fn sticker_set(&self, short_name: &str) -> Result<StickerSetInfo>;
    /// Add the document attached to a message to a sticker set
    /// (`stickers.AddStickerToSet`), with the given emoji.
    async fn add_document_to_sticker_set(
        &self,
        chat_id: i64,
        msg_id: i32,
        short_name: &str,
        emoji: &str,
    ) -> Result<()>;

    /// Installed + available wallpapers (`account.GetWallPapers`).
    async fn list_wallpapers(&self) -> Result<Vec<WallpaperInfo>>;
    /// Upload a local image as a (saved) wallpaper.
    async fn upload_wallpaper(&self, path: &Path) -> Result<()>;
    /// Available chat themes (`account.GetChatThemes`).
    async fn list_themes(&self) -> Result<Vec<ThemeInfo>>;
    /// Apply a chat theme by slug (`messages.SetChatTheme`).
    async fn set_chat_theme(&self, dialog_id: i64, theme_slug: &str) -> Result<()>;
    /// Start a group call in the chat (`phone.CreateGroupCall`).
    async fn create_group_call(&self, chat_id: i64) -> Result<()>;
    /// Discard (stop) the group call in the chat.
    async fn discard_group_call(&self, chat_id: i64) -> Result<()>;
    /// Set the group call's title.
    async fn edit_group_call_title(&self, chat_id: i64, title: &str) -> Result<()>;
    /// Invite users to the group call, in batches of 6. Best effort
    /// per user: users that fail to resolve are skipped; an error is
    /// returned only when nothing could be invited.
    async fn invite_to_group_call(&self, chat_id: i64, user_ids: Vec<i64>) -> Result<()>;
}

/// The context handed to every command handler.
pub struct Ctx {
    pub text: String,
    /// Text after the matched command (Python's `event.pattern_match.group(1)`
    /// equivalent for the common `cmd( (.*)|$)` shape; empty when none).
    pub args: String,
    /// All regex capture groups (group 0 is the whole match).
    pub captures: Vec<String>,
    /// Name of the matched command (`CommandDef.name`).
    pub command: &'static str,
    pub chat_id: i64,
    pub chat_type: ChatType,
    pub sender_id: i64,
    pub message_id: i64,
    pub reply_to_id: Option<i64>,
    pub reply_to_text: Option<String>,
    /// Bot API dialog id of the replied message's sender, when the event
    /// replies to a message (filled by the binary from the reply fetch).
    pub reply_to_sender_id: Option<i64>,
    pub is_owner: bool,
    pub is_sudo: bool,
    pub is_dev: bool,
    pub is_guest: bool,
    pub via_bot: bool,
    pub db: Arc<dyn Database>,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub responder: Arc<dyn Responder>,
    pub lang: String,
    pub strings: Arc<Strings>,
    pub client_kind: ClientKind,
    /// Shared command registry (powers `.help <plugin>` lookups).
    pub registry: Arc<Registry>,
    /// Bot API dialog id of the assistant bot, when connected
    /// (for inline-query ops: `.help`, `.gif`, `.icalc`).
    pub asst_dialog_id: Option<i64>,
    /// Username of the assistant bot (without `@`), when connected.
    /// Used for the settings URL on the help menu.
    pub asst_username: Option<String>,
    /// A responder bound to the assistant-bot client for the same peer,
    /// when the assistant is connected. Used by commands that must send
    /// as the assistant bot (bot2bot replies, pmpermit log notes).
    pub asst: Option<Arc<dyn Responder>>,
    /// Actor handle to the voice engine thread, when the engine was
    /// started (user client connected, VC enabled). `None` in bot-only
    /// mode or before the engine thread exists.
    pub vc: Option<VcHandle>,
}

impl Ctx {
    pub async fn reply(&self, text: &str) -> Result<i64> {
        self.responder.reply(text).await
    }

    pub async fn reply_html(&self, html: &str) -> Result<i64> {
        self.responder.reply_html(html).await
    }

    pub async fn edit(&self, msg_id: i64, text: &str) -> Result<()> {
        self.responder.edit(msg_id, text).await
    }

    pub async fn edit_html(&self, msg_id: i64, html: &str) -> Result<()> {
        self.responder.edit_html(msg_id, html).await
    }

    pub async fn delete(&self, msg_id: i64) -> Result<()> {
        self.responder.delete(msg_id).await
    }

    pub async fn send_file(&self, path: &Path, caption: &str) -> Result<i64> {
        self.responder.send_file(path, caption).await
    }

    /// Rich send — refuses on non-bot clients (`Error::RichOnlyBot`).
    pub async fn send_rich(&self, doc: &RichDoc) -> Result<i64> {
        self.responder.send_rich(doc).await
    }

    /// Rich send with explicit plain-text fallback on bot clients, and a
    /// plain-text send on user clients (the Python fallback behaviour).
    pub async fn send_rich_or_plain(&self, doc: &RichDoc) -> Result<i64> {
        match self.send_rich(doc).await {
            Ok(id) => Ok(id),
            Err(Error::RichOnlyBot) => self.reply(&doc.plain_text()).await,
            Err(e) => Err(e),
        }
    }

    pub fn get_str(&self, key: &str) -> String {
        self.strings.get(key)
    }

    /// First word of `args`, lowercased, for sub-command dispatch.
    pub fn subcommand(&self) -> &str {
        self.args.split_whitespace().next().unwrap_or("")
    }
}
