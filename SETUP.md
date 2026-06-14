# Setting up the Slack app and running a bot

tapir-bot is a library; a bot is a thin binary over it. This guide takes you
from nothing to a running bot: create the Slack app (configured for Socket
Mode), install it and mint the tokens, write the config, and run.

## 1. Create the Slack app

tapir-bot no longer generates a manifest — you configure the app on the Slack
side. The quickest path is *Create from manifest* with the JSON below; it sets
up a Socket Mode bot with every capability the runtime supports. Trim the
scopes/events you don't want before creating.

1. Go to <https://api.slack.com/apps> and click **Create New App**.
2. Choose **From a manifest**, pick the workspace, then **Next**.
3. Select the **JSON** tab, paste the manifest below (edit `name`/`description`),
   and click **Next**, then **Create**.

```json
{
  "display_information": {
    "name": "Tapir Bot",
    "description": "A Slack assistant that runs over Socket Mode."
  },
  "features": {
    "bot_user": { "display_name": "Tapir Bot", "always_online": true },
    "app_home": {
      "messages_tab_enabled": true,
      "messages_tab_read_only_enabled": false
    }
  },
  "oauth_config": {
    "scopes": {
      "bot": [
        "app_mentions:read",
        "chat:write",
        "reactions:read",
        "reactions:write",
        "im:history",
        "channels:history",
        "groups:history"
      ]
    }
  },
  "settings": {
    "event_subscriptions": {
      "bot_events": [
        "app_mention",
        "message.im",
        "message.channels",
        "message.groups"
      ]
    },
    "socket_mode_enabled": true,
    "org_deploy_enabled": false,
    "interactivity": { "is_enabled": false }
  }
}
```

