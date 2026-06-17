//! tapir-bot-slack — the Slack backend for [`tapir_bot_core`].
//!
//! It owns everything Slack-specific: the Socket Mode connection and read loop,
//! the Web API client ([`client`]), and Block Kit rendering ([`render`]). It
//! implements [`ChatBackend`] (it gives the loop, calling the engine per event)
//! and [`ReplySink`] (it posts/edits a streaming message with a cursor and
//! throttling). The pure protocol decisions live in [`protocol`].

mod client;
mod config;
mod protocol;
mod render;

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use tapir_bot_core::access::{Access, LoopGuard};
use tapir_bot_core::backend::TURN_FAILED_MESSAGE;
use tapir_bot_core::event::Inbound;
use tapir_bot_core::{BackendObserver, ChatBackend, Engine, ReplySink};

use client::Client;

pub use config::{Denial, Reactions, SlackConfig};

/// How long to wait before reopening after a failed connection, so a
/// persistent failure (bad token, Slack outage) does not spin.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Minimum gap between streaming message edits — keeps under Slack's
/// chat.update rate limit while staying responsive.
const STREAM_THROTTLE: Duration = Duration::from_millis(1000);

/// Appended to the reply while it is still streaming; dropped on the final edit.
const CURSOR: &str = " ▌";

/// Slack caps a message at 50 blocks. A reply that would exceed this (or one
/// that renders to nothing) falls back to plain `text` so the turn never fails
/// over formatting.
const MAX_BLOCKS: usize = 50;

/// The Slack backend: a Socket Mode bot. Built from the two Slack tokens and the
/// Slack-side config (reactions + access), then handed to `tapir_bot::Bot` (or
/// driven directly via [`ChatBackend::run`]).
pub struct SlackBackend {
    /// `xapp-…` — opens the Socket Mode connection.
    app_token: String,
    /// `xoxb-…` — speaks the Web API.
    bot_token: String,
    /// The Slack-side config: lifecycle reactions and the access allowlist.
    config: SlackConfig,
    /// Optional lifecycle observer (metrics/logging).
    observer: Option<Arc<dyn BackendObserver>>,
    /// The live access policy, seeded from `config.access`. Held behind a lock
    /// so a consumer can swap it at runtime (see [`access_handle`]); the read
    /// loop re-reads it per event.
    ///
    /// [`access_handle`]: SlackBackend::access_handle
    access: Arc<RwLock<Access>>,
}

impl SlackBackend {
    /// Build the backend from the two Slack tokens and the Slack config.
    pub fn new(app_token: String, bot_token: String, config: SlackConfig) -> Self {
        let access = Arc::new(RwLock::new(config.access.clone()));
        Self { app_token, bot_token, config, observer: None, access }
    }

    /// Build the backend from the environment: `SLACK_APP_TOKEN` (`xapp-…`) and
    /// `SLACK_BOT_TOKEN` (`xoxb-…`), plus the given Slack config. A missing or
    /// blank token is a clear error.
    pub fn from_env(config: SlackConfig) -> anyhow::Result<Self> {
        let app_token = require_env_token("SLACK_APP_TOKEN", std::env::var("SLACK_APP_TOKEN").ok())?;
        let bot_token = require_env_token("SLACK_BOT_TOKEN", std::env::var("SLACK_BOT_TOKEN").ok())?;
        Ok(Self::new(app_token, bot_token, config))
    }

