use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `~/.config/papin-cli/config.toml` (§6).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    /// Gateway base URL, e.g. `http://10.77.0.1:8080` (plain HTTP inside WG).
    pub url: String,
    /// Device token (bearer auth).
    pub token: String,
    /// Default agent id (skips the picker).
    pub default_agent: Option<String>,
}

impl Default for CliConfig {
    fn default() -> Self {
        CliConfig {
            url: "http://10.77.0.1:8080".into(),
            token: String::new(),
            default_agent: None,
        }
    }
}

impl CliConfig {
    pub fn config_path() -> PathBuf {
        if let Ok(p) = std::env::var("PAPIN_CLI_CONFIG") {
            return PathBuf::from(p);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/papin-cli/config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// WebSocket URL for an agent's ACP endpoint.
    pub fn ws_url(&self, agent_id: &str) -> String {
        let host = self
            .url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        format!("ws://{host}/acp/agents/{agent_id}")
    }
}
