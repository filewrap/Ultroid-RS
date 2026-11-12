//! Command router — port of `pyUltroid/_misc/_decorators.py`
//! (`PatternCompiler`, `ultroid_cmd` gates) and the dispatcher fan-out
//! in `pyUltroid/startup/dispatcher.py`.

use crate::context::Ctx;
use crate::error::{Error, Result};
use async_trait::async_trait;
use regex::Regex;
use std::sync::Arc;

/// One command registration. Mirrors the `ultroid_cmd(...)` kwargs.
#[derive(Debug, Clone)]
pub struct CommandDef {
    pub name: &'static str,
    /// Python pattern as written in the plugin (without handler prefix).
    pub pattern: &'static str,
    pub help: &'static str,
    /// Owning plugin module (set by `Registry::set_plugin` while the
    /// module registers; "" for entries registered without a plugin).
    pub plugin: &'static str,
    pub owner_only: bool,
    pub sudo_allowed: bool,
    pub groups_only: bool,
    pub admins_only: bool,
    pub fullsudo: bool,
    pub only_devs: bool,
    pub allow_guest: bool,
    /// Python `silent_denials` (default True): unauthorized attempts are
    /// silently ignored instead of getting an explicit denial.
    pub silent_denials: bool,
}

impl Default for CommandDef {
    fn default() -> Self {
        CommandDef {
            name: "",
            pattern: "",
            help: "",
            plugin: "",
            owner_only: false,
            sudo_allowed: true,
            groups_only: false,
            admins_only: false,
            fullsudo: false,
            only_devs: false,
            allow_guest: false,
            silent_denials: true,
        }
    }
}

/// Compile a plugin pattern the way Python's `PatternCompiler.compile` does:
/// strip a leading `^` / `.`, prepend `\` + handler, anchored at the start.
/// Rust adds `(?s)` (dotall) because Python `re` patterns such as
/// `([\\s\\S]*)` already assume `.` matches newlines in some places, and
/// Telethon matches with `pattern.match` (start-anchored, not full).
pub fn compile_pattern(pattern: &str, hndlr: char) -> Result<Regex> {
    let mut data = pattern;
    if let Some(rest) = data.strip_prefix('^') {
        data = rest;
    }
    if let Some(rest) = data.strip_prefix('.') {
        data = rest;
    }
    let escaped = regex::escape(&hndlr.to_string());
    Regex::new(&format!("(?s)^{escaped}{data}")).map_err(Error::from)
}

#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn handle(&self, ctx: &Ctx) -> Result<()>;
}

struct Entry {
    def: CommandDef,
    /// Compiled for both `.` (HNDLR/SUDO_HNDLR) and `/` (DUAL_HNDLR).
    regexes: Vec<Regex>,
    handler: Arc<dyn CommandHandler>,
}

pub enum DispatchOutcome {
    Matched,
    NoMatch,
    /// Pattern matched but the sender failed a gate (silent or explicit).
    Denied(String),
}

