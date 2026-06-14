//! The backend-neutral runtime config — the typed input the [`Engine`] reads:
//! the model settings, storage, and tool sandbox. These are plain `Deserialize`
//! structs: how the config is obtained (a TOML file, env, args, another format)
//! is the binary's job, so the library does no I/O and picks no format.
//!
//! Backend-specific config (Slack reactions, the access allowlist, …) lives with
//! its backend, not here, so this struct only tolerates unknown top-level tables
//! (a backend's tables share the same file). Each table it *does* own denies
//! unknown keys, so a typo inside one fails loudly.
//!
//! [`Engine`]: crate::engine::Engine

use serde::Deserialize;

/// The backend-neutral runtime config: model, storage, and sandbox.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The runtime's model settings. Absent means the defaults (Anthropic, the
    /// catalog's default model, no system prompt).
    pub agent: Agent,
    /// Where the bot persists conversation history and memory files.
    pub storage: Storage,
    /// The tool sandbox. Disabled by default — without it the agent is
    /// text-only and never runs tools on the host.
    pub sandbox: Sandbox,
}

/// How the agent's tools are executed for a turn. The gate for tool use:
/// `None` keeps the agent text-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    /// Text-only: no tools run anywhere (the default).
    #[default]
    None,
    /// Tools run directly in the bot's own process (the pod), using the
    /// runtime's local exec/fs ops. Intended for a hardened/containerized
    /// (e.g. Kubernetes) deployment, where the pod is the isolation boundary.
    Host,
    /// Tools run in an isolated per-channel container (the `[sandbox]` block).
    Sandbox,
}

/// The runtime's model settings: which provider and model a turn runs on, an
/// optional standing system prompt, and how tools execute. The provider's API
/// key is read from the environment, never the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Agent {
    /// The tapir provider id (e.g. `anthropic`, `openai`, `google`).
    pub provider: String,
    /// The model id; `None` uses the catalog's default model for the provider.
    pub model: Option<String>,
    /// An optional standing system prompt prepended to every turn.
    pub system_prompt: Option<String>,
    /// How tools execute: `none` (text-only), `host` (in the pod), or
    /// `sandbox` (per-channel container, configured by `[sandbox]`).
    pub tools: ToolMode,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: None,
            system_prompt: None,
            tools: ToolMode::None,
        }
    }
}

/// Where the bot keeps its on-disk state: conversation transcripts (under
/// `<dir>/sessions`) and the `MEMORY.md` facts files. Holds conversation
/// content, so place it accordingly.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Storage {
    /// The data directory (conversation transcripts live under `dir/sessions`).
    /// Default `tapir-bot-data` in the working directory.
    pub dir: std::path::PathBuf,
    /// Where the `MEMORY.md` facts live (the global file and `memory/<channel>.md`).
    /// `None` uses `dir`, so memory sits alongside the transcripts by default;
    /// point it at an existing notes directory to keep facts elsewhere.
    pub memory_dir: Option<std::path::PathBuf>,
}

impl Default for Storage {
    fn default() -> Self {
        Self { dir: std::path::PathBuf::from("tapir-bot-data"), memory_dir: None }
    }
}

