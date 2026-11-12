//! Core tests: config parsing, router matching/gates, rich gate data,
//! local JSON DB. These are unit tests only — they never touch Telegram.

use crate::config::Config;
use crate::context::{ChatType, ClientKind, Ctx, Responder};
use crate::db::local::{LocalJsonDb, MemoryDb};
use crate::db::Database;
use crate::error::Result;
use crate::ops::{
    AdminRights, BannedRights, BoostStatus, ChatSummary, DialogInfo, MessageSummary,
    ParticipantInfo, StickerSetInfo, ThemeInfo, UserSummary, WallpaperInfo,
};
use crate::rich::RichDoc;
use crate::tg::InlineButton;
use crate::router::{CommandDef, CommandHandler, DispatchOutcome, Registry};
use crate::strings::Strings;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct NullResponder;
#[async_trait]
impl Responder for NullResponder {
    async fn reply(&self, _t: &str) -> Result<i64> {
        Ok(1)
    }
    async fn reply_html(&self, _h: &str) -> Result<i64> {
        Ok(1)
    }
    async fn edit(&self, _m: i64, _t: &str) -> Result<()> {
        Ok(())
    }
    async fn edit_html(&self, _m: i64, _h: &str) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _m: i64) -> Result<()> {
        Ok(())
    }
    async fn send_file(&self, _p: &Path, _c: &str) -> Result<i64> {
        Ok(1)
    }
    async fn send_rich(&self, _d: &RichDoc) -> Result<i64> {
        // user-client responder: rich is refused
        Err(crate::Error::RichOnlyBot)
    }
    fn chat_id(&self) -> i64 {
        1
    }
    async fn download_media(&self, _chat_id: i64, _msg_id: i32, _dest: &Path) -> Result<PathBuf> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn message_summary(&self, _chat_id: i64, _msg_id: i32) -> Result<Option<MessageSummary>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn resolve_user(&self, _query: &str) -> Result<UserSummary> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn user_full(&self, _dialog_id: i64) -> Result<UserSummary> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn chat_info(&self, _dialog_id: i64) -> Result<ChatSummary> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn me(&self) -> Result<UserSummary> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn dialogs(&self) -> Result<Vec<DialogInfo>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_to(&self, _dialog_id: i64, _text: &str) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_html_to(&self, _dialog_id: i64, _html: &str) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_file_to(&self, _dialog_id: i64, _path: &Path, _caption: &str) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_markup_to(&self, _dialog_id: i64, _text: &str, _buttons: Vec<Vec<InlineButton>>) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_poll(&self, _dialog_id: i64, _question: &str, _options: Vec<String>, _multiple: bool, _quiz: bool) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_scheduled(&self, _dialog_id: i64, _text: &str, _delay_secs: u64) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_effect(&self, _dialog_id: i64, _text: &str, _effect_id: i64) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn send_geo_live(&self, _dialog_id: i64, _latitude: f64, _longitude: f64, _period_secs: i32) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn forward_to(&self, _from_dialog_id: i64, _msg_id: i32, _to_dialog_id: i64) -> Result<i64> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn delete_from(&self, _dialog_id: i64, _msg_ids: &[i32]) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn pin_message(&self, _dialog_id: i64, _msg_id: i32, _silent: bool) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn unpin_message(&self, _dialog_id: i64, _msg_id: i32) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn unpin_all(&self, _dialog_id: i64) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn ban_user(&self, _chat_id: i64, _user_id: i64, _rights: BannedRights) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn unban_user(&self, _chat_id: i64, _user_id: i64) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn promote_user(&self, _chat_id: i64, _user_id: i64, _rights: AdminRights, _rank: &str) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn edit_default_rights(&self, _chat_id: i64, _rights: BannedRights) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn participants(&self, _chat_id: i64, _limit: i32, _offset: i32) -> Result<(Vec<ParticipantInfo>, i32)> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn set_archived(&self, _dialog_id: i64, _archived: bool) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn update_profile(&self, _first_name: Option<&str>, _last_name: Option<&str>, _about: Option<&str>) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn upload_profile_photo(&self, _path: &Path) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn delete_profile_photos(&self, _limit: i32) -> Result<i32> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn download_profile_photos(&self, _dialog_id: i64, _dir: &Path, _limit: i32) -> Result<Vec<PathBuf>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn inline_query_click(&self, _bot_dialog_id: i64, _query: &str, _chat_id: i64, _reply_to: Option<i32>) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn boost_status(&self, _dialog_id: i64) -> Result<BoostStatus> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn apply_boost(&self, _dialog_id: i64) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn delete_stories(&self, _story_ids: Vec<i32>) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn set_story_stealth(&self, _enabled: bool) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn post_story(&self, _chat_id: i64, _msg_id: i32, _caption: &str) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn my_sticker_sets(&self) -> Result<Vec<StickerSetInfo>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn sticker_set(&self, _short_name: &str) -> Result<StickerSetInfo> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn add_document_to_sticker_set(&self, _chat_id: i64, _msg_id: i32, _short_name: &str, _emoji: &str) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn list_wallpapers(&self) -> Result<Vec<WallpaperInfo>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn upload_wallpaper(&self, _path: &Path) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn list_themes(&self) -> Result<Vec<ThemeInfo>> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
    async fn set_chat_theme(&self, _dialog_id: i64, _theme_slug: &str) -> Result<()> {
        Err(crate::Error::Other("NullResponder: no Telegram client".into()))
    }
}

