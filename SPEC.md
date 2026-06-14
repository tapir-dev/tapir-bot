# tapir-bot — Specification

tapir-bot is a **library** for building chat bots on the local
[`tapir`](../tapir) agent SDK. It is Layer B in the project's layering (see
`docs/plan_tapir_layering.md`): the reusable chat-bot framework — turn
lifecycle, reactions, access policy, storage, and tool execution — with no
opinion about any specific bot or chat service. A concrete bot is a thin binary
(Layer C) that loads a `Config` and calls `Bot::new(config).run(backend)`.

Mirroring tapir's own layering (`tapir-core` + `tapir-ai` + `tapir`), it is a
workspace of three crates:

- **`tapir-bot-core`** — the backend-neutral engine. Owns config, the turn
  lifecycle, conversation memory, the `!`-commands, the access policy, and tool
  execution. Defines the `ChatBackend` / `ReplySink` extension points.
- **`tapir-bot-slack`** — the Slack backend (Socket Mode + Web API + Block
  Kit). Implements `ChatBackend`. Future siblings: `-discord`, `-irc`,
  `-gchat`, `-teams`.
- **`tapir-bot`** — the facade: a config-driven `Bot` builder.

It is written in thin, verifiable slices and grows from here.

The backend **gives the loop**: the core exposes an `Engine` that processes one
event at a time, and each backend owns its own loop (ack/reconnect for a socket,
HTTP serving for a webhook backend), calling the engine per event. This avoids
binding every backend to one ack/reconnect model.

## 1. Objective

Give a bot author a single declarative config file and a small set of
extension points, so a working Slack assistant is a thin binary over this
library rather than a fork. The runtime targets a **Socket Mode** bot whose
behavior is: respond to `@mentions` (and DMs/threads), run a model turn through
the `tapir` SDK, and manage lifecycle reactions. No public URL is required.

Target users: bot authors self-hosting tapir-bot, comfortable with a terminal,
a TOML file, and a few lines of Rust glue.

Self-construct (self-installed tools, authored skills, change proposals) is the
**binary's** policy, not the library's — the library exposes the extension
points but never exercises them itself.

## 2. Public API

A bot binary wires the facade to a backend:

```rust
use tapir_bot::{Bot, Config, slack::{SlackBackend, SlackConfig}};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    // Loading config is the binary's job; the libraries only define the structs.
    // The neutral config and the Slack config come from the same file.
    let text = std::fs::read_to_string("tapir-bot.toml")?;
    let config: Config = toml::from_str(&text)?;
    let slack: SlackConfig = toml::from_str(&text)?;
    Bot::new(config).run(SlackBackend::from_env(slack)?).await
}
```

The surface the crates expose:

- `tapir_bot::Bot` — the facade. `Bot::new(config)` then `.run(backend)`: builds
  the engine (resolving the model and validating the provider key up front) and
  drives the backend until the process is stopped. `.skills_dir(p)` overrides
  the `skills/` tree; `.no_skills()` disables it.
- `tapir_bot_core::Config` — the **neutral** schema (`agent`, `storage`,
  `sandbox`), a plain `Deserialize` struct. Backend-specific config lives with
  its backend. The library does no I/O and picks no format: the binary obtains
  it and deserializes. Every table is optional and defaults.
- `tapir_bot_core::{access::Access, LoopGuard}` — the reusable access mechanism.
  `Access` is a generic allowlist (opaque channel/user ids) a backend embeds in
  its own config; `allows()` is the pure decision; `LoopGuard` the bot-loop cap.
- `tapir_bot_core::Engine` — processes one admitted message: `should_handle(&access,
  &mut loop_guard, bot_id, inbound)` (the decision; the backend passes its access
  config + loop guard) then `handle` (command or streamed turn).
- `tapir_bot_core::{ChatBackend, ReplySink}` — the extension points. A backend
  implements `ChatBackend::run(self, engine)` (owns the loop) and a `ReplySink`
  (`update(text, done)`) that the engine drives with the accumulating reply.
