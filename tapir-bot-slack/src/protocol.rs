//! The Socket Mode protocol, kept pure: given a raw text frame off the wire,
//! decide what to do — acknowledge the envelope, deliver an inbound mention,
//! or reconnect — without touching any transport. This split is what lets the
//! whole protocol run against synthetic payloads in tests.

use serde::Deserialize;
use tapir_bot_core::event::Inbound;

/// What one frame asks of the connection. Most frames set a single field; an
/// `events_api` envelope carrying a mention sets both `ack` and `inbound`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decision {
    /// The envelope id to acknowledge, if the frame was an ackable envelope.
    pub ack: Option<String>,
    /// The mention to hand the bot, if the envelope carried one.
    pub inbound: Option<Inbound>,
    /// Whether Slack asked us to drop this socket and open a fresh one.
    pub reconnect: bool,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    envelope_id: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    event: Option<Event>,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    channel: Option<String>,
    channel_type: Option<String>,
    subtype: Option<String>,
    user: Option<String>,
    bot_id: Option<String>,
    text: Option<String>,
    ts: Option<String>,
    thread_ts: Option<String>,
}

/// Decide what one text frame off the Socket Mode WebSocket asks of the
/// connection. Unknown envelope kinds are acked (so Slack stops redelivering)
/// but otherwise ignored; unparseable frames are a no-op.
pub fn handle_frame(frame: &str) -> Decision {
    let Ok(envelope) = serde_json::from_str::<Envelope>(frame) else {
        return Decision::default();
    };
    let mut decision = Decision::default();
    match envelope.kind.as_str() {
        // A disconnect carries no envelope id and needs no ack — just reopen.
        "disconnect" => decision.reconnect = true,
        // The Events API: ack always (Slack redelivers otherwise), and deliver
        // the mention if that is what it carried.
        "events_api" => {
            decision.ack = envelope.envelope_id;
            decision.inbound = envelope.payload.and_then(|p| p.event).and_then(inbound_from_event);
        }
        // `hello` is informational; everything else we ack and drop.
        "hello" => {}
        _ => decision.ack = envelope.envelope_id,
    }
    decision
}