struct Ping;
#[async_trait]
impl CommandHandler for Ping {
    async fn handle(&self, ctx: &Ctx) -> Result<()> {
        ctx.reply("pong").await?;
        Ok(())
    }
}

fn test_ctx(text: &str, owner: bool) -> Ctx {
    let cfg = Arc::new(Config {
        api_id: 123,
        api_hash: "hash".into(),
        session: None,
        session2: None,
        session3: None,
        bot_token: None,
        bot_token2: None,
        bot_token3: None,
        log_channel: 0,
        redis_uri: None,
        redis_password: None,
        database_url: None,
        mongo_uri: None,
        heroku_app_name: None,
        heroku_api: None,
        vc_session: None,
        addons: false,
        vcbot: false,
        app_version: "test".into(),
        device_model: "Ultroid".into(),
        sudo_special_ids: vec![],
        devlist_ids: vec![],
        apimix_api_key: None,
        apimix_base_url: "https://api.apmix.ai/v1".into(),
        apimix_model: None,
        bot_mode: false,
        setup_token: None,
        setup_bind: "0.0.0.0".into(),
        port: 7860,
        space_url: None,
        hf_token: None,
        keep_alive: false,
        openai_api_key: None,
        anthropic_api_key: None,
        gemini_api_key: None,
        groq_api_key: None,
        xai_api_key: None,
        meta_llama_api_key: None,
        compat_base_url: None,
        compat_api_key: None,
        compat_model: None,
        owner_id: 1,
        lang: "en".into(),
        hndlr: ".".into(),
        dual_hndlr: "/".into(),
        sudo_hndlr: ".".into(),
        data_dir: ".".into(),
    });
    Ctx {
        text: text.to_string(),
        args: String::new(),
        captures: Vec::new(),
        command: "",
        chat_id: 1,
        chat_type: ChatType::Private,
        sender_id: 1,
        message_id: 1,
        reply_to_id: None,
        reply_to_text: None,
        reply_to_sender_id: None,
        is_owner: owner,
        is_sudo: false,
        is_dev: false,
        is_guest: false,
        via_bot: false,
        db: Arc::new(MemoryDb::new()),
        config: cfg,
        http: reqwest::Client::new(),
        responder: Arc::new(NullResponder),
        lang: "en".into(),
        strings: Strings::load("en"),
        client_kind: ClientKind::User,
        registry: Arc::new(Registry::new()),
        asst_dialog_id: None,
        asst: None,
    }
}