- `tapir_bot_core::{event::Inbound, Tools, tools::{HostTools, build_sandbox_manager}}`
  — the neutral event and the two non-text tool backends.
- `tapir_bot_slack::{SlackBackend, SlackConfig}` — `SlackConfig` is the
  Slack-side config (`reactions` + `access`); `SlackBackend::from_env(config)`
  (or `::new(app, bot, config)`) implements `ChatBackend` over Socket Mode.

Developer workflow:

- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --all-targets`

## 3. Project structure

A Cargo workspace of three crates that path-depend on the sibling `tapir` SDK
and `tapir-sandbox`.

```
Cargo.toml              # [workspace]: members, shared deps, profiles
config.example.toml     # documented reference config
SETUP.md README.md SPEC.md
tapir-bot-core/
  src/
    lib.rs              # crate root: re-exports
    config.rs           # neutral schema: agent/storage/sandbox (Deserialize)
    access.rs           # reusable Access policy + allows() + LoopGuard
    event.rs            # the neutral Inbound event
    engine.rs           # Engine: should_handle, handle, the turn, commands
    backend.rs          # the ChatBackend / ReplySink traits
    tools.rs            # Tools/HostTools/build_sandbox_manager + skills
    memory.rs           # transcript + MEMORY.md facts assembly
    commands.rs         # the `!`-prefixed meta-commands
    meta.rs             # per-conversation state (owner, model override)
tapir-bot-slack/
  src/
    lib.rs              # SlackBackend (the loop) + SlackReply (ReplySink)
    config.rs           # SlackConfig: reactions + access (Slack-side)
    protocol.rs         # pure frame handling (handle_frame), unit-tested
    client.rs           # Web API client + the WebSocket read loop
    render.rs           # CommonMark/GFM → Slack Block Kit
tapir-bot/
  src/
    lib.rs              # the Bot facade/builder
```

Dependencies (each justified in the Cargo.toml comments):

- `anyhow` — error context.
- `serde` (derive) — config structs.
- `serde_json` — Slack Web API payloads and Block Kit rendering.
- `toml` — config parsing.
- `reqwest` (rustls, async) — the Slack Web API client.
- `tokio` + `tokio-tungstenite` + `futures` — the async Socket Mode loop.
- `tracing` — runtime logging (the subscriber is the binary's concern).
- `pulldown-cmark` — Markdown parsing for the Block Kit renderer.
- `tapir` (path `../tapir/tapir`) — the agent SDK.
- `tapir-sandbox` (path) + `libc` — the per-channel container backend and the
  host uid/gid for its file ownership. **Optional**, behind the off-by-default
  `sandbox` feature; the base bot (text-only/host) pulls neither.

### Config schema

`tapir-bot.toml`, parsed with `deny_unknown_fields` so a typo fails loudly.
Every table is optional and defaults — an empty file is a valid, text-only,
deny-everything bot.

```toml
[agent]
provider = "anthropic"           # tapir provider id (default)
# model = "claude-..."           # omit → the catalog's default model
# system_prompt = "..."          # optional standing prompt
tools = "none"                   # none | host | sandbox

[access]                         # deny-by-default; see §10
allow_bots = false
bot_turn_limit = 3
[access.dm]
users = []
# [access.channels.C0XYZ789]

[storage]                        # see §11
dir = "tapir-bot-data"
# memory_dir = "/path/to/notes"

[reactions]                      # lifecycle emojis; empty disables one
seen = "eyes"
done = "white_check_mark"
failed = "x"

[sandbox]                        # read only when [agent].tools = "sandbox"; §15
image = "alpine:3.20"
memory = "1g"
cpus = "2"
pids = 256
idle_minutes = 30
# network = "none"
# env = ["AWS_PROFILE", "AWS_REGION"]
```

The Slack app's identity (name, description, icon) and its scopes/events are
configured on the Slack side (the app's manifest in the dashboard, or
*Create from manifest*), not in this file — see SETUP.md. The library does not
generate or push that manifest.

## 4. Code style

- Rust 2024 edition. `anyhow::Result` at boundaries with `.context(...)` that
  names the file or operation, matching the existing config.rs style.
- `#[serde(deny_unknown_fields)]` on every config struct.
- Module-level `//!` doc comments explaining intent, and `///` on public
  items — keep the density of the current config.rs.