pub struct Registry {
    entries: Vec<Entry>,
    /// `plugin_name -> enabled` (dynamic unload/reload is enable/disable;
    /// Rust has no runtime plugin loading).
    pub enabled: std::collections::HashMap<String, bool>,
    /// Plugin currently registering (set by the coordinator before each
    /// module's `register` call; stamped onto each `CommandDef`).
    current_plugin: &'static str,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            entries: Vec::new(),
            enabled: std::collections::HashMap::new(),
            current_plugin: "",
        }
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tag subsequent `add` calls with this plugin (coordinator calls
    /// it once per module before that module's `register`).
    pub fn set_plugin(&mut self, plugin: &'static str) {
        self.current_plugin = plugin;
    }

    pub fn add(&mut self, mut def: CommandDef, handler: Arc<dyn CommandHandler>) {
        def.plugin = self.current_plugin;
        let mut regexes = Vec::new();
        for h in ['.', '/'] {
            if let Ok(re) = compile_pattern(def.pattern, h) {
                regexes.push(re);
            }
        }
        self.entries.push(Entry { def, regexes, handler });
    }

    pub fn set_plugin_enabled(&mut self, plugin: &str, enabled: bool) {
        self.enabled.insert(plugin.to_string(), enabled);
    }

    pub fn is_plugin_enabled(&self, plugin: &str) -> bool {
        self.enabled.get(plugin).copied().unwrap_or(true)
    }

    pub fn command_count(&self) -> usize {
        self.entries.len()
    }

    pub fn command_names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|e| e.def.name).collect()
    }

    /// Help listing grouped by plugin in first-registration order:
    /// `(plugin, commands)`. Powers `.help <plugin>` (port of
    /// `_help.py`'s HELP/LIST lookup). Entries registered without a
    /// plugin tag are skipped.
    pub fn help_listing(&self) -> Vec<(&'static str, Vec<&CommandDef>)> {
        let mut order: Vec<&'static str> = Vec::new();
        let mut groups: std::collections::HashMap<&'static str, Vec<&CommandDef>> =
            std::collections::HashMap::new();
        for entry in &self.entries {
            if entry.def.plugin.is_empty() {
                continue;
            }
            if !groups.contains_key(entry.def.plugin) {
                order.push(entry.def.plugin);
            }
            groups.entry(entry.def.plugin).or_default().push(&entry.def);
        }
        order
            .into_iter()
            .map(|p| (p, groups.remove(p).unwrap_or_default()))
            .collect()
    }

    /// Dispatch one incoming message context. Mirrors the `ultroid_cmd`
    /// wrapper: via-bot rejection, owner/sudo/dev/guest gates, groups-only.
    pub async fn dispatch(&self, ctx: &mut Ctx) -> Result<DispatchOutcome> {
        for entry in &self.entries {
            let caps = match entry.regexes.iter().find_map(|re| re.captures(&ctx.text)) {
                Some(c) => c,
                None => continue,
            };
            let def = &entry.def;
            // Default func: reject messages sent via a bot (Python: `not e.via_bot_id`).
            if ctx.via_bot {
                return Ok(DispatchOutcome::Denied("via_bot".into()));
            }
            // Gates (order mirrors the decorator wrapper).
            if def.only_devs && !ctx.is_dev {
                return Ok(self.deny(ctx, def, "only devs may use this command").await);
            }
            if def.owner_only && !ctx.is_owner {
                return Ok(self.deny(ctx, def, "only the owner may use this command").await);
            }
            if def.fullsudo && !(ctx.is_owner || ctx.is_sudo) {
                return Ok(self.deny(ctx, def, "only the owner and full sudos may use this command").await);
            }
            if !def.sudo_allowed && !ctx.is_owner && ctx.is_sudo {
                return Ok(self.deny(ctx, def, "sudos may not use this command").await);
            }
            if def.groups_only
                && !matches!(ctx.chat_type, crate::context::ChatType::Group | crate::context::ChatType::Supergroup)
            {
                return Ok(self.deny(ctx, def, "this command works only in groups").await);
            }
            if def.allow_guest && ctx.is_guest {
                // guests pass; non-guest non-owner non-sudo are denied
                if !(ctx.is_owner || ctx.is_sudo) {
                    return Ok(self.deny(ctx, def, "guests only").await);
                }
            }
            if ctx.is_guest && !def.allow_guest && !ctx.is_owner && !ctx.is_sudo {
                return Ok(self.deny(ctx, def, "guests may not use this command").await);
            }
            if def.admins_only && !(ctx.is_owner || ctx.is_sudo) {
                // Full admin check needs a Telegram call; the binary fills
                // `is_sudo` for group admins where known. Treat non-sudo as denied.
                return Ok(self.deny(ctx, def, "only admins may use this command").await);
            }

            // Populate captures (group 0 = whole match).
            ctx.captures = caps
                .iter()
                .map(|g| g.map(|m| m.as_str().to_string()).unwrap_or_default())
                .collect();
            ctx.command = def.name;
            // Common convention: args = group 1 when the pattern is
            // `cmd( (.*)|$)`-shaped, else the text after the first word.
            ctx.args = if ctx.captures.len() > 1 && !ctx.captures[1].is_empty() {
                let g1 = ctx.captures[1].trim().to_string();
                // group 1 for `cmd( (.*)|$)` includes the leading space
                g1
            } else if ctx.captures.len() > 2 {
                ctx.captures[2].clone()
            } else {
                ctx.text
                    .splitn(2, char::is_whitespace)
                    .nth(1)
                    .unwrap_or("")
                    .to_string()
            };
            entry.handler.handle(ctx).await?;
            return Ok(DispatchOutcome::Matched);
        }
        Ok(DispatchOutcome::NoMatch)
    }

    async fn deny(&self, ctx: &Ctx, def: &CommandDef, reason: &str) -> DispatchOutcome {
        if def.silent_denials {
            tracing::debug!(command = def.name, sender = ctx.sender_id, "silent denial: {reason}");
            DispatchOutcome::Denied(reason.to_string())
        } else {
            let _ = ctx.reply(&format!("⛔ {reason}")).await;
            DispatchOutcome::Denied(reason.to_string())
        }
    }
}
