//! Conversation memory helpers: the per-conversation id used to key stored
//! history, the durable `MEMORY.md` facts files, and assembling the system
//! prompt from the persona plus those facts.

use std::path::Path;

use crate::event::Inbound;

/// Read the durable facts for `channel`: the global `<data_dir>/MEMORY.md` and
/// the per-channel `<data_dir>/memory/<channel>.md`. Read fresh each turn (so
/// edits apply without a restart); a missing file is empty.
pub async fn read_facts(data_dir: &Path, channel: &str) -> (String, String) {
    let global = tokio::fs::read_to_string(data_dir.join("MEMORY.md")).await.unwrap_or_default();
    let per_channel = tokio::fs::read_to_string(data_dir.join("memory").join(format!("{channel}.md")))
        .await
        .unwrap_or_default();
    (global, per_channel)
}

/// Assemble the system prompt from the optional persona and the durable facts.
/// The facts go under a `# Memory` heading. Empty parts are omitted; when
/// nothing is present, returns `None` so the model uses its default prompt.
pub fn assemble_prompt(persona: Option<&str>, global: &str, per_channel: &str) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    if let Some(persona) = persona.map(str::trim).filter(|s| !s.is_empty()) {
        sections.push(persona.to_string());
    }
    let facts: Vec<&str> =
        [global, per_channel].into_iter().map(str::trim).filter(|f| !f.is_empty()).collect();
    if !facts.is_empty() {
        let mut memory =
            String::from("# Memory\n\nDurable facts — treat as context:\n\n");
        memory.push_str(&facts.join("\n\n"));
        sections.push(memory);
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// The conversation id an inbound belongs to. A DM is one rolling conversation
/// keyed by its channel (DM messages aren't threaded); a channel message is
/// keyed per thread, so each thread is its own conversation.
pub fn conversation_id(inbound: &Inbound) -> String {
    if inbound.is_dm {
        format!("dm:{}", inbound.channel)
    } else {
        format!("ch:{}:{}", inbound.channel, inbound.thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(channel: &str, thread: &str, is_dm: bool) -> Inbound {
        Inbound {
            channel: channel.into(),
            ts: "9.9".into(),
            thread: thread.into(),
            user: "U1".into(),
            text: "hi".into(),
            is_bot: false,
            is_dm,
            continuation: false,
        }
    }

    #[test]
    fn a_dm_is_keyed_by_channel_so_messages_share_one_conversation() {
        // Two DM messages with different ts (DMs aren't threaded) → same id.
        let a = conversation_id(&msg("D1", "100.1", true));
        let b = conversation_id(&msg("D1", "200.2", true));
        assert_eq!(a, b);
        assert_eq!(a, "dm:D1");
    }

    #[test]
    fn a_channel_is_keyed_per_thread() {
        let t1 = conversation_id(&msg("C1", "100.1", false));
        let t1_again = conversation_id(&msg("C1", "100.1", false));
        let t2 = conversation_id(&msg("C1", "200.2", false));
        assert_eq!(t1, t1_again, "same thread → same conversation");
        assert_ne!(t1, t2, "different threads → different conversations");
        assert_eq!(t1, "ch:C1:100.1");
    }

    #[test]
    fn assemble_uses_the_persona_alone_when_there_are_no_facts() {
        assert_eq!(assemble_prompt(Some("You are Tapir."), "", ""), Some("You are Tapir.".into()));
    }

    #[test]
    fn assemble_includes_a_memory_section_with_the_facts() {
        let out = assemble_prompt(Some("Persona."), "Global fact.", "Channel fact.").unwrap();
        assert!(out.starts_with("Persona."), "{out}");
        assert!(out.contains("# Memory"), "{out}");
        assert!(out.contains("Global fact."), "{out}");
        assert!(out.contains("Channel fact."), "{out}");
    }

    #[test]
    fn assemble_omits_empty_parts() {
        // No persona, only a channel fact: a Memory section, no leading blank.
        let out = assemble_prompt(None, "   ", "Only channel.").unwrap();
        assert!(out.starts_with("# Memory"), "{out}");
        assert!(out.contains("Only channel."));
        assert!(!out.contains("Global"), "{out}");
    }

    #[test]
    fn assemble_is_none_when_everything_is_empty() {
        assert_eq!(assemble_prompt(None, "", "  \n "), None);
        assert_eq!(assemble_prompt(Some("   "), "", ""), None);
    }
}