- Errors that involve a file name the file in their context.
- Markdown (SPEC, SETUP, README): one blank line after every heading;
  paragraphs and list items wrapped at 80 columns; code blocks/tables/URLs
  may exceed when unbreakable.
- Commits: English, imperative mood, no `feat:`/`fix:` prefixes, no
  Co-Authored-By trailers.

## 5. Testing strategy

`cargo test`, unit tests colocated with modules (as config.rs does today):

- Config: an empty file parses to all-defaults; an unknown key fails loudly and
  the message names the key; each table parses and an unknown key under it
  errors; `config.example.toml` parses (`include_str!`, so docs can't rot).
- Protocol/policy/memory/commands/render: pure functions unit-tested in place
  (`handle_frame`, `strip_mention`, `allows`, the `LoopGuard`,
  `conversation_id`, `assemble_prompt`, the command parsing, every Block Kit
  block type and fallback path).
- The live WebSocket/Web API/container paths are **manual checkpoints** — they
  cannot run in CI.

No network or container runtime in the default test suite.

## 6. Boundaries

Always:

- Keep the dependency set minimal; justify any addition in the Cargo.toml
  comment, as the repo already does.
- Fail loudly on bad config (unknown keys) with a message that names the file
  and the problem.
- Keep the library opinion-free: the framework exposes mechanisms, the bot
  binary supplies policy.
- Keep SETUP.md in English and in sync with the runtime and the scopes it needs.

Ask first:

- Before adding any new runtime dependency.
- Before changing the config schema shape.
- Before giving the agent write access to its own memory — deferred. The access
  policy (§10), conversation memory (§11), and tools (§15) are in. Tools are
  gated by `[agent].tools`: `none` (text-only, the default), `host` (tools run
  in the bot's own process — only sound when the bot itself runs in a hardened
  container, e.g. a Kubernetes pod), or `sandbox` (per-channel container).

Never:

- Hardcode or log Slack tokens; read `SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN` from
  the environment and never write them to disk or print a token back.
- Commit `CLAUDE.md`, `docs/plan_*.md`, or local-only files; stage files by
  explicit name, never `git add -A`.

## 8. Runtime milestone — Socket Mode echo

The foundational runtime capability: the bot connects and answers. Scope:

- **Behavior:** echo — on an `@mention`, react 👀, post the message text back
  in-thread, then react ✅. No LLM yet.
- **Transport:** Socket Mode over `tokio` + `tokio-tungstenite`. Tokens read
  from the environment (`SLACK_APP_TOKEN` = `xapp-…` opens the connection;
  `SLACK_BOT_TOKEN` = `xoxb-…` speaks the Web API).
- **Access:** any mention in any channel the bot is in. No allowlist yet.
- **Entry point:** `slack::run`.

Structure: a pure protocol (`slack/protocol.rs::handle_frame`, fully
unit-tested) over async I/O (`slack/client.rs` Web API + the WebSocket read loop
in `slack/mod.rs`, verified manually). The loop acks each envelope before the
slower reply (Slack redelivers otherwise), reconnects on Slack's periodic
disconnect or a dropped socket, and shuts down cleanly on Ctrl-C.

Testing: `handle_frame`, `strip_mention`, `check_ok`, and the env-token
resolution are unit-tested; the live WebSocket/Web API path is a manual
checkpoint, since it cannot run in CI.

## 9. Agent-turn milestone — real model reply

Replaces the echo with a real turn through the local `tapir` agent SDK (a path
dependency on `../tapir`).

- **Provider/model:** from the `[agent]` config table — `provider` (default
  `anthropic`), optional `model` (else the tapir catalog's default), optional
  `system_prompt`. The provider API key is read from the environment
  (`ANTHROPIC_API_KEY` for anthropic), validated at startup; never the config.
- **Tools:** none. The turn runs text-only (`only_tools([])`) so the model
  cannot execute anything on the host.
- **Memory:** stateless — each mention is an isolated turn.
- **Concurrency:** each mention's turn runs in a spawned task, so the read
  loop keeps polling the socket (answering Slack's pings) while the model
  thinks. The turn accumulates the streamed text and posts one in-thread
  reply; failures react ❌ with a short note.

Testing: model resolution and the provider-key check are unit-tested; the live
turn is a manual checkpoint.

## 10. Access-control milestone — who may talk to the bot

A configurable `[access]` policy, **deny-by-default**: nothing triggers a turn
until it is listed.

- **Channels:** an allowlist (`[access.channels.<id>]`). Everyone in a listed
  channel may mention the bot; an optional per-channel `users` restricts it.
- **DMs:** a user allowlist (`[access.dm].users`) — only listed users may DM.
  Requires the `im:history` scope and the `message.im` event on the Slack app,
  so the app must be reinstalled after granting them.
- **Bots:** `allow_bots` (default off) lets bot-authored messages trigger
  turns, capped by `bot_turn_limit` bot-triggered turns **per thread** to break
  bot-to-bot loops. The bot never answers itself.

Shape: the protocol carries `is_bot`/`is_dm` and delivers DM messages; the pure
`slack::policy::allows(access, inbound)` makes the deny-by-default decision; the
read loop's `admit` applies, in order, the self-guard (`user == bot_user_id`),
the policy, then the stateful `LoopGuard` (per-thread bot cap). An empty policy
is warned at startup.

Testing: `allows`, the `LoopGuard`, and the config parsing are unit-tested; the
live channel/DM/bot gating is a manual checkpoint.

Deferred: per-channel `allow_bots`/limit, group DMs (`mpim`), and a denylist
mode.

## 11. Memory milestone — the bot remembers

Two layers, both keyed per conversation (a channel thread, or a whole DM):

- **Transcript memory.** Each turn `resume`s the conversation from a
  `FileStore` under `<data_dir>/sessions` (replaying prior history, with
  tapir's auto-compaction), runs, then appends the user and assistant messages.
  Keying: `dm:<channel>` for DMs (one rolling conversation), `ch:<channel>:
  <thread>` for channels (one per thread).
- **MEMORY.md facts.** A global `<data_dir>/MEMORY.md` and a per-channel
  `<data_dir>/memory/<channel>.md` are read fresh each turn and assembled with
  the persona into the system prompt (under a `# Memory` heading), so edits
  apply without a restart.

`[storage].dir` (default `tapir-bot-data`) sets the data dir; it holds
conversation content and is gitignored. `[storage].memory_dir` overrides where
the MEMORY.md facts live (default: the data dir). The lifecycle reaction emojis
are configurable under `[reactions]` (`seen`/`done`/`failed`; empty disables
one). Persistence is best-effort — a storage hiccup logs a warning rather than
failing a delivered reply.

Testing: `conversation_id` and `assemble_prompt` are unit-tested; the live
resume/append and facts injection are a manual checkpoint.

Deferred: agent-writable memory (a `remember` tool, once tools land),
per-thread (not just per-channel) facts, and a conversation-reset command.

## 12. Thread-continuity milestone — follow threads without a re-mention

In a channel, once the bot is mentioned and a thread opens, replies in that
thread continue the conversation **without** another `@mention` (DMs already
flow this way). Additive: `app_mention` still triggers; this adds silent
continuation.

To hear thread replies the bot subscribes to channel messages
(`channels:history`/`groups:history` scopes + `message.channels`/
`message.groups` events on the Slack app), so it **receives every message** in
its channels (a volume/privacy implication). The protocol delivers only thread
replies (a `message` with a `thread_ts`, no subtype); the read loop's
`should_handle` acts on a continuation only when it passes the access policy,
is not the bot itself, does **not** mention the bot (the `app_mention` path
handles those, deduping the echo), and is in a thread the bot already has saved
history for (`store.load("ch:<channel>:<thread>")`). Root non-mentions and
unknown threads are dropped.

Granting these scopes changes the app — reinstall after.

Testing: the protocol delivery and `mentions_bot` are unit-tested; the live
continuation is a manual checkpoint.

Deferred: an in-memory known-thread cache (avoid a `store.load` per candidate),
group DMs (`mpim`), and a way to make the bot stop following a thread.

## 13. Streaming milestone — the reply appears as it's written

The reply streams into a single Slack message instead of arriving all at once:
the bot posts the message on the first text and edits it (`chat.update`) as
more arrives, throttled to ~1s with a trailing cursor, then a final edit drops
the cursor. Purely runtime — it uses `chat:write` (already granted), so no
scope change or reinstall.

`post_message` returns the message `ts` so it can be edited; `update_message`
performs the edit. Editing stays under Slack's `chat.update` rate limit via the
throttle. An empty turn still posts a placeholder; a mid-stream error leaves
the partial text and the usual failure note.

Deferred: making the throttle/cursor configurable, and a non-streaming mode.

## 14. Commands milestone — bot meta-commands

`!`-prefixed messages the bot answers directly, without a model turn. A command
is the whole trimmed message (after the mention is stripped) beginning with `!`,
named by its first word — `!providers` mid-prose is ordinary text. Commands are
intercepted in `answer` before the turn: they post a reply and do **not** run a
turn or write to memory; they still pass through the access policy and the
👀→✅ lifecycle.

The commands:

- `!providers` — the active providers (an API key is set in the environment).
- `!models` — `provider/model` for the active providers.
- `!help` — the commands. An unrecognized `!foo` points to `!help`.
- `!model [provider/model]` — show or set this conversation's model
  (catalog-validated; used by later turns). **Creator-only.**
- `!forget` — reset this conversation (history + model override).
  **Creator-only.**

Read-only commands (`!providers`/`!models`/`!help`) are open to anyone the
access policy admits. The **session commands** (`!model`, `!forget`) mutate the
conversation, so they're restricted to its **creator** — the user recorded as
`owner` on the first turn; another user gets a denial naming the owner.

Per-conversation state — the owner and the model override — lives in
`<data_dir>/meta/<id>.toml`, beside the transcripts; `!forget` deletes the
transcript and the meta file. Parsing/formatting are pure
(`slack/commands.rs`, unit-tested); runtime/env/store gathering is in the read
loop. The set is extensible by a match arm.

## 15. Sandbox-tools milestone — the agent runs tools (in progress)

The agent gains real tools. How they execute is gated by `[agent].tools`:

- `none` (default) — text-only; no tools run anywhere.
- `host` — tools run in the bot's **own process** at a per-channel workspace
  under `<data_dir>/workspaces/<channel>`, using the runtime's default local
  exec/fs ops. Turns in a channel are serialized by a per-channel lock
  (mirroring the sandbox lease); skills are provisioned into the workspace the
  same way the sandbox does. No container is spun and no path boundary is
  installed — exec runs with the bot's own privileges, so host mode is only
  sound when the bot itself runs in a hardened container (e.g. a **Kubernetes
  pod**, the intended deployment). Credentials/env come from the pod (service
  account, mounted secrets, `~/.aws`), not from `[sandbox].env`/`<data_dir>/aws`.
- `sandbox` — tools run in an **isolated per-channel container**
  (`tapir-sandbox`: docker/podman, cap-drop, `no-new-privileges`, mem/cpu/pids
  limits), configured by `[sandbox]`. Compiled only with the off-by-default
  **`sandbox` cargo feature**; without it, selecting this mode errors at startup
  (the base bot pulls no docker/podman stack).

Shipped (phases A–B):

- **Per-channel sandbox.** When `tools = "sandbox"`, a `SandboxManager` builds
  one container per channel rooted at `<data_dir>/sandboxes/<channel>/workspace`
  (the only persistent path), reaped on idle. A sandboxed turn runs at
  `/workspace` with tools on, pointing the agent's exec/fs/boundary at the
  channel's lease (busy-guarded for the turn). The workspace path is
  canonicalized (docker rejects a relative bind-mount source). The container
  runs as the **host uid:gid** (`SandboxConfig.user`), so files it writes under
  the workspace are host-owned (not root) and the bot can re-seed/read them —
  it also runs non-root.
- **Skills.** A versioned repo `skills/` (each = `SKILL.md` + scripts), plus
  per-channel `<data_dir>/skills/<channel>` overrides, is provisioned into
  `<workspace>/skills` on sandbox creation. The prompt tells the agent skills
  live there; it discovers them with its own `ls`/`read` and runs the scripts
  with bash (no host-path coupling to tapir's skills loader).
- Image: a repo `Dockerfile` (`docker build -t tapir-bot-sandbox .`); set
  `[sandbox].image` to it. The base ships bash/jq/git/curl; CLIs come with the
  AWS scenario.

Next (phases C–E, not yet built): AWS SSO via a URL relayed to Slack (session
persisted under `/workspace/.aws`) enabling the kubectl scenario; an approval
hook for destructive ops; terraform/terragrunt + PRs. The full roadmap is in
`docs/plan_sandbox_tools.md`.

Testing: config parsing and skill provisioning are unit-tested; the live
container path is a manual checkpoint (needs a runtime).

## 16. Rich-output milestone — Block Kit formatting

The model emits standard CommonMark/GFM, but Slack's `text` field only speaks
Slack *mrkdwn* — so `**bold**`, `# headings`, `[links](url)`, lists, code, and
tables would render as literal characters or the wrong style. A pure render
module (`src/slack/render.rs`) parses the Markdown with `pulldown-cmark` and
maps each top-level block to a Block Kit block:

- heading → `header` (plain_text, ≤150 chars, truncated)
- paragraph → `rich_text` › `rich_text_section`
- list (nested) → `rich_text` › `rich_text_list` (one element per indent run)
- block quote → `rich_text` › `rich_text_quote`
- fenced/indented code → `rich_text` › `rich_text_preformatted`
- thematic break (`---`) → `divider`
- GFM table → native `table` (head + body rows, `:--`/`--:` alignment →
  `column_settings`, cells as `raw_text` or `rich_text` for links/styles)

Inline runs use `rich_text` element style flags (bold/italic/strike/code) and
`link` elements, so Slack handles all escaping of `& < >` — we never rewrite
Markdown into mrkdwn strings.

Sending: `client.post_message`/`update_message` take an optional `blocks` slice
and always send `text` as the fallback (notifications, older clients, and
accessibility). `render_message` (in `slack/mod.rs`) returns the
`(text, blocks)` pair and falls back to **plain text only** (`blocks = None`)
when the render is empty, would exceed Slack's 50-block cap (`MAX_BLOCKS`), or
the message carries a Slack control sequence (`<@user>`, `<#channel>`,
`<!here>`) — those expand only in the `text` field, not inside `rich_text`. So
a formatting edge case never fails the turn.

Streaming: the reply still streams as plain text + cursor, but each throttled
edit (and the finalize) re-renders the accumulated Markdown to blocks, so the
formatting updates live. `pulldown-cmark` tolerates the partial Markdown a
half-streamed reply produces.

Scope: model replies, the `!`-command output (`render_*` now emit standard
Markdown), and the turn-failure error all flow through the one render path.

Dependency: `pulldown-cmark` (CommonMark + GFM tables, `default-features` off).

Testing: the render module and `render_message` are unit-tested (every block
type, nested lists, tables, the fallback paths); the live Slack rendering is a
manual checkpoint.
