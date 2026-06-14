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

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use tapir_bot_core::access::{Access, LoopGuard};
use tapir_bot_core::backend::TURN_FAILED_MESSAGE;
use tapir_bot_core::event::Inbound;
use tapir_bot_core::{ChatBackend, Engine, ReplySink};

use client::Client;

pub use config::{Reactions, SlackConfig};

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
}

impl SlackBackend {
    /// Build the backend from the two Slack tokens and the Slack config.
    pub fn new(app_token: String, bot_token: String, config: SlackConfig) -> Self {
        Self { app_token, bot_token, config }
    }

    /// Build the backend from the environment: `SLACK_APP_TOKEN` (`xapp-…`) and
    /// `SLACK_BOT_TOKEN` (`xoxb-…`), plus the given Slack config. A missing or
    /// blank token is a clear error.
    pub fn from_env(config: SlackConfig) -> anyhow::Result<Self> {
        let app_token = require_env_token("SLACK_APP_TOKEN", std::env::var("SLACK_APP_TOKEN").ok())?;
        let bot_token = require_env_token("SLACK_BOT_TOKEN", std::env::var("SLACK_BOT_TOKEN").ok())?;
        Ok(Self::new(app_token, bot_token, config))
    }
}

#[async_trait::async_trait]
impl ChatBackend for SlackBackend {
    async fn run(self, engine: Arc<Engine>) -> anyhow::Result<()> {
        let client = Arc::new(Client::new(self.app_token, self.bot_token));
        let bot_user_id = client.bot_user_id().await.context("authenticating with Slack")?;
        tracing::info!(%bot_user_id, "connected to Slack");

        // The Slack backend owns the access policy and the bot loop cap (their
        // config is Slack-side); it passes them to the engine per event.
        let access = self.config.access;
        let reactions = Arc::new(self.config.reactions);
        let mut loop_guard = LoopGuard::new(access.bot_turn_limit);
        if access.channels.is_empty() && access.dm.users.is_empty() && !access.allow_bots {
            tracing::warn!(
                "access policy is empty — the bot will respond to nothing; configure [access]"
            );
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
                    &client, &engine, &access, &mut loop_guard, &reactions, &bot_user_id,
                ) => {
                    match result {
                        // A clean end (disconnect/close) reopens immediately.
                        Ok(()) => tracing::info!("socket closed, reopening"),
                        // A failure backs off first so we do not hammer Slack.
                        Err(error) => {
                            tracing::warn!(error = format!("{error:#}"), "connection failed, reopening");
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
async fn run_one_connection(
    client: &Arc<Client>,
    engine: &Arc<Engine>,
    access: &Access,
    loop_guard: &mut LoopGuard,
    reactions: &Arc<Reactions>,
    bot_user_id: &str,
) -> anyhow::Result<()> {
    let url = client.open_connection().await.context("opening a Socket Mode connection")?;
    let (mut socket, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .context("connecting the Socket Mode websocket")?;
    tracing::info!("socket open");

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
        if let Some(inbound) = decision.inbound
            && engine.should_handle(access, loop_guard, bot_user_id, &inbound).await
        {
            let client = Arc::clone(client);
            let engine = Arc::clone(engine);
            let reactions = Arc::clone(reactions);
            let bot_user_id = bot_user_id.to_string();
            tokio::spawn(async move {
                handle_inbound(&client, &engine, &reactions, &bot_user_id, &inbound).await;
            });
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
    bot_user_id: &str,
    inbound: &Inbound,
) {
    add_reaction(client, &inbound.channel, &inbound.ts, &r.seen).await;

    let mut sink = SlackReply::new(client, inbound.channel.clone(), inbound.thread.clone());
    let result = engine.handle(bot_user_id, inbound, &mut sink).await;

    remove_reaction(client, &inbound.channel, &inbound.ts, &r.seen).await;
    match result {
        Ok(()) => add_reaction(client, &inbound.channel, &inbound.ts, &r.done).await,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "the turn failed");
            let mut sink = SlackReply::new(client, inbound.channel.clone(), inbound.thread.clone());
            let _ = sink.update(TURN_FAILED_MESSAGE, true).await;
            add_reaction(client, &inbound.channel, &inbound.ts, &r.failed).await;
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
    use super::{has_slack_controls, render_message, require_env_token};

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
}
