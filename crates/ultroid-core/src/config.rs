//! Configuration — port of `pyUltroid/configs.py` (`class Var`, python-decouple).
//!
//! Same env vars, same defaults. dotenv loading via dotenvy. CLI argv 1-5
//! override API_ID / API_HASH / SESSION / REDIS_URI / REDIS_PASSWORD, as in Python.

use crate::error::{Error, Result};

pub const VERSION: &str = "14.10.2026";
pub const ULTROID_VERSION: &str = "14.10.2026[fork]";

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env(key) {
        Some(v) => matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

fn env_int(key: &str) -> Option<i64> {
    env(key).and_then(|v| v.parse::<i64>().ok())
}

fn env_int_list(key: &str) -> Vec<i64> {
    env(key)
        .map(|v| {
            v.split(',')
                .filter_map(|s| s.trim().parse::<i64>().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_id: i32,
    pub api_hash: String,
    /// NOTE: in Python this is a Telethon StringSession string. That format
    /// is NOT portable to grammers. In Rust this value is accepted only as a
    /// *path* to a grammers SqliteSession file when it looks like a path, and
    /// otherwise ignored for auth (see README "Sessions"). The canonical
    /// session location is `sessions/<name>.db`.
    pub session: Option<String>,
    pub session2: Option<String>,
    pub session3: Option<String>,
    pub bot_token: Option<String>,
    pub bot_token2: Option<String>,
    pub bot_token3: Option<String>,
    pub log_channel: i64,
    pub redis_uri: Option<String>,
    pub redis_password: Option<String>,
    pub database_url: Option<String>,
    pub mongo_uri: Option<String>,
    pub heroku_app_name: Option<String>,
    pub heroku_api: Option<String>,
    pub vc_session: Option<String>,
    pub addons: bool,
    pub vcbot: bool,
    pub app_version: String,
    pub device_model: String,
    pub sudo_special_ids: Vec<i64>,
    pub devlist_ids: Vec<i64>,
    pub apimix_api_key: Option<String>,
    pub apimix_base_url: String,
    pub apimix_model: Option<String>,
    pub bot_mode: bool,
    pub setup_token: Option<String>,
    pub setup_bind: String,
    pub port: u16,
    pub space_url: Option<String>,
    pub hf_token: Option<String>,
    pub keep_alive: bool,
    // Agent provider keys (read from env directly by agent code in Python).
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub gemini_api_key: Option<String>,
    pub groq_api_key: Option<String>,
    pub xai_api_key: Option<String>,
    pub meta_llama_api_key: Option<String>,
    pub compat_base_url: Option<String>,
    pub compat_api_key: Option<String>,
    pub compat_model: Option<String>,
    pub owner_id: i64,
    pub lang: String,
    pub hndlr: String,
    pub dual_hndlr: String,
    pub sudo_hndlr: String,
    pub data_dir: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();
        let argv: Vec<String> = std::env::args().collect();
        let arg = |i: usize| argv.get(i).cloned().filter(|v| !v.is_empty());

        let api_id = arg(1)
            .and_then(|v| v.parse::<i32>().ok())
            .or_else(|| env_int("API_ID").map(|v| v as i32));
        let api_id = match api_id {
            Some(v) => v,
            None => return Err(Error::Config("API_ID is required (env or argv[1])".into())),
        };
        let api_hash = arg(2).or_else(|| env("API_HASH")).unwrap_or_default();
        if api_hash.is_empty() {
            return Err(Error::Config("API_HASH is required (env or argv[2])".into()));
        }
        let redis_uri = arg(4)
            .or_else(|| env("REDIS_URI").or_else(|| env("REDIS_URL")))
            .or_else(|| {
                // Railway-style parts
                match (env("REDISHOST"), env("REDISPORT")) {
                    (Some(h), Some(p)) => Some(format!(
                        "redis://{}:{}@{}:{}",
                        env("REDISUSER").unwrap_or_default(),
                        env("REDISPASSWORD").unwrap_or_default(),
                        h,
                        p
                    )),
                    _ => None,
                }
            });

        Ok(Config {
            api_id,
            api_hash,
            session: arg(3).or_else(|| env("SESSION")),
            session2: env("SESSION2"),
            session3: env("SESSION3"),
            bot_token: env("BOT_TOKEN"),
            bot_token2: env("BOT_TOKEN2"),
            bot_token3: env("BOT_TOKEN3"),
            log_channel: env_int("LOG_CHANNEL").unwrap_or(0),
            redis_uri,
            redis_password: arg(5).or_else(|| env("REDIS_PASSWORD")),
            database_url: env("DATABASE_URL"),
            mongo_uri: env("MONGO_URI")
                .or_else(|| env("MONGODB_URI"))
                .or_else(|| env("MONGODB_URL"))
                .or_else(|| env("MONGO_URL")),
            heroku_app_name: env("HEROKU_APP_NAME"),
            heroku_api: env("HEROKU_API"),
            vc_session: env("VC_SESSION"),
            addons: env_bool("ADDONS", false),
            vcbot: env_bool("VCBOT", false),
            app_version: env("APP_VERSION").unwrap_or_else(|| ULTROID_VERSION.to_string()),
            device_model: env("DEVICE_MODEL").unwrap_or_else(|| "Ultroid".to_string()),
            sudo_special_ids: env_int_list("SUDO_SPECIAL_IDS"),
            devlist_ids: env_int_list("DEVLIST_IDS"),
            apimix_api_key: env("APIMIX_API_KEY"),
            apimix_base_url: env("APIMIX_BASE_URL")
                .unwrap_or_else(|| "https://api.apmix.ai/v1".to_string()),
            apimix_model: env("APIMIX_MODEL"),
            bot_mode: env_bool("BOT_MODE", false),
            setup_token: env("SETUP_TOKEN"),
            setup_bind: env("SETUP_BIND").unwrap_or_else(|| "0.0.0.0".to_string()),
            port: env_int("PORT").map(|v| v as u16).unwrap_or(7860),
            space_url: env("SPACE_URL"),
            hf_token: env("HF_TOKEN"),
            keep_alive: env_bool("KEEP_ALIVE", false),
            openai_api_key: env("OPENAI_API_KEY"),
            anthropic_api_key: env("ANTHROPIC_API_KEY"),
            gemini_api_key: env("GEMINI_API_KEY").or_else(|| env("GOOGLE_API_KEY")),
            groq_api_key: env("GROQ_API_KEY"),
            xai_api_key: env("XAI_API_KEY"),
            meta_llama_api_key: env("META_LLAMA_API_KEY"),
            compat_base_url: env("COMPAT_BASE_URL"),
            compat_api_key: env("COMPAT_API_KEY"),
            compat_model: env("COMPAT_MODEL"),
            owner_id: env_int("OWNER_ID").unwrap_or(0),
            lang: env("LANG").unwrap_or_else(|| "en".to_string()),
            hndlr: env("HNDLR").unwrap_or_else(|| ".".to_string()),
            dual_hndlr: env("DUAL_HNDLR").unwrap_or_else(|| "/".to_string()),
            sudo_hndlr: env("SUDO_HNDLR").unwrap_or_else(|| ".".to_string()),
            data_dir: env("ULTROID_DATA_DIR").unwrap_or_else(|| ".".to_string()),
        })
    }

    /// Session file path for a named client (grammers SqliteSession).
    pub fn session_path(&self, name: &str) -> std::path::PathBuf {
        std::path::Path::new(&self.data_dir)
            .join("sessions")
            .join(format!("{name}.db"))
    }

    pub fn is_sudo(&self, user_id: i64) -> bool {
        user_id != 0 && (user_id == self.owner_id || self.sudo_special_ids.contains(&user_id))
    }

    pub fn is_dev(&self, user_id: i64) -> bool {
        user_id != 0 && (user_id == self.owner_id || self.devlist_ids.contains(&user_id))
    }
}
