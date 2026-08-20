//! Config file (`~/.config/lazymongo/config.toml`) and persisted state
//! (query history and pipelines, `~/.config/lazymongo/state.json`).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const HISTORY_CAP: usize = 50;

/// Saved connection (FR-2/FR-3). Secrets are never stored inline unless the
/// user opts to: `uri_env` references an environment variable instead.
#[derive(Debug, Clone, Deserialize)]
pub struct SavedConnection {
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub uri_env: Option<String>,
    #[serde(default)]
    pub read_only: bool,
}

impl SavedConnection {
    pub fn resolve_uri(&self) -> Result<String, String> {
        if let Some(uri) = &self.uri {
            return Ok(uri.clone());
        }
        if let Some(var) = &self.uri_env {
            return std::env::var(var)
                .map_err(|_| format!("environment variable {var} is not set"));
        }
        Err(format!(
            "connection \"{}\" has neither uri nor uri_env",
            self.name
        ))
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lazymongo")
}

/// Load the config; a missing file is an empty config, a broken file is an
/// error the caller should surface.
pub fn load_config() -> Result<Config, String> {
    let path = config_dir().join("config.toml");
    match std::fs::read_to_string(&path) {
        Err(_) => Ok(Config::default()),
        Ok(body) => toml::from_str(&body).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Persisted per-collection state, keyed by "db.coll".
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub history: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub pipelines: HashMap<String, String>,
}

impl State {
    pub fn push_history(&mut self, ns: &str, entry: String) {
        let list = self.history.entry(ns.to_string()).or_default();
        if list.last() == Some(&entry) {
            return;
        }
        list.push(entry);
        if list.len() > HISTORY_CAP {
            let excess = list.len() - HISTORY_CAP;
            list.drain(..excess);
        }
    }
}

pub fn load_state() -> State {
    let path = config_dir().join("state.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

/// Best-effort save; the TUI shows a toast on failure.
pub fn save_state(state: &State) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("state.json"), body).map_err(|e| e.to_string())
}