#[tokio::test]
async fn router_matches_dot_and_slash_prefixes() {
    let mut reg = Registry::new();
    reg.add(
        CommandDef {
            name: "ping",
            pattern: "ping$",
            help: "ping",
            ..Default::default()
        },
        Arc::new(Ping),
    );
    for text in [".ping", "/ping"] {
        let mut ctx = test_ctx(text, true);
        assert!(
            matches!(reg.dispatch(&mut ctx).await.unwrap(), DispatchOutcome::Matched),
            "{text} should match"
        );
    }
    let mut ctx = test_ctx(".pong", true);
    assert!(matches!(
        reg.dispatch(&mut ctx).await.unwrap(),
        DispatchOutcome::NoMatch
    ));
}

#[tokio::test]
async fn router_captures_args_like_python() {
    let mut reg = Registry::new();
    reg.add(
        CommandDef {
            name: "alive",
            pattern: "alive( (.*)|$)",
            help: "alive",
            ..Default::default()
        },
        Arc::new(Ping),
    );
    let mut ctx = test_ctx(".alive hello", true);
    assert!(matches!(reg.dispatch(&mut ctx).await.unwrap(), DispatchOutcome::Matched));
    assert_eq!(ctx.args, "hello");
    assert_eq!(ctx.command, "alive");
}

#[tokio::test]
async fn router_owner_gate_denies_non_owner_silently() {
    let mut reg = Registry::new();
    reg.add(
        CommandDef {
            name: "eval",
            pattern: "eval",
            help: "eval",
            owner_only: true,
            ..Default::default()
        },
        Arc::new(Ping),
    );
    let mut ctx = test_ctx(".eval 1+1", false);
    assert!(matches!(
        reg.dispatch(&mut ctx).await.unwrap(),
        DispatchOutcome::Denied(_)
    ));
}

#[tokio::test]
async fn rich_plain_text_fallback_strips_markers() {
    let doc = RichDoc::markdown("# Title\n\n**bold** and _italic_ `code`").unwrap();
    assert_eq!(doc.plain_text(), "Title\n\nbold and italic code");
    assert!(RichDoc::markdown("x".repeat(40_000).as_str()).is_err());
}

#[tokio::test]
async fn rich_only_bot_refusal() {
    let ctx = test_ctx(".x", true);
    let doc = RichDoc::markdown("hi").unwrap();
    assert!(matches!(
        ctx.send_rich(&doc).await,
        Err(crate::Error::RichOnlyBot)
    ));
    // ...but the plain fallback path works on a user client
    assert_eq!(ctx.send_rich_or_plain(&doc).await.unwrap(), 1);
}

#[tokio::test]
async fn local_db_roundtrip() {
    let dir = std::env::temp_dir().join(format!("ultroid-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = LocalJsonDb::open(dir.join("test.localdb.json")).unwrap();
    assert_eq!(db.get("k").await.unwrap(), None);
    db.set("k", "v").await.unwrap();
    assert_eq!(db.get("k").await.unwrap().as_deref(), Some("v"));
    // reopen: persisted
    let db2 = LocalJsonDb::open(dir.join("test.localdb.json")).unwrap();
    assert_eq!(db2.get("k").await.unwrap().as_deref(), Some("v"));
    db2.del("k").await.unwrap();
    assert_eq!(db2.get("k").await.unwrap(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn config_parses_int_lists() {
    // `set_var` is unsafe in edition 2024; single-threaded test, safe here.
    unsafe {
        std::env::set_var("SUDO_SPECIAL_IDS", "1, 2,3");
        std::env::set_var("API_ID", "123");
        std::env::set_var("API_HASH", "hash");
    }
    let cfg = Config::from_env().unwrap();
    assert_eq!(cfg.sudo_special_ids, vec![1, 2, 3]);
}