/// The tool sandbox: the agent's tools run in an isolated per-channel container
/// (one persistent sandbox per channel). Read only when `[agent].tools =
/// "sandbox"`. Defaults mirror tapir-sandbox's built-ins.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Sandbox {
    /// The container image (anything with `/bin/sh`). Build the repo Dockerfile
    /// for one with the CLIs (aws, kubectl, …) and set it here.
    pub image: String,
    /// Memory cap, in the runtime's syntax (e.g. `1g`).
    pub memory: String,
    /// CPU cap, in the runtime's syntax (e.g. `2`).
    pub cpus: String,
    /// Cap on concurrent processes (fork-bomb containment).
    pub pids: u32,
    /// Container network mode. `None` is the runtime's default bridge (egress
    /// on, needed for aws/kubectl/…); set `"none"` to isolate.
    pub network: Option<String>,
    /// Minutes without a turn before the idle reaper stops a channel's sandbox.
    pub idle_minutes: u64,
    /// Environment variable names to pass from the bot's process into the
    /// container (e.g. `AWS_PROFILE`, `AWS_REGION`, `EKS_CLUSTER`, secrets).
    /// Values travel through the runtime's process env, never argv.
    pub env: Vec<String>,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            image: "alpine:3.20".into(),
            memory: "1g".into(),
            cpus: "2".into(),
            pids: 256,
            network: None,
            idle_minutes: 30,
            env: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_parses_to_all_defaults() {
        // Every table is optional and defaults: an empty file is valid and
        // yields a text-only bot.
        let config = toml::from_str::<Config>("").expect("an empty config is valid");
        assert_eq!(config.agent.provider, "anthropic");
        assert_eq!(config.agent.tools, ToolMode::None, "tools default to text-only");
        assert_eq!(config.storage.dir, std::path::PathBuf::from("tapir-bot-data"));
    }

    #[test]
    fn unknown_top_level_tables_are_tolerated() {
        // A backend's tables (e.g. [reactions], [access]) share the same file,
        // so the neutral config must ignore them rather than reject the file.
        let config = toml::from_str::<Config>("[reactions]\nseen = \"eyes\"\n")
            .expect("a backend's table is ignored, not an error");
        assert_eq!(config.agent.provider, "anthropic");
    }

    #[test]
    fn the_agent_table_defaults_to_anthropic_when_absent() {
        let config = toml::from_str::<Config>("").unwrap();
        assert_eq!(config.agent.provider, "anthropic");
        assert!(config.agent.model.is_none());
        assert!(config.agent.system_prompt.is_none());
        assert_eq!(config.agent.tools, ToolMode::None, "tools default to text-only");
    }

    #[test]
    fn the_tool_mode_parses_each_value() {
        for (toml, expected) in [
            ("tools = \"none\"", ToolMode::None),
            ("tools = \"host\"", ToolMode::Host),
            ("tools = \"sandbox\"", ToolMode::Sandbox),
        ] {
            let src = format!("[agent]\n{toml}\n");
            let config = toml::from_str::<Config>(&src).unwrap();
            assert_eq!(config.agent.tools, expected, "{toml}");
        }
        let err = toml::from_str::<Config>("[agent]\ntools = \"docker\"\n")
            .expect_err("an unknown tool mode does not parse");
        assert!(format!("{err:#}").contains("tools"), "{err:#}");
    }

    #[test]
    fn the_agent_table_parses_and_defaults_per_field() {
        let config = toml::from_str::<Config>("[agent]\nmodel = \"claude-x\"\n").unwrap();
        assert_eq!(config.agent.provider, "anthropic", "provider falls back to the default");
        assert_eq!(config.agent.model.as_deref(), Some("claude-x"));

        let config = toml::from_str::<Config>(
            r#"
            [agent]
            provider = "openai"
            model = "gpt-x"
            system_prompt = "Be concise."
            "#,
        )
        .unwrap();
        assert_eq!(config.agent.provider, "openai");
        assert_eq!(config.agent.model.as_deref(), Some("gpt-x"));
        assert_eq!(config.agent.system_prompt.as_deref(), Some("Be concise."));
    }

    #[test]
    fn an_unknown_agent_key_is_a_clear_error() {
        let err = toml::from_str::<Config>("[agent]\nmodle = \"x\"\n")
            .expect_err("an unknown agent key does not parse");
        assert!(format!("{err:#}").contains("modle"), "{err:#}");
    }

    #[test]
    fn storage_defaults_to_the_data_dir() {
        let config = toml::from_str::<Config>("").unwrap();
        assert_eq!(config.storage.dir, std::path::PathBuf::from("tapir-bot-data"));
    }

    #[test]
    fn storage_dir_parses_and_an_unknown_key_errors() {
        let config = toml::from_str::<Config>("[storage]\ndir = \"/var/lib/tapir\"\n").unwrap();
        assert_eq!(config.storage.dir, std::path::PathBuf::from("/var/lib/tapir"));
        assert!(config.storage.memory_dir.is_none(), "memory_dir defaults to None");

        let err = toml::from_str::<Config>("[storage]\npath = \"x\"\n")
            .expect_err("unknown storage key does not parse");
        assert!(format!("{err:#}").contains("path"), "{err:#}");
    }

    #[test]
    fn memory_dir_parses_when_set() {
        let config = toml::from_str::<Config>("[storage]\nmemory_dir = \"/notes\"\n").unwrap();
        assert_eq!(config.storage.memory_dir, Some(std::path::PathBuf::from("/notes")));
    }

    #[test]
    fn the_sandbox_defaults_mirror_the_builtins() {
        let config = toml::from_str::<Config>("").unwrap();
        assert_eq!(config.sandbox.image, "alpine:3.20");
        assert_eq!(config.sandbox.pids, 256);
        assert_eq!(config.sandbox.idle_minutes, 30);
        assert!(config.sandbox.network.is_none());
    }

    #[test]
    fn the_sandbox_parses_and_an_unknown_key_errors() {
        let config = toml::from_str::<Config>(
            r#"
            [sandbox]
            image = "tapir-bot-sandbox"
            memory = "2g"
            network = "none"
            "#,
        )
        .unwrap();
        assert_eq!(config.sandbox.image, "tapir-bot-sandbox");
        assert_eq!(config.sandbox.memory, "2g");
        assert_eq!(config.sandbox.cpus, "2", "untouched field keeps its default");
        assert_eq!(config.sandbox.network.as_deref(), Some("none"));

        let err = toml::from_str::<Config>("[sandbox]\nimg = \"x\"\n")
            .expect_err("unknown sandbox key does not parse");
        assert!(format!("{err:#}").contains("img"), "{err:#}");
    }

    #[test]
    fn the_shipped_example_config_parses() {
        // The reference sample in the repo is real config, not prose: if a
        // neutral key changes, this fails before the docs can rot. (The Slack
        // tables in the example are exercised by tapir-bot-slack's own test.)
        let config = toml::from_str::<Config>(include_str!("../../config.example.toml"))
            .expect("config.example.toml stays valid");
        assert_eq!(config.agent.provider, "anthropic");
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(toml::from_str::<Config>("not toml at all [").is_err());
    }
}
