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

/// What a [`CommandHandler`] does with an invocation.
pub enum CommandOutcome {
    /// Post this markdown directly — no model turn (the default, cheap path).
    Reply(String),
    /// Run a model turn with this prompt, as if the user had typed it: the agent
    /// uses its generic skills/tools and streams its own reply. Use this when a
    /// command should delegate to the agent (e.g. read a thread and act on it)
    /// rather than answer deterministically.
    Prompt(String),
}

/// A consumer-registered `!`-command. The engine parses `!<name> [arg]`, finds
/// the handler whose [`CommandSpec::name`] matches, and either posts its
/// [`Reply`](CommandOutcome::Reply) or runs its [`Prompt`](CommandOutcome::Prompt)
/// as an agent turn. Register handlers on the bot builder.
#[async_trait::async_trait]
pub trait CommandHandler: Send + Sync {
    /// The command's identity and help metadata.
    fn spec(&self) -> CommandSpec;
    /// Handle one invocation: reply directly, or delegate to an agent turn.
    async fn run(&self, ctx: &CommandContext<'_>) -> anyhow::Result<CommandOutcome>;
}

/// Parse a `!`-invocation into `(name, arg)`: a message (mention already
/// stripped) whose trimmed text starts with `!`; the first word is the command
/// name and the rest (trimmed) is the argument (empty when none). `None` for
/// ordinary text (no `!` prefix) or a bare `!`. The engine dispatches the name
/// to a built-in, then a registered [`CommandHandler`], then "unknown".
pub fn parse_invocation(text: &str) -> Option<(String, String)> {
    let rest = text.trim().strip_prefix('!')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    if name.is_empty() {
        return None;
    }
    let arg = parts.next().unwrap_or("").trim();
    Some((name.to_string(), arg.to_string()))
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
    Builtin { name: "skills", command: "!skills", description: "the available skills" },
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
pub fn render_help(help: &crate::config::Help, registered: &[CommandSpec]) -> String {
    let mut out = String::from("**Commands:**");
    for builtin in BUILTINS {
        if help.hide.iter().any(|h| h == builtin.name) {
            continue;
        }
        out.push_str(&format!("\n- `{}` — {}", builtin.command, builtin.description));
    }
    for spec in registered {
        let command = match &spec.args {
            Some(args) => format!("!{} {args}", spec.name),
            None => format!("!{}", spec.name),
        };
        out.push_str(&format!("\n- `{command}` — {}", spec.description));
    }
    for row in &help.extra {
        out.push_str(&format!("\n- `{}` — {}", row.command, row.description));
    }
    out
}

/// The skills as a GFM table (`Skill | Description`), the name showing `<args>`
/// when the skill declares them. The Slack backend renders it as a Block Kit
/// table; the name is wrapped in backticks so an `<arg>` placeholder stays
/// literal.
pub fn render_skills(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "No skills available.".into();
    }
    let mut out = String::from("| Skill | Description |\n| --- | --- |");
    for skill in skills {
        let name = match &skill.args {
            Some(args) => format!("`{} {args}`", skill.name),
            None => format!("`{}`", skill.name),
        };
        let description = skill.description.as_deref().unwrap_or("");
        out.push_str(&format!("\n| {name} | {description} |"));
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
            async fn run(&self, ctx: &CommandContext<'_>) -> anyhow::Result<CommandOutcome> {
                Ok(CommandOutcome::Reply(format!("oi {}", ctx.arg)))
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
        match handler.run(&ctx).await.unwrap() {
            CommandOutcome::Reply(text) => assert_eq!(text, "oi ana"),
            CommandOutcome::Prompt(_) => panic!("expected a Reply"),
        }
        // Object-safe: registries hold `Arc<dyn CommandHandler>`.
        let _boxed: std::sync::Arc<dyn CommandHandler> = std::sync::Arc::new(Greet);
    }

    fn inv(text: &str) -> Option<(String, String)> {
        parse_invocation(text)
    }

    #[test]
    fn parse_invocation_splits_name_and_arg() {
        assert_eq!(inv("!providers"), Some(("providers".into(), String::new())));
        assert_eq!(inv("   !providers  "), Some(("providers".into(), String::new())));
        assert_eq!(
            inv("!model anthropic/claude-x"),
            Some(("model".into(), "anthropic/claude-x".into()))
        );
        // The name is the first word; the rest (trimmed) is the arg.
        assert_eq!(inv("!skills extra"), Some(("skills".into(), "extra".into())));
        // An unknown name still parses — the engine dispatches it (custom/unknown).
        assert_eq!(inv("!frobnicate"), Some(("frobnicate".into(), String::new())));
    }

    #[test]
    fn parse_invocation_is_none_for_ordinary_text() {
        assert_eq!(inv("hello there"), None);
        assert_eq!(inv("what about !providers"), None, "mid-prose is not a command");
        assert_eq!(inv("!"), None, "a bare bang is not a command");
        assert_eq!(inv("   "), None);
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
        let out = render_help(&crate::config::Help::default(), &[]);
        assert!(out.contains("!providers") && out.contains("!models") && out.contains("!help"));
        assert!(out.contains("!model") && out.contains("!forget") && out.contains("!skills"));
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
        let out = render_help(&help, &[]);
        // `!model` shares a prefix with `!models`, so match its unique row text.
        assert!(!out.contains("[provider/model]"), "hidden !model is gone: {out}");
        assert!(!out.contains("!forget"), "hidden !forget is gone: {out}");
        assert!(out.contains("!providers") && out.contains("!models"), "kept built-ins: {out}");
        assert!(out.contains("- `!version` — show the build version"), "extra row appended: {out}");
    }

    #[test]
    fn render_help_lists_registered_commands() {
        let registered = [
            CommandSpec { name: "version".into(), args: None, description: "build version".into() },
            CommandSpec {
                name: "deploy".into(),
                args: Some("<env>".into()),
                description: "deploy".into(),
            },
        ];
        let out = render_help(&crate::config::Help::default(), &registered);
        assert!(out.contains("- `!version` — build version"), "{out}");
        assert!(out.contains("- `!deploy <env>` — deploy"), "args shown: {out}");
    }

    #[test]
    fn render_skills_is_a_table_with_optional_args() {
        use crate::tools::Skill;
        let skills = [
            Skill {
                name: "shortcut".into(),
                description: Some("board".into()),
                args: Some("<comando>".into()),
            },
            Skill { name: "plain".into(), description: Some("does a thing".into()), args: None },
        ];
        let out = render_skills(&skills);
        assert!(out.starts_with("| Skill | Description |\n| --- | --- |"), "{out}");
        assert!(out.contains("| `shortcut <comando>` | board |"), "name with args: {out}");
        assert!(out.contains("| `plain` | does a thing |"), "name without args: {out}");
        assert_eq!(render_skills(&[]), "No skills available.");
    }
}
