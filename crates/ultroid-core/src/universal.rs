//! Universal plugin/runtime abstraction — port of `pyUltroid/universal/`
//! and `pyUltroid/universal/adapters/`.

use crate::context::Ctx;
use crate::error::Result;
use async_trait::async_trait;

/// A runtime-agnostic plugin: anything that can handle a context.
#[async_trait]
pub trait UniversalPlugin: Send + Sync {
    fn name(&self) -> &'static str;
    async fn handle(&self, ctx: &Ctx) -> Result<()>;
}

/// Storage abstraction over the shared database (port of `universal/storage.py`).
pub trait UniversalStorage: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, value: &str);
}

/// Runtime mode: userbot, assistant bot, or managed bot (adapters/mode.py).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Userbot,
    Assistant,
    ManagedBot,
    BotOnly,
}
