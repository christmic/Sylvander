use std::str::FromStr;

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

/// Bootstrap config. Only what's needed to start the server and open the DB.
/// Providers and logical models live in SQLite, not here.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_domain")]
    pub domain: String,
    #[serde(default = "default_db")]
    pub db_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: default_port(),
            bind: default_bind(),
            domain: default_domain(),
            db_path: default_db(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
}

impl Dialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Anthropic => "anthropic",
            Dialect::OpenaiChat => "openai_chat",
            Dialect::OpenaiResponses => "openai_responses",
        }
    }

    /// True for OpenAI-family dialects (Authorization: Bearer auth).
    pub fn is_openai(self) -> bool {
        matches!(self, Dialect::OpenaiChat | Dialect::OpenaiResponses)
    }
}

impl FromStr for Dialect {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "anthropic" => Ok(Dialect::Anthropic),
            "openai_chat" => Ok(Dialect::OpenaiChat),
            "openai_responses" => Ok(Dialect::OpenaiResponses),
            other => Err(format!(
                "unknown dialect `{other}` (expected anthropic|openai_chat|openai_responses)"
            )),
        }
    }
}

fn default_port() -> u16 {
    9527
}
fn default_bind() -> String {
    "127.0.0.1".to_string()
}
fn default_domain() -> String {
    "token9.oraculo.ai".to_string()
}
fn default_db() -> String {
    "~/.Oraculo/config/token9/token9.db".to_string()
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let path = expand_tilde(path);
        // Missing file is fine — defaults + env cover it.
        let cfg: Config = Figment::new()
            .merge(Toml::file(&path))
            .merge(Env::prefixed("TOKEN9_"))
            .extract()?;
        Ok(cfg)
    }
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}
