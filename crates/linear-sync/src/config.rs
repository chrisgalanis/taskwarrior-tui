use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub api_token: String,
    pub team_id: String,
    pub assignee_id: String,
    /// Workflow state ID for the "completed/done" state in the chosen team.
    pub done_state_id: String,
    /// Workflow state ID for the "in progress/started" state in the chosen team.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_progress_state_id: Option<String>,
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            })
            .join("linear-sync")
            .join("config.toml")
    }

    /// Load from env vars first, then fall back to the config file.
    pub fn load() -> Result<Self> {
        if let (Ok(token), Ok(team), Ok(assignee), Ok(done)) = (
            std::env::var("LINEAR_API_TOKEN"),
            std::env::var("LINEAR_TEAM_ID"),
            std::env::var("LINEAR_ASSIGNEE_ID"),
            std::env::var("LINEAR_DONE_STATE_ID"),
        ) {
            return Ok(Self {
                api_token: token,
                team_id: team,
                assignee_id: assignee,
                done_state_id: done,
                in_progress_state_id: std::env::var("LINEAR_IN_PROGRESS_STATE_ID").ok(),
            });
        }

        let path = Self::path();
        let content = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Config not found at {}. Run `linear-sync setup` first.",
                path.display()
            )
        })?;
        toml::from_str(&content).context("Invalid config file")
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("Failed to create config directory")?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("Failed to write config to {}", path.display()))
    }
}