/// Turn an event into an [`Inbound`] to consider: an `app_mention` (in a
/// channel), or a plain `message` in a DM (`channel_type == "im"`, no subtype —
/// DMs need no mention). Bot authorship is carried in `is_bot`, not dropped
/// here; the access policy decides. Edits/deletes (subtyped DM messages) and
/// everything else become `None`.
fn inbound_from_event(event: Event) -> Option<Inbound> {
    let channel_type = event.channel_type.as_deref();
    let is_dm = channel_type == Some("im");
    let is_mention = event.kind == "app_mention";
    let is_dm_message = event.kind == "message" && is_dm && event.subtype.is_none();
    // A non-mention reply inside a channel thread: a continuation candidate the
    // read loop only acts on for threads the bot is already in.
    let is_channel_reply = event.kind == "message"
        && matches!(channel_type, Some("channel") | Some("group"))
        && event.subtype.is_none()
        && event.thread_ts.is_some();
    if !is_mention && !is_dm_message && !is_channel_reply {
        return None;
    }
    let channel = event.channel?;
    let ts = event.ts?;
    let thread = event.thread_ts.unwrap_or_else(|| ts.clone());
    Some(Inbound {
        channel,
        ts,
        thread,
        user: event.user.unwrap_or_default(),
        text: event.text.unwrap_or_default(),
        is_bot: event.bot_id.is_some(),
        is_dm,
        continuation: is_channel_reply,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_mention_is_acked_and_delivered() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-1",
            "payload": {
                "event": {
                    "type": "app_mention",
                    "channel": "C1",
                    "user": "U1",
                    "text": "<@U0BOT> hello",
                    "ts": "1700000000.000100"
                }
            }
        }"#;
        let d = handle_frame(frame);
        assert_eq!(d.ack.as_deref(), Some("env-1"));
        assert!(!d.reconnect);
        let inbound = d.inbound.expect("a mention is delivered");
        assert_eq!(
            inbound,
            Inbound {
                channel: "C1".into(),
                ts: "1700000000.000100".into(),
                thread: "1700000000.000100".into(),
                user: "U1".into(),
                text: "<@U0BOT> hello".into(),
                is_bot: false,
                is_dm: false,
                continuation: false,
            }
        );
    }

    #[test]
    fn a_threaded_mention_replies_in_its_thread() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-2",
            "payload": { "event": {
                "type": "app_mention", "channel": "C1", "user": "U1",
                "text": "hi", "ts": "200.2", "thread_ts": "100.1"
            } }
        }"#;
        let inbound = handle_frame(frame).inbound.expect("delivered");
        assert_eq!(inbound.thread, "100.1", "the reply targets the existing thread");
        assert_eq!(inbound.ts, "200.2");
    }

    #[test]
    fn a_disconnect_asks_for_a_reconnect_without_an_ack() {
        let d = handle_frame(r#"{"type": "disconnect", "reason": "refresh"}"#);
        assert!(d.reconnect);
        assert!(d.ack.is_none());
        assert!(d.inbound.is_none());
    }

    #[test]
    fn a_bot_authored_mention_is_carried_with_is_bot() {
        // The protocol no longer drops bots — it flags them; the access policy
        // decides. (Previously this returned no inbound.)
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-3",
            "payload": { "event": {
                "type": "app_mention", "channel": "C1", "user": "U9", "bot_id": "B9",
                "text": "loop?", "ts": "1.1"
            } }
        }"#;
        let d = handle_frame(frame);
        assert_eq!(d.ack.as_deref(), Some("env-3"));
        let inbound = d.inbound.expect("a bot mention is carried");
        assert!(inbound.is_bot, "flagged as bot-authored");
        assert!(!inbound.is_dm);
    }

    #[test]
    fn a_dm_message_is_delivered_without_a_mention() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-4",
            "payload": { "event": {
                "type": "message", "channel_type": "im", "channel": "D1",
                "user": "U1", "text": "hi there", "ts": "1.1"
            } }
        }"#;
        let inbound = handle_frame(frame).inbound.expect("a DM delivers");
        assert!(inbound.is_dm);
        assert!(!inbound.is_bot);
        assert_eq!(inbound.channel, "D1");
        assert_eq!(inbound.text, "hi there");
    }

    #[test]
    fn a_dm_edit_or_delete_subtype_is_ignored() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-5",
            "payload": { "event": {
                "type": "message", "channel_type": "im", "subtype": "message_changed",
                "channel": "D1", "user": "U1", "text": "edited", "ts": "1.1"
            } }
        }"#;
        let d = handle_frame(frame);
        assert_eq!(d.ack.as_deref(), Some("env-5"), "acked");
        assert!(d.inbound.is_none(), "subtyped DM messages are not turns");
    }

    #[test]
    fn a_root_channel_message_without_a_mention_is_not_delivered() {
        // A plain channel message with no thread_ts (and no mention) is ignored
        // — continuation only applies to thread replies.
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-6",
            "payload": { "event": {
                "type": "message", "channel_type": "channel", "channel": "C1",
                "text": "hi", "ts": "1.1"
            } }
        }"#;
        let d = handle_frame(frame);
        assert_eq!(d.ack.as_deref(), Some("env-6"));
        assert!(d.inbound.is_none());
    }

    #[test]
    fn a_channel_thread_reply_is_delivered_as_a_continuation() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-7",
            "payload": { "event": {
                "type": "message", "channel_type": "channel", "channel": "C1",
                "user": "U1", "text": "and another thing", "ts": "200.2",
                "thread_ts": "100.1"
            } }
        }"#;
        let inbound = handle_frame(frame).inbound.expect("a thread reply delivers");
        assert!(inbound.continuation, "flagged as a continuation candidate");
        assert!(!inbound.is_dm);
        assert_eq!(inbound.thread, "100.1");
    }

    #[test]
    fn a_private_channel_thread_reply_is_also_a_continuation() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-8",
            "payload": { "event": {
                "type": "message", "channel_type": "group", "channel": "G1",
                "user": "U1", "text": "more", "ts": "2.2", "thread_ts": "1.1"
            } }
        }"#;
        let inbound = handle_frame(frame).inbound.expect("a private thread reply delivers");
        assert!(inbound.continuation);
    }

    #[test]
    fn a_channel_thread_reply_with_a_subtype_is_ignored() {
        let frame = r#"{
            "type": "events_api",
            "envelope_id": "env-9",
            "payload": { "event": {
                "type": "message", "channel_type": "channel", "subtype": "message_changed",
                "channel": "C1", "text": "edited", "ts": "2.2", "thread_ts": "1.1"
            } }
        }"#;
        assert!(handle_frame(frame).inbound.is_none());
    }

    #[test]
    fn hello_is_a_no_op_and_unknown_envelopes_are_acked() {
        let hello = handle_frame(r#"{"type": "hello", "num_connections": 1}"#);
        assert_eq!(hello, Decision::default());

        let unknown = handle_frame(r#"{"type": "slash_commands", "envelope_id": "env-5"}"#);
        assert_eq!(unknown.ack.as_deref(), Some("env-5"));
        assert!(unknown.inbound.is_none());
    }

    #[test]
    fn garbage_is_a_no_op() {
        assert_eq!(handle_frame("not json at all {"), Decision::default());
    }
}
