//! Config file (`~/.config/lazymongo/config.toml`) and persisted state
//! (query history and pipelines, `~/.config/lazymongo/state.json`).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const HISTORY_CAP: usize = 50;

/// Saved connection (FR-2/FR-3). Secrets are never stored inline unless the
/// user opts to: `uri_env` references an environment variable instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedConnection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Theme name (see theme::NAMES); None = default dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// DNS resolver for mongodb+srv lookups: "system" (default),
    /// "cloudflare", "google", or "quad9" — use a public one if your
    /// local/VPN DNS mangles SRV records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<String>,
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

/// Write the config back to config.toml (in-app connection manager and
/// theme switching). Note: rewriting drops any hand-written comments.
pub fn save_config(config: &Config) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    let body = format!(
        "# lazymongo connections — managed in-app (C key) or by hand.\n# Prefer uri_env over uri for secrets: the URI is read from that env var.\n\n{body}"
    );
    std::fs::write(dir.join("config.toml"), body).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips_through_toml() {
        let conns = vec![
            SavedConnection {
                name: "local".into(),
                uri: Some("mongodb://localhost:27017".into()),
                uri_env: None,
                read_only: false,
            },
            SavedConnection {
                name: "prod".into(),
                uri: None,
                uri_env: Some("MONGO_PROD_URI".into()),
                read_only: true,
            },
        ];
        let body = toml::to_string_pretty(&Config {
            theme: Some("claude-dark".into()),
            dns: Some("cloudflare".into()),
            connections: conns.clone(),
        })
        .unwrap();
        // Inline `uri = none` must not be emitted.
        assert!(!body.contains("uri =") || body.contains("uri = \"mongodb"));
        let parsed: Config = toml::from_str(&body).unwrap();
        assert_eq!(parsed.connections.len(), 2);
        assert_eq!(parsed.connections[0].name, "local");
        assert_eq!(
            parsed.connections[1].uri_env.as_deref(),
            Some("MONGO_PROD_URI")
        );
        assert!(parsed.connections[1].read_only);
        assert!(parsed.connections[1].uri.is_none());
    }

    #[test]
    fn resolve_uri_precedence_and_errors() {
        let c = SavedConnection {
            name: "x".into(),
            uri: Some("mongodb://a".into()),
            uri_env: Some("SOME_VAR".into()),
            read_only: false,
        };
        assert_eq!(c.resolve_uri().unwrap(), "mongodb://a");
        let c = SavedConnection {
            name: "y".into(),
            uri: None,
            uri_env: None,
            read_only: false,
        };
        assert!(c.resolve_uri().is_err());
    }
}
