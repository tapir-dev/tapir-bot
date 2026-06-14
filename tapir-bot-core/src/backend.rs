//! The extension points a chat backend implements. The backend owns its own
//! connection and event loop ([`ChatBackend`]) and the reply transport
//! ([`ReplySink`]); the engine supplies the turn logic. This is the seam that
//! lets the same engine drive Slack today and Discord/IRC/Google Chat/Teams
//! later.

use std::sync::Arc;

use crate::engine::Engine;

/// The reply shown to the user, fed the accumulating turn text. The engine
/// calls [`update`](ReplySink::update) as the model streams; the backend
/// decides how to surface it (post-then-edit, throttle, rich formatting).
///
/// A one-shot reply (a command, or an error) is a single `update(text, true)`.
#[async_trait::async_trait]
pub trait ReplySink: Send {
    /// Show `text` as the reply so far. `done` marks the final update — the
    /// backend must flush it (and drop any streaming cursor). Intermediate
    /// updates (`done == false`) may be coalesced or throttled.
    async fn update(&mut self, text: &str, done: bool) -> anyhow::Result<()>;
}

/// A chat backend: it owns its connection and event loop, calling the shared
/// [`Engine`] to process each admitted message. Socket backends (Slack, IRC)
/// run a read loop; webhook backends (Teams, Google Chat) serve HTTP — the
/// trait imposes neither.
#[async_trait::async_trait]
pub trait ChatBackend {
    /// Run until the process is stopped. Resolve the bot's identity, deliver
    /// each event through `engine`, and apply the lifecycle signals.
    async fn run(self, engine: Arc<Engine>) -> anyhow::Result<()>;
}

/// The message posted when a turn fails. Backends render and post it (then
/// signal failure) so a bad turn still gets a visible reply.
pub const TURN_FAILED_MESSAGE: &str = "⚠️ Sorry — that turn failed. Please try again.";
