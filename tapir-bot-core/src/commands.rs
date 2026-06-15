//! Bot meta-commands: `!`-prefixed messages the bot answers directly, without
//! a model turn. The built-ins are parsed/rendered here (pure, tested);
//! consumers register their own via [`CommandHandler`].

use crate::event::Inbound;
use crate::tools::Skill;

/// Help/identity metadata for a command — drives the `!help` listing and the
/// dispatcher (a registered command matches on [`name`](CommandSpec::name)).
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The command name, without the leading `!` (e.g. `version`).
    pub name: String,
    /// An optional argument placeholder, shown as `!name <args>` in `!help`
    /// (e.g. `<provider/model>`). `None` for a no-argument command.
    pub args: Option<String>,
    /// One-line description for the `!help` listing.
    pub description: String,
}

/// What a [`CommandHandler`] sees for one invocation. `#[non_exhaustive]` so
/// fields can be added without breaking downstream handlers.
#[non_exhaustive]
pub struct CommandContext<'a> {
    /// The text after the command name, trimmed (empty when none).
    pub arg: &'a str,
    /// The message that triggered the command (user / channel / thread).
    pub inbound: &'a Inbound,
    /// The skills provisioned for the bot (for skill-aware commands).
    pub skills: &'a [Skill],
}

/// A consumer-registered `!`-command, answered directly (no model turn). The
/// engine parses `!<name> [arg]`, finds the handler whose
/// [`CommandSpec::name`] matches, and posts the markdown that
/// [`run`](CommandHandler::run) returns. Register handlers on the bot builder.
#[async_trait::async_trait]
pub trait CommandHandler: Send + Sync {
    /// The command's identity and help metadata.
    fn spec(&self) -> CommandSpec;
    /// Produce the reply markdown for one invocation.
    async fn run(&self, ctx: &CommandContext<'_>) -> anyhow::Result<String>;
}

/// A recognized command — the whole trimmed message (after the mention is
/// stripped) beginning with `!`.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `!providers` — the active providers (an API key is set).
    Providers,
    /// `!models` — `provider/model` for the active providers.
    Models,
    /// `!help` — the available commands.
    Help,
    /// `!model` (show the current model) or `!model <provider/model>` (set it
    /// for this conversation). Owner-only.
    Model(Option<String>),
    /// `!forget` — reset this conversation (history + model override).
    /// Owner-only.
    Forget,
    /// `!<name>` that isn't recognized.
    Unknown(String),
}

/// Parse `text` (the message, mention already stripped) as a command. A command
/// is the whole trimmed text starting with `!`, named by its first word.
/// Returns `None` for ordinary text (no `!` prefix, or a bare `!`).
pub fn parse(text: &str) -> Option<Command> {
    let rest = text.trim().strip_prefix('!')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match name {
        "" => None,
        "providers" => Some(Command::Providers),
        "models" => Some(Command::Models),
        "help" => Some(Command::Help),
        "model" => Some(Command::Model((!arg.is_empty()).then(|| arg.to_string()))),
        "forget" => Some(Command::Forget),
        other => Some(Command::Unknown(other.to_string())),
    }
}

// Renderers emit standard Markdown (`**bold**`, `-` bullets, `` `code` ``); the
// runtime turns it into Slack Block Kit via `render::to_blocks` before posting.

/// The active providers (those with an API key), one per line.
pub fn render_providers(active: &[&str]) -> String {
    if active.is_empty() {
        return "No active providers — set a provider API key (e.g. ANTHROPIC_API_KEY).".into();
    }
    let mut out = String::from("**Active providers:**\n");
    for id in active {
        out.push_str(&format!("- {id}\n"));
    }
    out.trim_end().to_string()
}

/// The available `provider/model` rows.
pub fn render_models(rows: &[(&str, &str)]) -> String {
    if rows.is_empty() {
        return "No models available — no active providers.".into();
    }
    let mut out = String::from("**Available models:**\n");
    for (provider, model) in rows {
        out.push_str(&format!("- {provider}/{model}\n"));
    }
    out.trim_end().to_string()
}

/// One built-in `!help` row: the hide key (the command's name), the command as
/// shown, and its description.
struct Builtin {
    name: &'static str,
    command: &'static str,
    description: &'static str,
}

/// The built-in commands, in listing order.
const BUILTINS: &[Builtin] = &[
    Builtin { name: "providers", command: "!providers", description: "active providers" },
    Builtin { name: "models", command: "!models", description: "available provider/model" },
    Builtin {
        name: "model",
        command: "!model [provider/model]",
        description: "show or set this conversation's model (creator only)",
    },
    Builtin { name: "forget", command: "!forget", description: "reset this conversation (creator only)" },
    Builtin { name: "help", command: "!help", description: "this list" },
];

