//! Single owner of AI provider configuration.
//! Port of `pyUltroid/agent/_config.py`.
//!
//! Configuration lives in the database under `AGENT_*` keys (one value
//! per provider field); the legacy environment reaches the database only
//! through `config_migrate`, which runs once at startup and is guarded
//! by the `AGENT_ENV_MIGRATED` flag. Values already set through the menu
//! are never overwritten by migration.

use std::sync::Arc;
use ultroid_core::db::Database;
use ultroid_core::error::{Error, Result};

/// Set once the legacy environment has been copied into the database.
pub const MIGRATED_KEY: &str = "AGENT_ENV_MIGRATED";

/// Which provider a bare `.ai` command uses. Unset means the registry's
/// own cost-tier routing decides.
pub const DEFAULT_PROVIDER_KEY: &str = "AGENT_PROVIDER";

/// One provider's configuration addressing.
/// `keys`: field -> database keys holding it, most specific first; the
/// first key that exists wins. `legacy`: field -> (env var, db key) pairs
/// whose values `config_migrate` copies into the field's first key.
pub struct ProviderSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub needs_key: bool,
    pub registry_name: Option<&'static str>,
    pub keys: &'static [(&'static str, &'static [&'static str])],
    pub legacy: &'static [(&'static str, &'static [(&'static str, Option<&'static str>)])],
}

