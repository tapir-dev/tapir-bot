# tapir-bot

This is the main source code repository for `tapir-bot`, a library for building
chat bots on the [tapir](https://github.com/tapir-dev/tapir) agent SDK. It gives
you a backend-neutral bot engine — the turn lifecycle, conversation memory,
`!`-commands, access policy, and tool execution — and a Slack backend, and lets
you bring your own.

## Why tapir-bot?

- **Backend-neutral.** One engine drives any chat service; bring yours by
  implementing the `ChatBackend` trait. Slack ships today — Discord, IRC,
  Google Chat, and Teams are the same shape.
- **Config-driven.** A declarative `tapir-bot.toml` describes the bot: the
  neutral `[agent]`/`[storage]`/`[sandbox]` tables feed the engine, and a
  backend's own tables (Slack's `[reactions]`/`[access]`) feed the backend.
- **Composable.** Three layered crates — `tapir-bot-core`, `tapir-bot-slack`,
  and the `tapir-bot` facade — with dependencies pointing strictly downward, no
  cycles.

## Quick start

```toml
[dependencies]
tapir-bot = { git = "https://github.com/tapir-dev/tapir-bot" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
toml = "1"
```

```rust
use tapir_bot::{Bot, Config, slack::{SlackBackend, SlackConfig}};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // The binary owns config loading; the libraries only define the typed
    // structs. The neutral config (agent/storage/sandbox) feeds the engine; the
    // Slack config (reactions/access) feeds the backend — both from one file.
    let text = std::fs::read_to_string("tapir-bot.toml")?;
    let config: Config = toml::from_str(&text)?;
    let slack: SlackConfig = toml::from_str(&text)?;

    // SLACK_APP_TOKEN, SLACK_BOT_TOKEN, and the provider key (e.g.
    // ANTHROPIC_API_KEY) come from the environment.
    Bot::new(config).run(SlackBackend::from_env(slack)?).await
}
```

Swap `SlackBackend` for another `ChatBackend` to target a different service —
the engine is the same. See `SETUP.md` for the end-to-end Slack setup and
`SPEC.md` for the full scope.

## Layers

```
tapir-bot-core    backend-neutral engine: config, turn lifecycle, memory,
   │              !-commands, the reusable access policy, tool execution.
   │              Defines the ChatBackend / ReplySink extension points.
   │
   ├── tapir-bot-slack   the Slack backend: Socket Mode, Web API, Block Kit,
   │                     and the Slack-side config. Implements ChatBackend.
   │
   └── tapir-bot         the facade: a config-driven Bot builder you hand a
                         backend.
```

## Examples

The `tapir-bot/examples/` directory has a runnable reference bot:

- `slack_bot` — load a `tapir-bot.toml`, split it into the engine and Slack
  configs, and run over Socket Mode.

```sh
cargo run --example slack_bot                  # reads ./tapir-bot.toml
SLACK_APP_TOKEN=xapp-… SLACK_BOT_TOKEN=xoxb-… ANTHROPIC_API_KEY=sk-… \
  cargo run --example slack_bot
```

## Building from source

```sh
cargo build --workspace
cargo test --workspace
cargo doc -p tapir-bot --no-deps --open
```

Common tasks are wrapped in a [`justfile`](justfile) — run `just` to list them:

```sh
just check        # type-check the workspace
just test         # run the test suite
just deny         # lint the dependency graph (cargo-deny)
```

The bot links the agent SDK ([`../tapir`](https://github.com/tapir-dev/tapir))
by path, so that checkout must be present to build.

### Cargo features

- `slack` *(default)* — the Slack backend (`tapir_bot::slack`).
- `sandbox` — the container tool mode (`[agent].tools = "sandbox"`), backed by
  [`tapir-sandbox`](https://github.com/tapir-dev/tapir-sandbox). **Off by
  default**: the base bot is text-only/host and pulls no docker/podman stack.
  Enable it (and check out `../tapir-sandbox`) to use it:

  ```toml
  tapir-bot = { git = "...", features = ["sandbox"] }
  ```

  Without it, `[agent].tools = "sandbox"` fails at startup with a clear error.

Releasing is GitHub-only and also driven through `just`; see
[RELEASING.md](RELEASING.md).

## License

tapir-bot is free and open-source software licensed under the
[ISC License](LICENSE).

It builds on third-party crates whose licenses are listed in
[THIRD-PARTY-LICENSE](THIRD-PARTY-LICENSE).
