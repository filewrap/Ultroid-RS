//! Plain data types for the extended `Responder` surface — the values
//! plugins need from Telegram operations that go beyond reply/edit.
//!
//! These types are deliberately free of grammers/TL types: `ultroid-core`
//! has no grammers dependency, so the binary crate maps grammers values
//! onto these and plugins never see a TL constructor. Field layouts mirror
//! the layer-233 TL structs they come from (`ChatBannedRights`,
//! `ChatAdminRights`, `User`, `ChannelParticipant` …); see
//! `crates/ultroid-rs/src/responder_ops.rs` for the mapping.

// ---------------------------------------------------------------------------
// Users / chats
// ---------------------------------------------------------------------------

/// A Telegram user, as far as plugins need to know them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSummary {
    /// Bot API dialog id (negative for groups/channels, positive for users).
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    /// Telegram Premium flag (`User.premium`).
    pub premium: bool,
    pub bot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    User,
    Group,
    Supergroup,
    Channel,
}

/// A chat / channel, as far as plugins need to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    /// Bot API dialog id.
    pub id: i64,
    pub title: String,
    pub username: Option<String>,
    pub kind: ChatKind,
}

/// One participant of a group/channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantInfo {
    pub user: UserSummary,
    /// Holds an admin right (creator counts as admin too).
    pub admin: bool,
    /// Is the chat creator/owner.
    pub owner: bool,
}

/// One dialog (chat list entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogInfo {
    /// Bot API dialog id.
    pub id: i64,
    pub title: String,
    pub unread_count: i32,
    pub pinned: bool,
    /// Archived folder (folder_id == 1).
    pub archived: bool,
}

/// A single fetched message — sender + text + media presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSummary {
    pub id: i32,
    /// Bot API dialog id of the sender, if any.
    pub sender_id: Option<i64>,
    pub text: String,
    pub has_media: bool,
}

// ---------------------------------------------------------------------------
// Rights (mirrors TL ChatBannedRights / ChatAdminRights, layer 233)
// ---------------------------------------------------------------------------

/// Mirrors `types::ChatBannedRights`. `until_date` is a Unix timestamp;
/// `None` means "forever". All flags default to `false` (nothing banned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BannedRights {
    pub until_date: Option<i32>,
    pub view_messages: bool,
    pub send_messages: bool,
    pub send_media: bool,
    pub send_stickers: bool,
    pub send_gifs: bool,
    pub send_games: bool,
    pub send_inline: bool,
    pub embed_links: bool,
    pub send_polls: bool,
    pub change_info: bool,
    pub invite_users: bool,
    pub pin_messages: bool,
    pub manage_topics: bool,
    pub send_photos: bool,
    pub send_videos: bool,
    pub send_roundvideos: bool,
    pub send_audios: bool,
    pub send_voices: bool,
    pub send_docs: bool,
    pub send_plain: bool,
    pub edit_rank: bool,
    pub send_reactions: bool,
    pub manage_linked_peers: bool,
}

impl BannedRights {
    /// Every restriction on, forever — the "ban everything" set
    /// Python's `EditBannedRequest` uses for a full ban.
    pub fn all_banned() -> Self {
        Self {
            until_date: None,
            view_messages: true,
            send_messages: true,
            send_media: true,
            send_stickers: true,
            send_gifs: true,
            send_games: true,
            send_inline: true,
            embed_links: true,
            send_polls: true,
            change_info: true,
            invite_users: true,
            pin_messages: true,
            manage_topics: true,
            send_photos: true,
            send_videos: true,
            send_roundvideos: true,
            send_audios: true,
            send_voices: true,
            send_docs: true,
            send_plain: true,
            edit_rank: true,
            send_reactions: true,
            manage_linked_peers: true,
        }
    }
}

/// Mirrors `types::ChatAdminRights`. All flags default to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdminRights {
    pub change_info: bool,
    pub post_messages: bool,
    pub edit_messages: bool,
    pub delete_messages: bool,
    pub ban_users: bool,
    pub invite_users: bool,
    pub pin_messages: bool,
    pub add_admins: bool,
    pub anonymous: bool,
    pub manage_call: bool,
    pub other: bool,
    pub manage_topics: bool,
    pub post_stories: bool,
    pub edit_stories: bool,
    pub delete_stories: bool,
    pub manage_direct_messages: bool,
    pub manage_ranks: bool,
    pub manage_linked_peers: bool,
    pub manage_welcome_messages: bool,
}

impl AdminRights {
    /// Every admin right on — Python's "full admin" promotion set.
    pub fn all() -> Self {
        Self {
            change_info: true,
            post_messages: true,
            edit_messages: true,
            delete_messages: true,
            ban_users: true,
            invite_users: true,
            pin_messages: true,
            add_admins: true,
            anonymous: true,
            manage_call: true,
            other: true,
            manage_topics: true,
            post_stories: true,
            edit_stories: true,
            delete_stories: true,
            manage_direct_messages: true,
            manage_ranks: true,
            manage_linked_peers: true,
            manage_welcome_messages: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Boosts / stickers / wallpapers / themes
// ---------------------------------------------------------------------------

/// Boost status of a channel, from `premium.GetBoostsStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoostStatus {
    pub level: i32,
    pub boosts: i32,
    /// Boosts our own account gave this peer, if any.
    pub gift_boosts: Option<i32>,
    /// Boosts needed for the next level, if any.
    pub next_level_boosts: Option<i32>,
}

/// One sticker inside a sticker set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerItem {
    pub emoji: String,
    pub document_id: i64,
}

/// A sticker set, from `messages.GetStickerSet` / `GetMyStickers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetInfo {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    pub short_name: String,
    pub count: i32,
    /// Whether this is an emoji (custom-emoji) set rather than stickers.
    pub emojis: bool,
    /// First stickers of the set (may be truncated by the server).
    pub stickers: Vec<StickerItem>,
}

/// A wallpaper, from `account.GetWallPapers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperInfo {
    pub id: i64,
    pub title: Option<String>,
    pub dark: bool,
    pub pattern: bool,
}

/// A chat theme, from `account.GetChatThemes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeInfo {
    pub id: i64,
    pub access_hash: i64,
    pub slug: String,
    pub title: String,
    /// The theme's emoticon, when the server reports one — this is what
    /// layer 233's `messages.SetChatTheme` takes (`InputChatTheme`).
    pub emoticon: Option<String>,
}
