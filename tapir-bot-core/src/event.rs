//! The neutral inbound event a backend hands the engine. A chat backend
//! (Slack, Discord, IRC, …) maps its own protocol onto this shape; the engine
//! reasons only about it. Ids (`channel`/`ts`/`thread`/`user`) are opaque
//! strings the backend defines.

/// A message to consider answering: the channel, the message id (`ts`), the
/// thread to reply in (the message's own thread, or its `ts` when it starts
/// one), the author, the raw text (a mention still carries the leading
/// `<@bot>`), and whether the author is a bot and whether this is a DM. The
/// access policy and the engine decide whether to actually answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub channel: String,
    pub ts: String,
    pub thread: String,
    pub user: String,
    pub text: String,
    pub is_bot: bool,
    pub is_dm: bool,
    /// A non-mention channel thread reply: only handled when the bot is already
    /// in the thread (and it doesn't mention the bot). Mentions and DM messages
    /// are not continuations — they always trigger (subject to policy).
    pub continuation: bool,
}

/// An emoji reaction added to a message. The seam for reaction-as-signal flows
/// (approvals, acknowledgements): a backend surfaces it via
/// [`BackendObserver::reaction`](crate::backend::BackendObserver::reaction).
/// Unlike an [`Inbound`], a reaction never drives an engine turn on its own —
/// the consumer decides what (if anything) it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionEvent {
    /// The channel of the message that was reacted to.
    pub channel: String,
    /// The `ts` of the message that was reacted to.
    pub ts: String,
    /// The user who added the reaction.
    pub user: String,
    /// The emoji name without colons (e.g. `thumbsup`, `white_check_mark`).
    pub reaction: String,
}
