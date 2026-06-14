//! The access policy and the decision it drives: who may make the bot take a
//! turn, where. A reusable mechanism — the policy types are generic over opaque
//! channel/user ids, so any backend (Slack, Discord, IRC, …) can embed an
//! [`Access`] in its own config and call [`allows`]. The decision is pure;
//! the per-thread bot loop cap ([`LoopGuard`]) is stateful and lives in the
//! backend's read loop, not here.

use std::collections::HashMap;

use serde::Deserialize;

use crate::event::Inbound;

/// The access policy: who may make the bot take a turn, where. Deny-by-default
/// — only what is listed here triggers. Bots are gated by `allow_bots` plus a
/// per-thread turn cap (the [`LoopGuard`], applied at runtime).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Access {
    /// Whether bot-authored messages may trigger turns at all. Off by default.
    pub allow_bots: bool,
    /// The cap on bot-triggered turns per thread, breaking bot-to-bot loops.
    pub bot_turn_limit: u32,
    /// Who may DM the bot.
    pub dm: DmAccess,
    /// The allowlisted channels, keyed by channel id. Channels not listed here
    /// are ignored entirely.
    pub channels: HashMap<String, ChannelAccess>,
}

impl Default for Access {
    fn default() -> Self {
        Self {
            allow_bots: false,
            bot_turn_limit: 3,
            dm: DmAccess::default(),
            channels: HashMap::new(),
        }
    }
}

/// The DM policy: the user allowlist. Empty (or absent) means no one may DM
/// the bot.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DmAccess {
    /// Stable user ids allowed to DM the bot.
    pub users: Vec<String>,
}

/// One allowlisted channel's rule: everyone may mention by default, or only
/// the listed users when `users` is non-empty.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChannelAccess {
    /// When non-empty, only these user ids may trigger turns in this channel;
    /// empty means any member may.
    pub users: Vec<String>,
}

/// A coarse cap on how many threads the loop guard tracks at once, so a
/// long-running bot does not grow the table without bound.
const MAX_TRACKED_THREADS: usize = 4096;

/// Per-thread cap on bot-triggered turns, breaking bot-to-bot loops. In-memory
/// and per-process (resets on restart); loops are short-lived so a coarse size
/// bound is enough.
pub struct LoopGuard {
    limit: u32,
    counts: HashMap<String, u32>,
}

impl LoopGuard {
    pub fn new(limit: u32) -> Self {
        Self { limit, counts: HashMap::new() }
    }

    /// Record a bot-triggered turn in `thread` and report whether it is within
    /// the limit: the first `limit` bot turns in a thread return `true`,
    /// further ones return `false` (cutting the loop).
    pub fn allow_bot_turn(&mut self, thread: &str) -> bool {
        if self.counts.len() >= MAX_TRACKED_THREADS && !self.counts.contains_key(thread) {
            tracing::warn!("loop-guard thread table full, resetting");
            self.counts.clear();
        }
        let count = self.counts.entry(thread.to_string()).or_insert(0);
        if *count >= self.limit {
            return false;
        }
        *count += 1;
        true
    }
}

