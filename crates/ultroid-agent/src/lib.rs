//! `ultroid-agent` — the AI agent: providers, tools, loop, runtime.
//!
//! Port of `source/pyUltroid/agent/` (21 files). Providers talk to the
//! AI backends over reqwest; tools execute shell/file/web work; the loop
//! drives provider ↔ tool rounds; the runtime wires it to Telegram via
//! small local traits the binary crate implements (this crate has no
//! Telethon/grammers dependency by design).

pub mod agent;
pub mod bridge;
pub mod chatkey;
pub mod client;
pub mod config;
pub mod controls;
pub mod delivery;
pub mod interactive;
pub mod lifecycle;
pub mod r#loop;
pub mod mini;
pub mod presentation;
pub mod providers;
pub mod reporting;
pub mod runtime;
pub mod sessions;
pub mod skills;
pub mod telegram;
pub mod tgbridge;
pub mod tools;