    /// Attach a [`BackendObserver`] for connection/turn metrics or logging.
    pub fn observer(mut self, observer: Arc<dyn BackendObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// A handle to the live access policy, for changing who the bot answers
    /// without a restart. Replace it through the lock — e.g.
    /// `*backend.access_handle().write().unwrap() = new_access;` — and the read
    /// loop picks it up on the next event. Take the handle before
    /// [`ChatBackend::run`] consumes the backend.
    ///
    /// Note: the per-turn loop cap (`access.bot_turn_limit`) is read once at
    /// startup; only the allowlist (channels/dm/bots) is re-read live.
    pub fn access_handle(&self) -> Arc<RwLock<Access>> {
        Arc::clone(&self.access)
    }
}

#[async_trait::async_trait]
impl ChatBackend for SlackBackend {
    async fn run(self, engine: Arc<Engine>) -> anyhow::Result<()> {
        let client = Arc::new(Client::new(self.app_token, self.bot_token));
        let bot_user_id = client.bot_user_id().await.context("authenticating with Slack")?;
        tracing::info!(%bot_user_id, "connected to Slack");

        // The Slack backend owns the access policy and the bot loop cap (their
        // config is Slack-side); it passes them to the engine per event. The
        // policy lives behind a lock so a consumer can swap it at runtime; the
        // loop cap is fixed at startup.
        let access = self.access;
        let reactions = Arc::new(self.config.reactions);
        let denial = Arc::new(self.config.denial);
        let observer = self.observer;
        let mut loop_guard = LoopGuard::new(access.read().unwrap().bot_turn_limit);
        {
            let a = access.read().unwrap();
            if a.channels.is_empty() && a.dm.users.is_empty() && !a.allow_bots {
                tracing::warn!(
                    "access policy is empty — the bot will respond to nothing; configure [access]"
                );
            }
        }

        // Reconnect forever: Slack refreshes the socket roughly hourly, and a
        // dropped socket should just reopen — until Ctrl-C ends the loop.
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutting down");
                    return Ok(());
                }
                result = run_one_connection(
                    &client, &engine, &access, &mut loop_guard, &reactions, &denial, &observer,
                    &bot_user_id,
                ) => {
                    match result {
                        // A clean end (disconnect/close) reopens immediately.
                        Ok(()) => {
                            tracing::info!("socket closed, reopening");
                            notify(&observer, |o| o.reconnecting(false));
                        }
                        // A failure backs off first so we do not hammer Slack.
                        Err(error) => {
                            tracing::warn!(error = format!("{error:#}"), "connection failed, reopening");
                            notify(&observer, |o| o.reconnecting(true));
                            tokio::time::sleep(RECONNECT_DELAY).await;
                        }
                    }
                }
            }
        }
    }
}

/// Open one Socket Mode connection and process frames until it disconnects or
/// the socket closes. Returns `Err` for failures the caller should back off on.
// The read loop threads the connection's shared pieces (client, engine, policy,
// reactions, denial, observer, bot id) plus the mutable loop guard; grouping
// them into a context struct would obscure more than the arg list does.
#[allow(clippy::too_many_arguments)]
async fn run_one_connection(
    client: &Arc<Client>,
    engine: &Arc<Engine>,
    access: &Arc<RwLock<Access>>,
    loop_guard: &mut LoopGuard,
    reactions: &Arc<Reactions>,
    denial: &Arc<Denial>,
    observer: &Option<Arc<dyn BackendObserver>>,
    bot_user_id: &str,
) -> anyhow::Result<()> {
    let url = client.open_connection().await.context("opening a Socket Mode connection")?;
    let (mut socket, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .context("connecting the Socket Mode websocket")?;
    tracing::info!("socket open");
    notify(observer, |o| o.connected());

    while let Some(frame) = socket.next().await {
        let text = match frame {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Close(_)) => break,
            // Ping/pong are handled by the library; ignore the rest.
            Ok(_) => continue,
            Err(error) => return Err(error).context("reading a websocket frame"),
        };

        let decision = protocol::handle_frame(&text);

        // Ack before the (slower) reply, or Slack redelivers the envelope.
        if let Some(envelope_id) = decision.ack {
            let ack = serde_json::json!({ "envelope_id": envelope_id }).to_string();
            socket.send(Message::text(ack)).await.context("acking an envelope")?;
        }

        // Apply the access policy before doing any work, and run the turn off
        // the read loop so the socket keeps being polled (Slack's pings get
        // answered) while the model thinks.
        if let Some(inbound) = decision.inbound {
            // Snapshot the live policy for this event (a consumer may have
            // swapped it). Clone so no lock is held across the turn's awaits.
            let access = access.read().unwrap().clone();
            let admitted = engine.should_handle(&access, loop_guard, bot_user_id, &inbound).await;
            notify(observer, |o| o.received(&inbound, admitted));
            if admitted {
                let client = Arc::clone(client);
                let engine = Arc::clone(engine);
                let reactions = Arc::clone(reactions);
                let observer = observer.clone();
                let bot_user_id = bot_user_id.to_string();
                tokio::spawn(async move {
                    handle_inbound(&client, &engine, &reactions, &observer, &bot_user_id, &inbound)
                        .await;
                });
            } else if denied_when_addressed(&inbound, bot_user_id, &access) {
                // A non-allowed user addressed the bot: record the denial and,
                // if a message is configured, tell them (ephemerally), off the
                // read loop.
                notify(observer, |o| o.denied(&inbound));
                if let Some(message) = denial.message.clone() {
                    let client = Arc::clone(client);
                    tokio::spawn(async move {
                        send_denial(&client, &inbound, &message).await;
                    });
                }
            }
        }

        // A reaction is a signal, not a turn: surface it to the observer (which
        // queues it) and move on. The engine never acts on it directly.
        if let Some(reaction) = decision.reaction {
            notify(observer, |o| o.reaction(&reaction));
        }

        if decision.reconnect {
            break;
        }
    }
    Ok(())
}