/// Whether `inbound` may trigger a turn under `access`. Deny-by-default:
///
/// - a bot only when `allow_bots` (the per-thread cap is enforced separately);
/// - a DM only from a listed user (a bot DM rides `allow_bots`);
/// - a channel only when allowlisted, and then any member unless the channel
///   restricts to specific users (a bot rides `allow_bots`).
pub fn allows(access: &Access, inbound: &Inbound) -> bool {
    if inbound.is_bot && !access.allow_bots {
        return false;
    }
    if inbound.is_dm {
        return inbound.is_bot || access.dm.users.iter().any(|u| u == &inbound.user);
    }
    match access.channels.get(&inbound.channel) {
        Some(channel) => {
            inbound.is_bot
                || channel.users.is_empty()
                || channel.users.iter().any(|u| u == &inbound.user)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(channel: &str, user: &str, is_bot: bool, is_dm: bool) -> Inbound {
        Inbound {
            channel: channel.into(),
            ts: "1.1".into(),
            thread: "1.1".into(),
            user: user.into(),
            text: "hi".into(),
            is_bot,
            is_dm,
            continuation: false,
        }
    }

    fn access_with_channel(id: &str, users: Vec<&str>) -> Access {
        let mut access = Access::default();
        access.channels.insert(
            id.into(),
            ChannelAccess { users: users.into_iter().map(String::from).collect() },
        );
        access
    }

    #[test]
    fn an_unlisted_channel_is_denied() {
        let access = access_with_channel("C1", vec![]);
        assert!(!allows(&access, &msg("C2", "U1", false, false)));
    }

    #[test]
    fn a_listed_channel_allows_any_member() {
        let access = access_with_channel("C1", vec![]);
        assert!(allows(&access, &msg("C1", "U-anyone", false, false)));
    }

    #[test]
    fn a_channel_user_restriction_is_enforced() {
        let access = access_with_channel("C1", vec!["U1"]);
        assert!(allows(&access, &msg("C1", "U1", false, false)));
        assert!(!allows(&access, &msg("C1", "U2", false, false)));
    }

    #[test]
    fn a_dm_is_allowed_only_for_listed_users() {
        let access = Access { dm: DmAccess { users: vec!["U1".into()] }, ..Access::default() };
        assert!(allows(&access, &msg("D1", "U1", false, true)));
        assert!(!allows(&access, &msg("D1", "U2", false, true)));
    }

    #[test]
    fn bots_are_denied_unless_allow_bots() {
        let mut access = access_with_channel("C1", vec![]);
        assert!(!allows(&access, &msg("C1", "U-bot", true, false)), "off by default");
        access.allow_bots = true;
        assert!(allows(&access, &msg("C1", "U-bot", true, false)), "on when allowed");
    }

    #[test]
    fn a_bot_in_an_unlisted_channel_is_still_denied() {
        let mut access = access_with_channel("C1", vec![]);
        access.allow_bots = true;
        assert!(!allows(&access, &msg("C2", "U-bot", true, false)));
    }

    #[test]
    fn a_bot_dm_rides_allow_bots_not_the_user_list() {
        let mut access = Access::default(); // no dm users
        assert!(!allows(&access, &msg("D1", "U-bot", true, true)), "off by default");
        access.allow_bots = true;
        assert!(allows(&access, &msg("D1", "U-bot", true, true)), "on when allowed");
    }

    #[test]
    fn the_loop_guard_caps_bot_turns_per_thread() {
        let mut guard = LoopGuard::new(2);
        assert!(guard.allow_bot_turn("T1"), "1st bot turn");
        assert!(guard.allow_bot_turn("T1"), "2nd bot turn");
        assert!(!guard.allow_bot_turn("T1"), "3rd is over the cap");
        assert!(!guard.allow_bot_turn("T1"), "stays capped");
    }

    #[test]
    fn the_loop_guard_counts_threads_independently() {
        let mut guard = LoopGuard::new(1);
        assert!(guard.allow_bot_turn("T1"));
        assert!(!guard.allow_bot_turn("T1"), "T1 capped");
        assert!(guard.allow_bot_turn("T2"), "T2 has its own budget");
    }

    #[test]
    fn access_defaults_to_deny_everything() {
        let access = toml::from_str::<Access>("").unwrap();
        assert!(!access.allow_bots);
        assert_eq!(access.bot_turn_limit, 3, "the loop cap defaults to 3");
        assert!(access.dm.users.is_empty());
        assert!(access.channels.is_empty());
    }

    #[test]
    fn access_parses_channels_dm_and_bots() {
        let access = toml::from_str::<Access>(
            r#"
            allow_bots = true
            bot_turn_limit = 5

            [dm]
            users = ["U1", "U2"]

            [channels.C0AAA]

            [channels.C0BBB]
            users = ["U9"]
            "#,
        )
        .unwrap();
        assert!(access.allow_bots);
        assert_eq!(access.bot_turn_limit, 5);
        assert_eq!(access.dm.users, vec!["U1", "U2"]);
        assert!(access.channels.contains_key("C0AAA"));
        assert!(access.channels["C0AAA"].users.is_empty(), "no per-channel users");
        assert_eq!(access.channels["C0BBB"].users, vec!["U9"]);
    }

    #[test]
    fn bot_turn_limit_alone_keeps_the_other_access_defaults() {
        let access = toml::from_str::<Access>("allow_bots = true\n").unwrap();
        assert!(access.allow_bots);
        assert_eq!(access.bot_turn_limit, 3, "untouched field keeps its default");
    }

    #[test]
    fn an_unknown_access_key_is_a_clear_error() {
        let err = toml::from_str::<Access>("allow_bot = true\n")
            .expect_err("an unknown access key does not parse");
        assert!(format!("{err:#}").contains("allow_bot"), "{err:#}");
    }
}
