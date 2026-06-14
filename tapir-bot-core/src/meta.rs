//! Per-conversation metadata kept beside the transcripts: who created the
//! conversation (the owner, for command authorization) and an optional model
//! override set by `!model`. Stored as `<data_dir>/meta/<id>.toml`, separate
//! from the message history.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A conversation's metadata. Both fields are absent until set.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationMeta {
    /// The user id that created the conversation — only they may `!model`/
    /// `!forget` it.
    pub owner: Option<String>,
    /// A `provider/model` override for this conversation; `None` uses the
    /// configured default.
    pub model: Option<String>,
}

fn meta_path(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("meta").join(format!("{id}.toml"))
}

/// Load a conversation's metadata; a missing or unreadable file is the default
/// (no owner, no override).
pub async fn load(data_dir: &Path, id: &str) -> ConversationMeta {
    match tokio::fs::read_to_string(meta_path(data_dir, id)).await {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => ConversationMeta::default(),
    }
}

/// Persist a conversation's metadata.
pub async fn save(data_dir: &Path, id: &str, meta: &ConversationMeta) -> anyhow::Result<()> {
    let path = meta_path(data_dir, id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, toml::to_string(meta)?).await?;
    Ok(())
}

/// Forget a conversation: delete its transcript(s) under `sessions/` and its
/// metadata. Idempotent — missing files are fine.
pub async fn forget(data_dir: &Path, id: &str) -> anyhow::Result<()> {
    let sessions = data_dir.join("sessions");
    let suffix = format!("_{id}.jsonl");
    if let Ok(mut entries) = tokio::fs::read_dir(&sessions).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_name().to_str().is_some_and(|name| name.ends_with(&suffix)) {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    let _ = tokio::fs::remove_file(meta_path(data_dir, id)).await;
    Ok(())
}

/// Split a `!model` argument into `(provider, model)`: `provider/model` as
/// given, or a bare `model` against `default_provider`.
pub fn split_model_spec(spec: &str, default_provider: &str) -> (String, String) {
    match spec.split_once('/') {
        Some((provider, model)) => (provider.trim().to_string(), model.trim().to_string()),
        None => (default_provider.to_string(), spec.trim().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_uses_the_given_provider_or_the_default() {
        assert_eq!(
            split_model_spec("anthropic/claude-x", "openai"),
            ("anthropic".into(), "claude-x".into())
        );
        assert_eq!(
            split_model_spec("claude-x", "anthropic"),
            ("anthropic".into(), "claude-x".into()),
            "bare model uses the default provider"
        );
    }

    #[test]
    fn meta_round_trips_through_toml() {
        let meta = ConversationMeta {
            owner: Some("U1".into()),
            model: Some("anthropic/claude-x".into()),
        };
        let text = toml::to_string(&meta).unwrap();
        let back: ConversationMeta = toml::from_str(&text).unwrap();
        assert_eq!(meta, back);

        // An empty meta serializes and parses back to the default.
        let empty = toml::to_string(&ConversationMeta::default()).unwrap();
        assert_eq!(toml::from_str::<ConversationMeta>(&empty).unwrap(), ConversationMeta::default());
    }
}
