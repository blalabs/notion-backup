//! Runtime configuration loaded from the environment.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// Notion API version that introduces the database -> data_sources split.
/// See <https://developers.notion.com/docs/upgrade-guide-2025-09-03>.
pub const DEFAULT_API_VERSION: &str = "2025-09-03";
pub const DEFAULT_BACKUP_DIR: &str = "backup";

const TRUTHY: [&str; 4] = ["1", "true", "yes", "on"];

/// Raised when required configuration is missing or invalid.
#[derive(Debug)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Resolved configuration for a backup run.
#[derive(Debug, Clone)]
pub struct Config {
    pub notion_token: String,
    pub backup_dir: PathBuf,
    pub git_remote: Option<String>,
    pub api_version: String,
    pub render_markdown: bool,
    pub full_sync: bool,
}

impl Config {
    /// Directory holding the raw JSON tree (always written).
    pub fn json_dir(&self) -> PathBuf {
        self.backup_dir.join("json")
    }

    /// Directory holding the rendered Markdown/CSV tree (optional).
    pub fn markdown_dir(&self) -> PathBuf {
        self.backup_dir.join("markdown")
    }

    /// Build a `Config` from environment variables (and a `.env` file).
    ///
    /// `backup_dir_override` takes precedence over the `BACKUP_DIR` env var.
    /// `require_token` can be disabled for the offline render-only path.
    pub fn from_env(
        backup_dir_override: Option<&str>,
        require_token: bool,
    ) -> Result<Config, ConfigError> {
        dotenvy::dotenv().ok();

        let token = env_str("NOTION_TOKEN").unwrap_or_default();
        if require_token && token.is_empty() {
            return Err(ConfigError(
                "NOTION_TOKEN is not set. Create an internal integration at \
                 https://www.notion.so/my-integrations, share your pages with \
                 it, and put the token in your .env file."
                    .to_string(),
            ));
        }

        let backup_dir = backup_dir_override
            .map(str::to_string)
            .or_else(|| env_str("BACKUP_DIR"))
            .unwrap_or_else(|| DEFAULT_BACKUP_DIR.to_string());
        let git_remote = env_str("GIT_REMOTE");
        let api_version =
            env_str("NOTION_API_VERSION").unwrap_or_else(|| DEFAULT_API_VERSION.to_string());
        let render_markdown = env_truthy("RENDER_MARKDOWN");
        let full_sync = env_truthy("FULL_SYNC");

        Ok(Config {
            notion_token: token,
            backup_dir: resolve_dir(&backup_dir),
            git_remote,
            api_version,
            render_markdown,
            full_sync,
        })
    }
}

fn env_str(key: &str) -> Option<String> {
    match env::var(key) {
        Ok(v) => {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(_) => None,
    }
}

fn env_truthy(key: &str) -> bool {
    env::var(key)
        .map(|v| TRUTHY.contains(&v.trim().to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Expand a leading `~` and make the path absolute (relative to the cwd).
fn resolve_dir(value: &str) -> PathBuf {
    let expanded = expand_tilde(value);
    if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(&expanded))
            .unwrap_or(expanded)
    }
}

fn expand_tilde(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    } else if value == "~" {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(value)
}
