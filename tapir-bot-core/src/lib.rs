//! tapir-bot-core — the backend-neutral bot engine.
//!
//! It owns everything a chat bot does that isn't tied to a specific chat
//! service: the backend-neutral config (model, storage, sandbox), the turn
//! lifecycle, conversation memory, the `!`-commands, the reusable access
//! mechanism, and tool execution. A chat service (Slack, Discord, IRC, Google
//! Chat, Teams, …) is a separate crate that implements [`backend::ChatBackend`]
//! and [`backend::ReplySink`] and drives this engine. Backend-specific config
//! (Slack reactions, the access *value*, …) lives with its backend.
//!
//! - [`config::Config`] — the neutral schema: `agent`, `storage`, `sandbox`.
//! - [`access`] — the reusable allowlist policy (an [`Access`](access::Access)
//!   a backend embeds in its own config) plus the [`LoopGuard`](access::LoopGuard).
//! - [`event::Inbound`] — the neutral message a backend hands the engine.
//! - [`engine::Engine`] — admit a message, then process it (command or turn).
//! - [`backend`] — the [`ChatBackend`](backend::ChatBackend) /
//!   [`ReplySink`](backend::ReplySink) extension points.
//! - [`tools`] — how a turn's tools execute (text-only, host, sandbox).

pub mod access;
pub mod backend;
pub mod commands;
pub mod config;
pub mod engine;
pub mod event;
pub mod memory;
pub mod meta;
pub mod tools;

pub use access::{Access, LoopGuard};
pub use backend::{BackendObserver, ChatBackend, ReplySink};
pub use config::Config;
pub use engine::{AgentSettings, Engine};
pub use event::Inbound;
pub use tools::Tools;
