//! The extension points a chat backend implements. The backend owns its own
//! connection and event loop ([`ChatBackend`]) and the reply transport
//! ([`ReplySink`]); the engine supplies the turn logic. This is the seam that
//! lets the same engine drive Slack today and Discord/IRC/Google Chat/Teams
//! later.

use std::sync::Arc;

use crate::engine::Engine;
use crate::event::{Inbound, ReactionEvent};

/// A hook for observing the backend's lifecycle — connection health, message
/// flow, and turn outcomes — so a consumer can emit metrics (Prometheus, …) or
/// custom logging without the library depending on any metrics backend. Every
/// method defaults to a no-op, so a consumer implements only what it needs and
/// hands an `Arc<dyn BackendObserver>` to its backend.
///
/// Calls are synchronous and run on the backend's read loop: bump a counter or
/// set a gauge and return — do no blocking or slow work.
pub trait BackendObserver: Send + Sync {
    /// The connection is established (the socket opened / the server is ready).
    fn connected(&self) {}
    /// The connection ended and the backend will reopen. `after_error` is true
    /// for a backed-off reconnect after a failure, false for a clean reopen.
    fn reconnecting(&self, after_error: bool) {
        let _ = after_error;
    }
    /// A message was received. `admitted` is true when it will be handled (a
    /// turn runs), false when the access policy or another gate dropped it.
    fn received(&self, inbound: &Inbound, admitted: bool) {
        let _ = (inbound, admitted);
    }
    /// An *addressed* message (a mention or DM) was denied by the access policy.
    fn denied(&self, inbound: &Inbound) {
        let _ = inbound;
    }
    /// A turn finished. `ok` is false when it errored.
    fn turn_finished(&self, inbound: &Inbound, ok: bool) {
        let _ = (inbound, ok);
    }
    /// An emoji reaction was added to a message. The seam for using reactions
    /// as signals (approvals, acks): the consumer decides what it means. Like
    /// the others, this runs on the backend's read loop — do no slow work
    /// (hand the reaction to a queue/task and return).
    fn reaction(&self, reaction: &ReactionEvent) {
        let _ = reaction;
    }
}

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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::event::{Inbound, ReactionEvent};

    #[derive(Default)]
    struct Counter {
        received: AtomicUsize,
        denied: AtomicUsize,
        turns_ok: AtomicUsize,
        reactions: AtomicUsize,
    }

    impl BackendObserver for Counter {
        fn received(&self, _inbound: &Inbound, _admitted: bool) {
            self.received.fetch_add(1, Ordering::Relaxed);
        }
        fn denied(&self, _inbound: &Inbound) {
            self.denied.fetch_add(1, Ordering::Relaxed);
        }
        fn turn_finished(&self, _inbound: &Inbound, ok: bool) {
            if ok {
                self.turns_ok.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn reaction(&self, _reaction: &ReactionEvent) {
            self.reactions.fetch_add(1, Ordering::Relaxed);
        }
        // connected / reconnecting are left as the default no-ops.
    }

    fn inbound() -> Inbound {
        Inbound {
            channel: "C".into(),
            ts: "1".into(),
            thread: "1".into(),
            user: "U".into(),
            text: "hi".into(),
            is_bot: false,
            is_dm: false,
            continuation: false,
        }
    }

    #[test]
    fn overrides_fire_and_defaults_are_callable_no_ops() {
        let counter = Counter::default();
        let observer: &dyn BackendObserver = &counter;
        // Defaults must be callable without panicking.
        observer.connected();
        observer.reconnecting(true);
        // Overridden methods record.
        observer.received(&inbound(), true);
        observer.denied(&inbound());
        observer.turn_finished(&inbound(), true);
        observer.turn_finished(&inbound(), false);
        observer.reaction(&reaction());
        assert_eq!(counter.received.load(Ordering::Relaxed), 1);
        assert_eq!(counter.denied.load(Ordering::Relaxed), 1);
        assert_eq!(counter.turns_ok.load(Ordering::Relaxed), 1, "only the ok turn counts");
        assert_eq!(counter.reactions.load(Ordering::Relaxed), 1);
    }

    fn reaction() -> ReactionEvent {
        ReactionEvent {
            channel: "C".into(),
            ts: "1".into(),
            user: "U".into(),
            reaction: "thumbsup".into(),
        }
    }
}