/// The list of commands. Built-ins whose name is in `help.hide` are omitted, and
/// `help.extra` rows are appended — so a consumer can tailor the `!help` table
/// without the library knowing its commands.
pub fn render_help(help: &crate::config::Help) -> String {
    let mut out = String::from("**Commands:**");
    for builtin in BUILTINS {
        if help.hide.iter().any(|h| h == builtin.name) {
            continue;
        }
        out.push_str(&format!("\n- `{}` — {}", builtin.command, builtin.description));
    }
    for row in &help.extra {
        out.push_str(&format!("\n- `{}` — {}", row.command, row.description));
    }
    out
}

/// The reply for an unrecognized `!<name>`.
pub fn render_unknown(name: &str) -> String {
    format!("Unknown command: `!{name}`. Try `!help`.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_command_handler_exposes_its_spec_and_runs() {
        struct Greet;
        #[async_trait::async_trait]
        impl CommandHandler for Greet {
            fn spec(&self) -> CommandSpec {
                CommandSpec {
                    name: "greet".into(),
                    args: Some("<nome>".into()),
                    description: "diz oi".into(),
                }
            }
            async fn run(&self, ctx: &CommandContext<'_>) -> anyhow::Result<String> {
                Ok(format!("oi {}", ctx.arg))
            }
        }
        let handler = Greet;
        assert_eq!(handler.spec().name, "greet");
        assert_eq!(handler.spec().args.as_deref(), Some("<nome>"));
        let inbound = Inbound {
            channel: "C".into(),
            ts: "1".into(),
            thread: "1".into(),
            user: "U".into(),
            text: "!greet ana".into(),
            is_bot: false,
            is_dm: false,
            continuation: false,
        };
        let ctx = CommandContext { arg: "ana", inbound: &inbound, skills: &[] };
        assert_eq!(handler.run(&ctx).await.unwrap(), "oi ana");
        // Object-safe: registries hold `Arc<dyn CommandHandler>`.
        let _boxed: std::sync::Arc<dyn CommandHandler> = std::sync::Arc::new(Greet);
    }

    #[test]
    fn parse_recognizes_the_commands() {
        assert_eq!(parse("!providers"), Some(Command::Providers));
        assert_eq!(parse("!models"), Some(Command::Models));
        assert_eq!(parse("!help"), Some(Command::Help));
    }

    #[test]
    fn parse_trims_and_takes_the_first_word() {
        assert_eq!(parse("   !providers  "), Some(Command::Providers));
        assert_eq!(parse("!models please"), Some(Command::Models), "extra args ignored");
    }

    #[test]
    fn parse_returns_unknown_for_an_unrecognized_bang() {
        assert_eq!(parse("!frobnicate"), Some(Command::Unknown("frobnicate".into())));
    }

    #[test]
    fn parse_captures_the_model_argument_and_forget() {
        assert_eq!(parse("!model"), Some(Command::Model(None)));
        assert_eq!(
            parse("!model anthropic/claude-x"),
            Some(Command::Model(Some("anthropic/claude-x".into())))
        );
        assert_eq!(parse("!forget"), Some(Command::Forget));
    }

    #[test]
    fn parse_is_none_for_ordinary_text() {
        assert_eq!(parse("hello there"), None);
        assert_eq!(parse("what about !providers"), None, "mid-prose is not a command");
        assert_eq!(parse("!"), None, "a bare bang is not a command");
        assert_eq!(parse("   "), None);
    }

    #[test]
    fn render_providers_lists_or_explains_empty() {
        let out = render_providers(&["anthropic", "openai"]);
        assert!(out.contains("anthropic") && out.contains("openai"), "{out}");
        assert!(out.contains("- anthropic"), "standard-markdown bullets: {out}");
        assert!(out.starts_with("**Active providers:**"), "standard-markdown bold: {out}");
        assert!(render_providers(&[]).contains("No active providers"));
    }

    #[test]
    fn render_models_lists_provider_slash_model_or_explains_empty() {
        let out = render_models(&[("anthropic", "claude-opus-4-8")]);
        assert!(out.contains("- anthropic/claude-opus-4-8"), "{out}");
        assert!(render_models(&[]).contains("No models"));
    }

    #[test]
    fn render_help_lists_the_builtins_by_default() {
        let out = render_help(&crate::config::Help::default());
        assert!(out.contains("!providers") && out.contains("!models") && out.contains("!help"));
        assert!(out.contains("!model") && out.contains("!forget"));
        assert!(out.contains("- `!providers`"), "standard-markdown list + inline code: {out}");
    }

    #[test]
    fn render_help_hides_built_ins_and_appends_extra_rows() {
        let help = crate::config::Help {
            hide: vec!["model".into(), "forget".into()],
            extra: vec![crate::config::HelpRow {
                command: "!version".into(),
                description: "show the build version".into(),
            }],
        };
        let out = render_help(&help);
        // `!model` shares a prefix with `!models`, so match its unique row text.
        assert!(!out.contains("[provider/model]"), "hidden !model is gone: {out}");
        assert!(!out.contains("!forget"), "hidden !forget is gone: {out}");
        assert!(out.contains("!providers") && out.contains("!models"), "kept built-ins: {out}");
        assert!(out.contains("- `!version` — show the build version"), "extra row appended: {out}");
    }
}
