//! The Slack backend's own config: the lifecycle reaction emojis and the access
//! allowlist. These are Slack-side concerns — reaction emoji are Slack short
//! names, and the access policy is keyed by Slack channel/user ids — so they
//! live with the backend, not in the neutral [`tapir_bot_core::Config`]. The
//! access *mechanism* (the type and the decision) is reused from core.

use serde::Deserialize;
use tapir_bot_core::Access;

/// The Slack backend config (the `[reactions]` and `[access]` tables). Shares
/// the same file as the neutral config; it ignores unknown top-level tables
/// (core's `[agent]`/`[storage]`/`[sandbox]`) so both can be deserialized from
/// the one file. Each table it owns denies unknown keys.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SlackConfig {
    /// The lifecycle reaction emojis.
    pub reactions: Reactions,
    /// Who may make the bot take a turn, where (deny-by-default).
    pub access: Access,
    /// The optional reply sent to a non-allowed user who addresses the bot.
    pub denial: Denial,
}

/// The access-denied reply. Optional: with no `message`, a denied user who
/// addresses the bot is met with silence (the default). With a `message`, that
/// styled text is sent to them — ephemerally, falling back to a DM — so they
/// learn why nothing happened.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Denial {
    /// Markdown sent to a denied user. `None` (the default) sends nothing.
    pub message: Option<String>,
}

/// The lifecycle reaction emojis (Slack short names, no colons). An empty
/// string disables that reaction.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reactions {
    /// Added while a turn is being handled. Default `eyes` (👀).
    pub seen: String,
    /// Added when the turn succeeds. Default `white_check_mark` (✅).
    pub done: String,
    /// Added when the turn fails. Default `x` (❌).
    pub failed: String,
}

impl Default for Reactions {
    fn default() -> Self {
        Self { seen: "eyes".into(), done: "white_check_mark".into(), failed: "x".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_default_to_the_built_in_emojis() {
        let config = toml::from_str::<SlackConfig>("").unwrap();
        assert_eq!(config.reactions.seen, "eyes");
        assert_eq!(config.reactions.done, "white_check_mark");
        assert_eq!(config.reactions.failed, "x");
    }

    #[test]
    fn reactions_parse_and_can_be_disabled_with_an_empty_string() {
        let config = toml::from_str::<SlackConfig>(
            r#"
            [reactions]
            seen = "wave"
            done = "tada"
            failed = ""
            "#,
        )
        .unwrap();
        assert_eq!(config.reactions.seen, "wave");
        assert_eq!(config.reactions.done, "tada");
        assert_eq!(config.reactions.failed, "", "empty disables that reaction");

        let err = toml::from_str::<SlackConfig>("[reactions]\nok = \"x\"\n")
            .expect_err("unknown reactions key does not parse");
        assert!(format!("{err:#}").contains("ok"), "{err:#}");
    }

    #[test]
    fn access_parses_from_the_access_table() {
        let config = toml::from_str::<SlackConfig>(
            r#"
            [access]
            allow_bots = true

            [access.dm]
            users = ["U1"]

            [access.channels.C0AAA]
            "#,
        )
        .unwrap();
        assert!(config.access.allow_bots);
        assert_eq!(config.access.dm.users, vec!["U1"]);
        assert!(config.access.channels.contains_key("C0AAA"));
    }

    #[test]
    fn denial_defaults_to_no_message() {
        let config = toml::from_str::<SlackConfig>("").unwrap();
        assert!(config.denial.message.is_none(), "silent by default");
    }

    #[test]
    fn denial_parses_a_message() {
        let config = toml::from_str::<SlackConfig>(
            "[denial]\nmessage = \"Desculpe, não falo com estranhos.\"\n",
        )
        .unwrap();
        assert_eq!(
            config.denial.message.as_deref(),
            Some("Desculpe, não falo com estranhos.")
        );

        let err = toml::from_str::<SlackConfig>("[denial]\nmsg = \"x\"\n")
            .expect_err("unknown denial key does not parse");
        assert!(format!("{err:#}").contains("msg"), "{err:#}");
    }

    #[test]
    fn unknown_top_level_tables_are_tolerated() {
        // The neutral [agent] table shares the file; SlackConfig ignores it.
        let config = toml::from_str::<SlackConfig>("[agent]\nprovider = \"openai\"\n")
            .expect("a neutral table is ignored, not an error");
        assert_eq!(config.reactions.seen, "eyes");
    }

    #[test]
    fn the_shipped_example_config_parses() {
        // The Slack half of the reference config: if a Slack key changes, this
        // fails before the docs can rot. (Neutral keys are core's test.)
        let config = toml::from_str::<SlackConfig>(include_str!("../../config.example.toml"))
            .expect("config.example.toml stays valid");
        assert_eq!(config.reactions.seen, "eyes");
    }
}