/// Answer one message: 👀, run the engine streaming into a Slack message, then
/// ✅ — or, on failure, a short note and ❌. Handles its own errors (reactions
/// are best-effort) so one bad turn never takes down the read loop.
async fn handle_inbound(
    client: &Client,
    engine: &Engine,
    r: &Reactions,
    observer: &Option<Arc<dyn BackendObserver>>,
    bot_user_id: &str,
    inbound: &Inbound,
) {
    add_reaction(client, &inbound.channel, &inbound.ts, &r.seen).await;

    let mut sink = SlackReply::new(client, inbound.channel.clone(), inbound.thread.clone());
    let result = engine.handle(bot_user_id, inbound, &mut sink).await;

    remove_reaction(client, &inbound.channel, &inbound.ts, &r.seen).await;
    let ok = result.is_ok();
    match result {
        Ok(()) => add_reaction(client, &inbound.channel, &inbound.ts, &r.done).await,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "the turn failed");
            let mut sink = SlackReply::new(client, inbound.channel.clone(), inbound.thread.clone());
            let _ = sink.update(TURN_FAILED_MESSAGE, true).await;
            add_reaction(client, &inbound.channel, &inbound.ts, &r.failed).await;
        }
    }
    notify(observer, |o| o.turn_finished(inbound, ok));
}

/// Call `f` with the observer when one is set; a no-op otherwise. Keeps the
/// `Option` plumbing out of the lifecycle call sites.
fn notify(observer: &Option<Arc<dyn BackendObserver>>, f: impl FnOnce(&dyn BackendObserver)) {
    if let Some(observer) = observer {
        f(observer.as_ref());
    }
}

/// Whether a not-handled `inbound` warrants the access-denied reply: it was
/// *addressed* to the bot (a mention or a DM — not a thread continuation, not
/// our own message, not a bot) yet the access policy denied it. This is the
/// subset of `should_handle == false` that is a real "you can't do that", as
/// opposed to silent drops (self, bot loops, unknown threads).
fn denied_when_addressed(inbound: &Inbound, bot_user_id: &str, access: &Access) -> bool {
    inbound.user != bot_user_id
        && !inbound.is_bot
        && !inbound.continuation
        && !tapir_bot_core::access::allows(access, inbound)
}

/// Send the access-denied `message` to the user who addressed the bot: styled,
/// ephemeral first (only they see it), falling back to a DM if the ephemeral
/// post fails (e.g. the bot isn't in the channel). Best-effort — a failure is
/// logged, never propagated.
async fn send_denial(client: &Client, inbound: &Inbound, message: &str) {
    let (fallback, blocks) = render_message(message);
    if let Err(error) = client
        .post_ephemeral(&inbound.channel, &inbound.user, &fallback, blocks.as_deref())
        .await
    {
        tracing::debug!(error = format!("{error:#}"), "ephemeral denial failed; trying a DM");
        if let Err(error) =
            client.post_message(&inbound.user, "", &fallback, blocks.as_deref()).await
        {
            tracing::warn!(error = format!("{error:#}"), "denial DM fallback failed");
        }
    }
}

