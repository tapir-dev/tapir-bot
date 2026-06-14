//! A minimal runnable Slack bot over tapir-bot — the reference binary.
//!
//! Run it with the three secrets in the environment and a `tapir-bot.toml`:
//!
//! ```sh
//! export SLACK_APP_TOKEN=xapp-...   # opens the Socket Mode connection
//! export SLACK_BOT_TOKEN=xoxb-...   # speaks the Web API
//! export ANTHROPIC_API_KEY=sk-...   # the model provider (see [agent] config)
//! cargo run --example slack_bot                 # reads ./tapir-bot.toml
//! cargo run --example slack_bot -- path/to.toml # or a given config
//! ```
//!
//! `RUST_LOG=debug` turns up the logging; `Ctrl-C` stops the bot.

use std::path::{Path, PathBuf};

use anyhow::Context;
use tapir_bot::{
    Bot, Config,
    slack::{SlackBackend, SlackConfig},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    // Loading the config is the binary's job — the libraries only define the
    // typed structs. The binary picks the source and format and splits it: the
    // neutral config (agent/storage/sandbox) feeds the engine, the Slack config
    // (reactions/access) feeds the backend. Here, a TOML file at the first
    // argument, or `tapir-bot.toml` in the cwd.
    let path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| "tapir-bot.toml".into());
    let (config, slack) = load_config(&path)?;

    Bot::new(config).run(SlackBackend::from_env(slack)?).await
}

/// Read a `tapir-bot.toml` and split it into the neutral engine config and the
/// Slack backend config, naming the file on any failure.
fn load_config(path: &Path) -> anyhow::Result<(Config, SlackConfig)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the config at {}", path.display()))?;
    let config = toml::from_str(&text)
        .with_context(|| format!("invalid config at {}", path.display()))?;
    let slack = toml::from_str(&text)
        .with_context(|| format!("invalid config at {}", path.display()))?;
    Ok((config, slack))
}

/// Initialize logging, honoring `RUST_LOG` and defaulting to `info`.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
