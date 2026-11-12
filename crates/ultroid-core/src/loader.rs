//! Plugin loader / manifests — port of `pyUltroid/_core/loader.py`,
//! `manifest.py`, `dynamic.py` (dynamic load/unload/reload).
//!
//! Rust has no runtime plugin loading: "loading" a plugin means enabling
//! its already-compiled module in the `Registry`; unloading disables it.

use crate::router::Registry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub scope: String, // PluginPart scope, e.g. "user" / "assistant" / "both"
}

#[derive(Default)]
pub struct ManifestRegistry {
    manifests: HashMap<String, PluginManifest>,
}

impl ManifestRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, manifest: PluginManifest) {
        self.manifests.insert(manifest.name.clone(), manifest);
    }

    pub fn get(&self, name: &str) -> Option<&PluginManifest> {
        self.manifests.get(name)
    }

    pub fn all(&self) -> Vec<&PluginManifest> {
        self.manifests.values().collect()
    }
}

/// Loader: enable/disable plugins in the registry (`.install`/`.unload`/`.load`).
pub struct PluginLoader<'a> {
    pub registry: &'a mut Registry,
    pub manifests: &'a mut ManifestRegistry,
}

impl<'a> PluginLoader<'a> {
    pub fn load(&mut self, plugin: &str) -> bool {
        self.registry.set_plugin_enabled(plugin, true);
        true
    }

    pub fn unload(&mut self, plugin: &str) -> bool {
        self.registry.set_plugin_enabled(plugin, false);
        true
    }

    pub fn is_loaded(&self, plugin: &str) -> bool {
        self.registry.is_plugin_enabled(plugin)
    }

    pub fn loaded_plugins(&self) -> Vec<String> {
        self.manifests
            .all()
            .into_iter()
            .filter(|m| self.is_loaded(&m.name))
            .map(|m| m.name.clone())
            .collect()
    }
}