/// provider id -> how its configuration is addressed.
/// Mirrors `PROVIDERS` in `_config.py` exactly (key names, order, labels).
pub static PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "openai",
        label: "OpenAI",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_OPENAI_KEY"]),
            ("MODEL", &["AGENT_CLOUD_OPENAI_MODEL"]),
            ("BASE", &["AGENT_CLOUD_OPENAI_BASE"]),
            ("SYS", &["AGENT_CLOUD_OPENAI_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("OPENAI_API_KEY", Some("OPENAI_API_KEY"))]),
            ("MODEL", &[("OPENAI_MODEL", Some("OPENAI_MODEL"))]),
            ("BASE", &[("OPENAI_BASE_URL", Some("OPENAI_BASE_URL"))]),
        ],
    },
    ProviderSpec {
        id: "gemini",
        label: "Google Gemini",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_GEMINI_KEY"]),
            ("MODEL", &["AGENT_CLOUD_GEMINI_MODEL"]),
            ("BASE", &["AGENT_CLOUD_GEMINI_BASE"]),
            ("SYS", &["AGENT_CLOUD_GEMINI_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("GEMINI_API_KEY", None), ("GOOGLE_API_KEY", None)]),
            ("MODEL", &[("GEMINI_MODEL", None)]),
        ],
    },
    ProviderSpec {
        id: "anthropic",
        label: "Anthropic",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLAUDE_KEY", "AGENT_CLOUD_ANTHROPIC_KEY"]),
            ("MODEL", &["AGENT_CLAUDE_MODEL", "AGENT_CLOUD_ANTHROPIC_MODEL"]),
            ("BASE", &["AGENT_CLOUD_ANTHROPIC_BASE"]),
            ("SYS", &["AGENT_CLAUDE_SYS", "AGENT_CLOUD_ANTHROPIC_SYS"]),
            ("FALLBACK", &["AGENT_CLAUDE_FALLBACK"]),
        ],
        legacy: &[
            ("KEY", &[("ANTHROPIC_API_KEY", None)]),
            ("MODEL", &[("ANTHROPIC_MODEL", None)]),
        ],
    },
    ProviderSpec {
        id: "groq",
        label: "Groq",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_GROQ_KEY"]),
            ("MODEL", &["AGENT_CLOUD_GROQ_MODEL"]),
            ("BASE", &["AGENT_CLOUD_GROQ_BASE"]),
            ("SYS", &["AGENT_CLOUD_GROQ_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("GROQ_API_KEY", None)]),
            ("MODEL", &[("GROQ_MODEL", None)]),
        ],
    },
    ProviderSpec {
        id: "openrouter",
        label: "OpenRouter",
        needs_key: true,
        registry_name: Some("openrouter"),
        keys: &[
            ("KEY", &["AGENT_CLOUD_OPENROUTER_KEY"]),
            ("MODEL", &["AGENT_CLOUD_OPENROUTER_MODEL"]),
            ("BASE", &["AGENT_CLOUD_OPENROUTER_BASE"]),
            ("SYS", &["AGENT_CLOUD_OPENROUTER_SYS"]),
        ],
        legacy: &[],
    },
    ProviderSpec {
        id: "ollama",
        label: "Ollama",
        needs_key: false,
        registry_name: None,
        keys: &[
            ("BASE", &["AGENT_LLAMA_OLLAMA_BASE"]),
            ("MODEL", &["AGENT_LLAMA_OLLAMA_MODEL"]),
            ("CTX", &["AGENT_LLAMA_OLLAMA_CTX"]),
            ("TEMP", &["AGENT_LLAMA_OLLAMA_TEMP"]),
            ("SYS", &["AGENT_LLAMA_OLLAMA_SYS"]),
        ],
        legacy: &[
            ("BASE", &[("OLLAMA_URL", None)]),
            ("MODEL", &[("OLLAMA_MODEL", Some("OLLAMA_MODEL"))]),
        ],
    },
    ProviderSpec {
        id: "openai_compat",
        label: "OpenAI-compatible",
        needs_key: true,
        registry_name: Some("openai_compat"),
        keys: &[
            ("KEY", &["AGENT_CLOUD_COMPAT_KEY"]),
            ("MODEL", &["AGENT_CLOUD_COMPAT_MODEL"]),
            ("BASE", &["AGENT_CLOUD_COMPAT_BASE"]),
            ("SYS", &["AGENT_CLOUD_COMPAT_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("COMPAT_API_KEY", Some("COMPAT_API_KEY"))]),
            ("MODEL", &[("COMPAT_MODEL", Some("COMPAT_MODEL"))]),
            ("BASE", &[("COMPAT_BASE_URL", Some("COMPAT_BASE_URL"))]),
        ],
    },
    ProviderSpec {
        id: "xai",
        label: "xAI",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_XAI_KEY"]),
            ("MODEL", &["AGENT_CLOUD_XAI_MODEL"]),
            ("BASE", &["AGENT_CLOUD_XAI_BASE"]),
            ("SYS", &["AGENT_CLOUD_XAI_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("XAI_API_KEY", None)]),
            ("MODEL", &[("XAI_MODEL", None)]),
            ("BASE", &[("XAI_BASE_URL", None)]),
        ],
    },
    ProviderSpec {
        id: "apimix",
        label: "apmix",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_APIMIX_KEY"]),
            ("MODEL", &["AGENT_CLOUD_APIMIX_MODEL"]),
            ("BASE", &["AGENT_CLOUD_APIMIX_BASE"]),
            ("SYS", &["AGENT_CLOUD_APIMIX_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("APIMIX_API_KEY", None)]),
            ("MODEL", &[("APIMIX_MODEL", None)]),
            ("BASE", &[("APIMIX_BASE_URL", None)]),
        ],
    },
    ProviderSpec {
        id: "meta_llama",
        label: "Meta Llama",
        needs_key: true,
        registry_name: None,
        keys: &[
            ("KEY", &["AGENT_CLOUD_META_KEY"]),
            ("MODEL", &["AGENT_CLOUD_META_MODEL"]),
            ("BASE", &["AGENT_CLOUD_META_BASE"]),
            ("SYS", &["AGENT_CLOUD_META_SYS"]),
        ],
        legacy: &[
            ("KEY", &[("META_LLAMA_API_KEY", None)]),
            ("MODEL", &[("META_LLAMA_MODEL", None)]),
            ("BASE", &[("META_LLAMA_BASE_URL", None)]),
        ],
    },
];

pub fn provider_spec(provider: &str) -> Result<&'static ProviderSpec> {
    let name = if provider.eq_ignore_ascii_case("compat") {
        "openai_compat"
    } else {
        &provider.to_lowercase()
    };
    PROVIDERS
        .iter()
        .find(|s| s.id == name)
        .ok_or_else(|| {
            Error::Config(format!(
                "Unknown AI provider {provider:?}; expected one of: {}",
                PROVIDERS
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// The AI provider configuration store (`AGENT_*` database keys).
#[derive(Clone)]
pub struct AgentConfig {
    db: Arc<dyn Database>,
}

impl AgentConfig {
    pub fn new(db: Arc<dyn Database>) -> Self {
        AgentConfig { db }
    }

    async fn read(&self, key: &str) -> Option<String> {
        self.db.get(key).await.ok().flatten()
    }

    async fn write(&self, key: &str, value: &str) -> bool {
        self.db.set(key, value).await.is_ok()
    }

    /// The configured value for *provider*'s *field*, or *default*.
    /// Only the database is read.
    pub async fn config_field(
        &self,
        provider: &str,
        field: &str,
        default: Option<String>,
    ) -> Result<Option<String>> {
        let spec = provider_spec(provider)?;
        for key in spec.keys.iter().find(|(f, _)| *f == field).map(|(_, k)| *k).unwrap_or(&[]) {
            if let Some(v) = self.read(key).await {
                if !v.is_empty() {
                    return Ok(Some(v));
                }
            }
        }
        Ok(default)
    }

    /// Write *field* for *provider* to its first database key.
    pub async fn config_set(
        &self,
        provider: &str,
        field: &str,
        value: Option<&str>,
    ) -> Result<bool> {
        let spec = provider_spec(provider)?;
        let Some(keys) = spec.keys.iter().find(|(f, _)| *f == field).map(|(_, k)| *k) else {
            return Ok(false);
        };
        match value {
            None | Some("") => self.config_clear(provider, field).await,
            Some(v) => Ok(self.write(keys[0], v).await),
        }
    }

    pub async fn config_clear(&self, provider: &str, field: &str) -> Result<bool> {
        let spec = provider_spec(provider)?;
        let mut cleared = false;
        if let Some(keys) = spec.keys.iter().find(|(f, _)| *f == field).map(|(_, k)| *k) {
            for key in keys {
                if self.db.del(key).await.is_ok() {
                    cleared = true;
                }
            }
        }
        Ok(cleared)
    }

    /// Which database key holds *field*, for showing provenance in the UI.
    pub async fn config_source(&self, provider: &str, field: &str) -> Result<Option<String>> {
        let spec = provider_spec(provider)?;
        if let Some(keys) = spec.keys.iter().find(|(f, _)| *f == field).map(|(_, k)| *k) {
            for key in keys {
                if let Some(v) = self.read(key).await {
                    if !v.is_empty() {
                        return Ok(Some(key.to_string()));
                    }
                }
            }
        }
        Ok(None)
    }

    fn legacy_value(&self, env_attr: &str, db_key: Option<&str>) -> Option<String> {
        if let Ok(v) = std::env::var(env_attr) {
            if !v.is_empty() {
                return Some(v);
            }
        }
        let _ = db_key;
        None
    }

    /// Copy legacy environment values into the unified keys. Runs once;
    /// the `AGENT_ENV_MIGRATED` flag guards it. Returns the keys written.
    pub async fn config_migrate(&self, force: bool) -> Result<Vec<String>> {
        let _ = self;
        let mut written: Vec<String> = Vec::new();
        let mut failed = 0usize;
        // DB reads are async; do the flag check first via a fresh read.
        if !force && self.read(MIGRATED_KEY).await.is_some() {
            return Ok(written);
        }
        for spec in PROVIDERS {
            for legacy_entry in spec.legacy.iter() {
                let (field, entries) = *legacy_entry;
                let Some(keys) = spec.keys.iter().find(|(f, _)| *f == field).map(|(_, k)| *k)
                else {
                    continue;
                };
                let mut already = false;
                for key in keys {
                    if let Some(v) = self.read(key).await {
                        if !v.is_empty() {
                            already = true;
                            break;
                        }
                    }
                }
                if already {
                    continue;
                }
                for entry in entries.iter() {
                    let (env_attr, db_key) = *entry;
                    // Legacy DB keys: check the database first for that key.
                    let mut value = self.legacy_value(env_attr, db_key);
                    if value.is_none() {
                        if let Some(k) = db_key {
                            if let Some(v) = self.read(k).await {
                                if !v.is_empty() {
                                    value = Some(v);
                                }
                            }
                        }
                    }
                    if let Some(v) = value {
                        if self.write(keys[0], &v).await {
                            written.push(keys[0].to_string());
                        } else {
                            failed += 1;
                        }
                        break;
                    }
                }
            }
        }
        if failed == 0 {
            let _ = self.write(MIGRATED_KEY, "1").await;
        }
        Ok(written)
    }

    /// Whether *provider* has what it needs to run.
    pub async fn config_ready(&self, provider: &str) -> Result<bool> {
        let spec = provider_spec(provider)?;
        if spec.needs_key {
            Ok(self
                .config_field(provider, "KEY", None)
                .await?
                .is_some())
        } else {
            Ok(self
                .config_field(provider, "BASE", None)
                .await?
                .is_some())
        }
    }

    /// Effective configuration per provider, for `.aiconf` and the menu.
    pub async fn config_status(&self) -> Result<Vec<ProviderStatus>> {
        let mut out = Vec::new();
        for spec in PROVIDERS {
            let mut fields = Vec::new();
            let mut sources = Vec::new();
            for (field, _) in spec.keys {
                fields.push((
                    field.to_string(),
                    self.config_field(spec.id, field, None).await?,
                ));
                sources.push((
                    field.to_string(),
                    self.config_source(spec.id, field).await?,
                ));
            }
            out.push(ProviderStatus {
                provider: spec.id.to_string(),
                label: spec.label.to_string(),
                configured: self.config_ready(spec.id).await?,
                fields,
                sources,
            });
        }
        Ok(out)
    }

    /// Resolve a provider pin, or configured apimix as the default.
    pub async fn config_default_provider(&self) -> Result<Option<String>> {
        if let Some(value) = self.read(DEFAULT_PROVIDER_KEY).await {
            let mut v = value.to_lowercase();
            if v == "compat" {
                v = "openai_compat".to_string();
            }
            if PROVIDERS.iter().any(|s| s.id == v) {
                return Ok(Some(v));
            }
        }
        // A configured apimix account is the preferred route for unpinned
        // .ai/.ask requests; a saved explicit provider choice still wins.
        if self.config_ready("apimix").await? {
            return Ok(Some("apimix".to_string()));
        }
        Ok(None)
    }

    /// Pin the provider for bare AI commands, or clear it with `None`.
    /// `require_configured` prevents recording a pin that can never run.
    pub async fn config_use_default(
        &self,
        provider: Option<&str>,
        require_configured: bool,
    ) -> Result<bool> {
        match provider {
            None => Ok(self.db.del(DEFAULT_PROVIDER_KEY).await.is_ok()),
            Some(p) => {
                let spec = provider_spec(p)?;
                if require_configured && !self.config_ready(spec.id).await? {
                    return Err(Error::Config(format!(
                        "Provider {p:?} is not configured; configure it first or keep automatic routing."
                    )));
                }
                let name = spec.registry_name.unwrap_or(spec.id).to_lowercase();
                Ok(self.write(DEFAULT_PROVIDER_KEY, &name).await)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub provider: String,
    pub label: String,
    pub configured: bool,
    pub fields: Vec<(String, Option<String>)>,
    pub sources: Vec<(String, Option<String>)>,
}