What each capability is for (drop the ones you don't need):

- `app_mentions:read` + `chat:write` + `app_mention` — answer `@mentions`. The
  baseline; keep these.
- `reactions:read` + `reactions:write` — the 👀/✅/❌ lifecycle reactions.
- `im:history` + `message.im` + the `app_home` Messages tab — direct messages.
- `channels:history` + `groups:history` + `message.channels`/`message.groups` —
  follow channel threads without a re-mention (the bot then receives **every**
  message in its channels).

Prefer the dashboard? Create an app **From scratch**, then set the same bot
scopes under **OAuth & Permissions**, the same bot events under **Event
Subscriptions**, enable **Socket Mode**, and turn on the **Messages** tab under
**App Home** (for DMs).

The app icon has no manifest field — set it by hand under **Basic Information →
Display Information → App icon**.

## 2. Install the app and get the tokens

1. Open **OAuth & Permissions** and click **Install to Workspace**, then
   **Allow**. Copy the **Bot User OAuth Token** (`xoxb-…`).
2. Open **Basic Information → App-Level Tokens**, click **Generate Token and
   Scopes**, add the `connections:write` scope, and generate it. Copy the
   **App-Level Token** (`xapp-…`). Socket Mode connects with this one.

Keep both tokens safe; the bot reads them from the environment. If you later
change scopes (e.g. to add DMs or channel threads), **reinstall** the app from
**OAuth & Permissions** for the new scopes to take effect.

## 3. Configure the bot

Copy the reference config and edit it:

```sh
cp config.example.toml tapir-bot.toml
$EDITOR tapir-bot.toml
```

Every table is optional and defaults — an empty file is a valid, text-only,
deny-everything bot. Unknown keys are errors, so a typo fails loudly. The tables
below cover what you'll usually set.

### Model settings (`[agent]`)

The `[agent]` table selects the model (the API key always comes from the
environment, never the file):

```toml
[agent]
provider = "anthropic"          # tapir provider id
# model = "claude-..."          # omit → the catalog's default model
system_prompt = "You are Tapir, a concise Slack assistant."
tools = "none"                  # none | host | sandbox (see below)
```

### Access control (`[access]`)

The bot is **deny-by-default**: with no `[access]` table it answers nobody (and
warns at startup). List what may trigger a turn:

```toml
[access]
allow_bots = false        # let bots trigger turns (off by default)
bot_turn_limit = 3        # cap bot-triggered turns per thread (anti-loop)

[access.dm]
users = ["U0ABC123"]      # user ids allowed to DM the bot (not the D… DM id)

[access.channels.C0XYZ789]   # a channel the bot serves; everyone may mention
[access.channels.C0OTHER]
users = ["U0ABC123"]         # optional: only these users may trigger here
```

Channel and user ids are the stable `C…`/`U…` ids (in Slack, *Copy link* or a
member's profile → *Copy member ID*). Bots are allowed only when `allow_bots`
is true, and then capped per thread so two bots can't loop forever; the bot
never answers itself.

DMs need the `im:history` scope + `message.im` event (step 1); channel-thread
continuation needs the `channels:history`/`groups:history` scopes +
`message.channels`/`message.groups` events. Reinstall the app after granting
any new scope.

### Memory (`[storage]`)

The bot remembers each conversation. History persists on disk under
`[storage].dir` (default `tapir-bot-data`), keyed per channel thread and per DM,
and is replayed into each turn. The data dir holds conversation content — it is
gitignored; place it appropriately for your deployment.

Durable facts go in editable markdown, read fresh each turn (no restart):

- `<memory_dir>/MEMORY.md` — facts for every conversation.
- `<memory_dir>/memory/<channel_id>.md` — facts for one channel.

`<memory_dir>` is `[storage].dir` by default; set `[storage].memory_dir` to
keep the facts somewhere else (e.g. an existing notes directory):

```toml
[storage]
dir = "tapir-bot-data"
memory_dir = "/home/me/notes/tapir"
```

### Reaction emojis (`[reactions]`)

The lifecycle reactions are configurable (Slack short names, no colons); set any
to an empty string to disable it:

```toml
[reactions]
seen = "eyes"               # while the turn is handled (default 👀)
done = "white_check_mark"   # on success (default ✅)
failed = "x"                # on failure (default ❌)
```

### Tools (`[agent].tools` and `[sandbox]`)

`[agent].tools` gates tool use:

- `none` — text-only, no tools run anywhere (the default).
- `host` — tools run in the bot's **own process** at a per-channel workspace.
  Only sound when the bot itself runs in a hardened container (e.g. a Kubernetes
  pod, the intended deployment).
- `sandbox` — tools run in an **isolated per-channel container** (needs docker
  or podman), configured by the `[sandbox]` table. Build the repo `Dockerfile`
  (`docker build -t tapir-bot-sandbox .`) for an image with the CLIs and set
  `[sandbox].image` to it. This mode is behind the off-by-default **`sandbox`
  cargo feature** — build the bot with `--features sandbox` (or
  `features = ["sandbox"]` on the dependency), or it fails at startup with a
  clear error.

In `host`/`sandbox` the agent gains tools and can use **skills** (`skills/`,
`SKILL.md` + scripts) provisioned into its per-channel workspace. See
`docs/plan_sandbox_tools.md` for the roadmap.

## 4. Run the bot

A bot is a thin binary that loads the config and runs a backend. A minimal one:

```rust
use tapir_bot::{Bot, Config, slack::{SlackBackend, SlackConfig}};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // The binary owns config loading; the libraries only define the typed
    // structs. The neutral config (agent/storage/sandbox) feeds the engine; the
    // Slack config (reactions/access) feeds the backend.
    let text = std::fs::read_to_string("tapir-bot.toml")?;
    let config: Config = toml::from_str(&text)?;
    let slack: SlackConfig = toml::from_str(&text)?;
    Bot::new(config).run(SlackBackend::from_env(slack)?).await
}
```

A ready-to-run version of this is `tapir-bot/examples/slack_bot.rs`
(`cargo run --example slack_bot`).

`SlackBackend::from_env()` reads `SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN`. To target
another chat service later, swap it for that service's `ChatBackend` — the
config and engine are unchanged.

Export the three secrets (here from `pass`) and run:

```sh
export SLACK_BOT_TOKEN="$(pass tapir/slack/dev/bot-token)"   # xoxb-…
export SLACK_APP_TOKEN="$(pass tapir/slack/dev/app-token)"   # xapp-…
export ANTHROPIC_API_KEY="$(pass tapir/anthropic/api)"       # the provider key
cargo run
```

`SLACK_APP_TOKEN` opens the Socket Mode connection; `SLACK_BOT_TOKEN` speaks the
Web API; `ANTHROPIC_API_KEY` is the default provider's key (the provider/model
come from the `[agent]` table). Then invite the bot to a channel
(`/invite @Tapir Dev`) and mention it (`@Tapir Dev what can you do?`): it reacts
👀, streams the model's answer in-thread, then reacts ✅ (or ❌ with a short note
if the turn fails). `Ctrl-C` stops it. Set `RUST_LOG=debug` for more detail.
