//! tapir-bot — the facade for building chat bots on the tapir agent SDK.
//!
//! It ties [`tapir_bot_core`] (the backend-neutral engine) to a concrete chat
//! backend. A bot is a thin binary:
//!
//! ```no_run
//! use tapir_bot::{Bot, Config, slack::{SlackBackend, SlackConfig}};
//!
//! # async fn run() -> anyhow::Result<()> {
//! // The binary owns config loading; the libraries only define the typed
//! // structs. The neutral config (agent/storage/sandbox) and the Slack config
//! // (reactions/access) are read from the same file.
//! let text = std::fs::read_to_string("tapir-bot.toml")?;
//! let config: Config = toml::from_str(&text)?;
//! let slack: SlackConfig = toml::from_str(&text)?;
//! Bot::new(config).run(SlackBackend::from_env(slack)?).await
//! # }
//! ```
//!
//! Swap `SlackBackend` for any other [`ChatBackend`] (Discord, IRC, Google
//! Chat, Teams, …) to target a different service — the engine is the same.

use std::path::PathBuf;
use std::sync::Arc;

use tapir_bot_core::Engine;

pub use tapir_bot_core::{self as core, ChatBackend, Config, Inbound, ReplySink, config};

/// The Slack backend, re-exported when the `slack` feature is on (default).
#[cfg(feature = "slack")]
pub use tapir_bot_slack as slack;

/// A bot to run: a [`Config`] plus where its skills live. Build it, then [`run`]
/// it against a [`ChatBackend`].
///
/// [`run`]: Bot::run
pub struct Bot {
    config: Config,
    skills_dir: Option<PathBuf>,
}

impl Bot {
    /// Start from a config. Skills default to `./skills` (used only when
    /// `[agent].tools` is `host`/`sandbox`, and only if the directory exists).
    pub fn new(config: Config) -> Self {
        let skills_dir = std::env::current_dir().ok().map(|dir| dir.join("skills"));
        Self { config, skills_dir }
    }

    /// Override the repo `skills/` directory provisioned into each tool
    /// workspace.
    pub fn skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.skills_dir = Some(dir.into());
        self
    }

    /// Disable skills entirely (no `skills/` provisioning).
    pub fn no_skills(mut self) -> Self {
        self.skills_dir = None;
        self
    }

    /// Build the engine from the config and run it on `backend` until the
    /// process is stopped. Resolves the model and validates the provider key up
    /// front, so a misconfigured bot fails before connecting.
    pub async fn run<B: ChatBackend>(self, backend: B) -> anyhow::Result<()> {
        let engine = Engine::from_config(self.config, self.skills_dir)?;
        let engine = Arc::new(engine);
        engine.start();
        backend.run(engine).await
    }
}
