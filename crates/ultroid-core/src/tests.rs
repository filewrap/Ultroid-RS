//! Core tests: config parsing, router matching/gates, rich gate data,
//! local JSON DB. These are unit tests only — they never touch Telegram.

use crate::config::Config;
use crate::context::{ChatType, ClientKind, Ctx, Responder};
use crate::db::local::{LocalJsonDb, MemoryDb};
use crate::db::Database;
use crate::error::Result;
use crate::rich::RichDoc;
use crate::router::{CommandDef, CommandHandler, DispatchOutcome, Registry};
use crate::strings::Strings;
use async_trait::async_trait;
use std::path::Path;
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