/// A streaming reply posted into a single Slack message: posted on the first
/// chunk (with a trailing cursor), edited as more arrives (throttled), and
/// finalized without the cursor. Implements [`ReplySink`].
struct SlackReply<'a> {
    client: &'a Client,
    channel: String,
    thread: String,
    message_ts: Option<String>,
    last_edit: Instant,
}

impl<'a> SlackReply<'a> {
    fn new(client: &'a Client, channel: String, thread: String) -> Self {
        Self { client, channel, thread, message_ts: None, last_edit: Instant::now() }
    }
}

#[async_trait::async_trait]
impl ReplySink for SlackReply<'_> {
    async fn update(&mut self, text: &str, done: bool) -> anyhow::Result<()> {
        if done {
            // Finalize: drop the cursor, editing the streamed message or posting
            // one if nothing was streamed.
            let (fallback, blocks) = render_message(text);
            match &self.message_ts {
                Some(ts) => {
                    let ts = ts.clone();
                    self.client
                        .update_message(&self.channel, &ts, &fallback, blocks.as_deref())
                        .await?;
                }
                None => {
                    self.client
                        .post_message(&self.channel, &self.thread, &fallback, blocks.as_deref())
                        .await?;
                }
            }
            return Ok(());
        }

        let rendered = format!("{text}{CURSOR}");
        match &self.message_ts {
            // First chunk: post the message so the user sees it appear.
            None => {
                let (fallback, blocks) = render_message(&rendered);
                let ts = self
                    .client
                    .post_message(&self.channel, &self.thread, &fallback, blocks.as_deref())
                    .await?;
                self.message_ts = Some(ts);
                self.last_edit = Instant::now();
            }
            // Later chunks: edit, throttled to stay under the rate limit.
            Some(ts) if self.last_edit.elapsed() >= STREAM_THROTTLE => {
                let ts = ts.clone();
                let (fallback, blocks) = render_message(&rendered);
                self.client.update_message(&self.channel, &ts, &fallback, blocks.as_deref()).await?;
                self.last_edit = Instant::now();
            }
            Some(_) => {}
        }
        Ok(())
    }
}

/// Render `markdown` into the `(text fallback, blocks)` pair to send. `blocks`
/// is `None` — so the caller sends plain `text` only — when the render is empty,
/// over the block cap, or the message carries Slack control sequences
/// (`<@user>`, `<#channel>`, `<!here>`) which only resolve in the `text` field,
/// not inside `rich_text` elements.
fn render_message(markdown: &str) -> (String, Option<Vec<serde_json::Value>>) {
    let fallback = render::fallback_text(markdown);
    if has_slack_controls(markdown) {
        return (fallback, None);
    }
    let blocks = render::to_blocks(markdown);
    if blocks.is_empty() || blocks.len() > MAX_BLOCKS {
        (fallback, None)
    } else {
        (fallback, Some(blocks))
    }
}

/// Whether `s` contains a Slack control sequence (`<@U…>`, `<#C…>`, `<!here>`)
/// that Slack only expands in a message's plain `text` field.
fn has_slack_controls(s: &str) -> bool {
    s.contains("<@") || s.contains("<#") || s.contains("<!")
}

/// Add `name` as a reaction unless it is empty (empty disables it).
async fn add_reaction(client: &Client, channel: &str, ts: &str, name: &str) {
    if !name.is_empty() {
        let _ = client.add_reaction(channel, ts, name).await;
    }
}

/// Remove `name` unless it is empty.
async fn remove_reaction(client: &Client, channel: &str, ts: &str, name: &str) {
    if !name.is_empty() {
        let _ = client.remove_reaction(channel, ts, name).await;
    }
}

/// Require a runtime token from the environment. Blank counts as absent; a
/// missing token is a clear error naming the variable.
fn require_env_token(name: &str, value: Option<String>) -> anyhow::Result<String> {
    value
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .with_context(|| format!("{name} must be set (export it from your secret store)"))
}

#[cfg(test)]
mod tests {
    use tapir_bot_core::access::{Access, ChannelAccess, DmAccess};
    use tapir_bot_core::event::Inbound;

    use super::{denied_when_addressed, has_slack_controls, render_message, require_env_token};

    const BOT: &str = "U0BOT";

