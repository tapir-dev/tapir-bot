//! The Slack Web API surface the runtime needs, over async reqwest. Two
//! tokens: `apps.connections.open` authenticates with the app-level token
//! (`xapp-…`); everything else with the bot token (`xoxb-…`). Response-status
//! checking is a pure helper so it can be unit-tested; the live calls are
//! exercised manually.

use anyhow::Context;
use serde_json::{Value, json};

/// Base URL for Web API methods.
const API_BASE: &str = "https://slack.com/api/";

/// A Web API client holding the two tokens and a reused HTTP client.
pub struct Client {
    http: reqwest::Client,
    app_token: String,
    bot_token: String,
}

impl Client {
    pub fn new(app_token: String, bot_token: String) -> Self {
        Self { http: reqwest::Client::new(), app_token, bot_token }
    }

    /// The bot's own user id, via `auth.test` — used to strip the leading
    /// mention from incoming text.
    pub async fn bot_user_id(&self) -> anyhow::Result<String> {
        let value = self.call("auth.test", &self.bot_token, json!({})).await?;
        value
            .get("user_id")
            .and_then(Value::as_str)
            .map(String::from)
            .context("auth.test response missing user_id")
    }

    /// Open a Socket Mode connection and return its `wss://` URL, via
    /// `apps.connections.open` (app-level token).
    pub async fn open_connection(&self) -> anyhow::Result<String> {
        let value = self.call("apps.connections.open", &self.app_token, json!({})).await?;
        value
            .get("url")
            .and_then(Value::as_str)
            .map(String::from)
            .context("apps.connections.open response missing url")
    }

    /// Post a message in a thread under `thread_ts`; returns the new message's
    /// `ts` (so it can be edited as the reply streams in). `text` is always sent
    /// (Slack uses it for notifications and as the fallback); `blocks`, when
    /// present, drives the rich rendering.
    pub async fn post_message(
        &self,
        channel: &str,
        thread_ts: &str,
        text: &str,
        blocks: Option<&[Value]>,
    ) -> anyhow::Result<String> {
        let mut body = json!({ "channel": channel, "thread_ts": thread_ts, "text": text });
        with_blocks(&mut body, blocks);
        let value = self.call("chat.postMessage", &self.bot_token, body).await?;
        value
            .get("ts")
            .and_then(Value::as_str)
            .map(String::from)
            .context("chat.postMessage response missing ts")
    }

    /// Replace a previously posted message (the streaming edit). `text` is the
    /// fallback; `blocks`, when present, replaces the rich rendering.
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
        blocks: Option<&[Value]>,
    ) -> anyhow::Result<()> {
        let mut body = json!({ "channel": channel, "ts": ts, "text": text });
        with_blocks(&mut body, blocks);
        self.call("chat.update", &self.bot_token, body).await?;
        Ok(())
    }

    /// Add reaction `name` (without colons, e.g. `eyes`) to a message.
    /// Tolerates `already_reacted` so a retry is harmless.
    pub async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> anyhow::Result<()> {
        self.react("reactions.add", channel, ts, name, "already_reacted").await
    }

    /// Remove reaction `name` from a message. Tolerates `no_reaction`.
    pub async fn remove_reaction(&self, channel: &str, ts: &str, name: &str) -> anyhow::Result<()> {
        self.react("reactions.remove", channel, ts, name, "no_reaction").await
    }

    /// A reaction call that swallows one benign error code (so lifecycle
    /// reactions never fail the turn over a double-add or a missing reaction).
    async fn react(
        &self,
        method: &str,
        channel: &str,
        ts: &str,
        name: &str,
        ignorable: &str,
    ) -> anyhow::Result<()> {
        let url = format!("{API_BASE}{method}");
        let body = json!({ "channel": channel, "timestamp": ts, "name": name });
        let text = self
            .http
            .post(&url)
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("calling {method}"))?
            .text()
            .await
            .with_context(|| format!("reading {method} response"))?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {method} response"))?;
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let code = value.get("error").and_then(Value::as_str).unwrap_or("unknown_error");
        if code == ignorable {
            return Ok(());
        }
        anyhow::bail!("{method} failed: {code}");
    }

    /// POST a JSON body to `method` with `token` and return the parsed,
    /// ok-checked response.
    async fn call(&self, method: &str, token: &str, body: Value) -> anyhow::Result<Value> {
        let url = format!("{API_BASE}{method}");
        let text = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("calling {method}"))?
            .text()
            .await
            .with_context(|| format!("reading {method} response"))?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {method} response"))?;
        check_ok(method, value)
    }
}

/// Add a `blocks` field to a `chat.postMessage`/`chat.update` body when blocks
/// are supplied. An empty slice is treated as "no blocks" so the message falls
/// back to plain `text`.
fn with_blocks(body: &mut Value, blocks: Option<&[Value]>) {
    if let Some(blocks) = blocks.filter(|b| !b.is_empty())
        && let Some(map) = body.as_object_mut()
    {
        map.insert("blocks".into(), Value::Array(blocks.to_vec()));
    }
}

/// Pass a Slack response through when `ok` is true, else turn it into an error
/// naming the method and Slack's `error` code.
fn check_ok(method: &str, value: Value) -> anyhow::Result<Value> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(value);
    }
    let code = value.get("error").and_then(Value::as_str).unwrap_or("unknown_error");
    anyhow::bail!("{method} failed: {code}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ok_response_passes_through() {
        let value = check_ok("auth.test", json!({"ok": true, "user_id": "U0BOT"}))
            .expect("ok passes through");
        assert_eq!(value["user_id"], "U0BOT");
    }

    #[test]
    fn a_failed_response_names_the_method_and_code() {
        let err = check_ok("chat.postMessage", json!({"ok": false, "error": "channel_not_found"}))
            .expect_err("ok:false is an error");
        let msg = format!("{err:#}");
        assert!(msg.contains("chat.postMessage"), "{msg}");
        assert!(msg.contains("channel_not_found"), "{msg}");
    }

    #[test]
    fn with_blocks_adds_blocks_only_when_non_empty() {
        let mut body = json!({ "channel": "C", "text": "hi" });
        with_blocks(&mut body, None);
        assert!(body.get("blocks").is_none(), "no blocks when None");

        with_blocks(&mut body, Some(&[]));
        assert!(body.get("blocks").is_none(), "no blocks when empty");

        let blocks = vec![json!({ "type": "divider" })];
        with_blocks(&mut body, Some(&blocks));
        assert_eq!(body["blocks"], json!([{ "type": "divider" }]));
        assert_eq!(body["text"], "hi", "text fallback stays");
    }

    #[test]
    fn a_missing_ok_is_treated_as_failure() {
        let err = check_ok("auth.test", json!({})).expect_err("no ok is a failure");
        assert!(format!("{err:#}").contains("unknown_error"), "{err:#}");
    }
}