    fn inbound(user: &str, is_dm: bool, continuation: bool) -> Inbound {
        Inbound {
            channel: if is_dm { "D1".into() } else { "C1".into() },
            ts: "1.1".into(),
            thread: "1.1".into(),
            user: user.into(),
            text: "<@U0BOT> oi".into(),
            is_bot: false,
            is_dm,
            continuation,
        }
    }

    #[test]
    fn an_addressed_stranger_is_denied() {
        let access = Access::default(); // deny-by-default
        // A DM from a non-listed user, and a mention in an unlisted channel.
        assert!(denied_when_addressed(&inbound("U-stranger", true, false), BOT, &access));
        assert!(denied_when_addressed(&inbound("U-stranger", false, false), BOT, &access));
    }

    #[test]
    fn an_allowed_user_is_not_denied() {
        let access = Access { dm: DmAccess { users: vec!["U1".into()] }, ..Access::default() };
        assert!(!denied_when_addressed(&inbound("U1", true, false), BOT, &access));
    }

    #[test]
    fn the_bot_itself_and_other_bots_are_not_denied() {
        let access = Access::default();
        // our own message
        assert!(!denied_when_addressed(&inbound(BOT, true, false), BOT, &access));
        // another bot
        let mut bot_msg = inbound("U-bot", false, false);
        bot_msg.is_bot = true;
        assert!(!denied_when_addressed(&bot_msg, BOT, &access));
    }

    #[test]
    fn a_thread_continuation_is_not_a_denial() {
        // Plain chatter in a channel thread is not "addressed" — never deny it,
        // even though access would drop it.
        let access = Access::default();
        assert!(!denied_when_addressed(&inbound("U-stranger", false, true), BOT, &access));
    }

    #[test]
    fn a_channel_member_outside_the_user_list_is_denied() {
        let mut access = Access::default();
        access
            .channels
            .insert("C1".into(), ChannelAccess { users: vec!["U1".into()] });
        assert!(denied_when_addressed(&inbound("U2", false, false), BOT, &access));
        assert!(!denied_when_addressed(&inbound("U1", false, false), BOT, &access));
    }

    #[test]
    fn render_message_emits_blocks_for_markdown() {
        let (fallback, blocks) = render_message("# Title\n\nbody");
        assert_eq!(fallback, "# Title\n\nbody");
        let blocks = blocks.expect("markdown renders to blocks");
        assert_eq!(blocks[0]["type"], "header");
    }

    #[test]
    fn render_message_falls_back_to_text_for_slack_mentions() {
        let (fallback, blocks) = render_message("Only <@U123> can do that.");
        assert_eq!(fallback, "Only <@U123> can do that.");
        assert!(blocks.is_none(), "mentions only resolve in the text field");
        assert!(has_slack_controls("<#C1>") && has_slack_controls("<!here>"));
        assert!(!has_slack_controls("plain text"));
    }

    #[test]
    fn a_missing_env_token_names_the_variable() {
        let err = require_env_token("SLACK_APP_TOKEN", None).expect_err("missing is an error");
        assert!(format!("{err:#}").contains("SLACK_APP_TOKEN"), "{err:#}");
        let err =
            require_env_token("SLACK_BOT_TOKEN", Some("  ".into())).expect_err("blank is an error");
        assert!(format!("{err:#}").contains("SLACK_BOT_TOKEN"), "{err:#}");
    }

    #[test]
    fn a_present_env_token_is_trimmed() {
        let tok = require_env_token("SLACK_BOT_TOKEN", Some(" xoxb-1 \n".into())).unwrap();
        assert_eq!(tok, "xoxb-1");
    }

    #[test]
    fn the_access_handle_swaps_the_live_policy() {
        use super::SlackBackend;
        let backend =
            SlackBackend::new("xapp-1".into(), "xoxb-1".into(), crate::SlackConfig::default());
        let handle = backend.access_handle();
        assert!(handle.read().unwrap().dm.users.is_empty(), "seeded from default (deny-all)");
        // A consumer swaps the policy at runtime.
        *handle.write().unwrap() =
            Access { dm: DmAccess { users: vec!["U1".into()] }, ..Access::default() };
        assert_eq!(handle.read().unwrap().dm.users, vec!["U1"], "the live policy reflects the swap");
    }
}
