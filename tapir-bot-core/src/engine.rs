//! The backend-neutral bot engine. It admits an inbound message
//! ([`Engine::should_handle`]) and processes it ([`Engine::handle`]): a
//! `!`-command answered directly, or a model turn streamed through a
//! [`ReplySink`]. Conversation memory, the model override, and tool execution
//! live here; the access policy and the bot loop cap are passed in by the
//! backend (which owns that config), and a backend supplies the I/O.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use tapir::runtime::Runtime;
use tapir::store::FileStore;

use crate::access::{Access, LoopGuard};
use crate::backend::ReplySink;
use crate::config::{Config, ToolMode};
use crate::event::Inbound;
#[cfg(feature = "sandbox")]
use crate::tools::build_sandbox_manager;
use crate::tools::{HostTools, Tools};
use crate::{commands, memory, meta};

/// The resolved model settings a turn runs on.
pub struct AgentSettings {
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
}

/// The shared, backend-neutral engine. Built once from a [`Config`] and shared
/// (Arc) across the backend's per-turn tasks.
pub struct Engine {
    rt: Runtime,
    settings: AgentSettings,
    memory_dir: PathBuf,
    /// The data dir: conversation transcripts under `sessions/`, per-conversation
    /// metadata (owner, model override) under `meta/`.
    data_dir: PathBuf,
    /// How a turn's tools execute (text-only, in the pod, or per-channel
    /// container).
    tools: Tools,
    /// The skills notice (enumerated skills) appended to tool-aware prompts.
    skills_notice: Option<String>,
}

impl Engine {
    /// Build the engine from a config. `skills_dir` is the repo `skills/` tree
    /// (provisioned into each tool workspace); pass `None` to disable skills.
    /// Resolves the model and validates the provider's API key up front, so a
    /// misconfigured bot fails at startup rather than mid-turn.
    pub fn from_config(config: Config, skills_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let provider = config.agent.provider;
        let model = resolve_model(
            config.agent.model.as_deref(),
            tapir::catalog::default_model(&provider).map(|m| m.id),
            &provider,
        )?;
        let var = tapir::providers::env_var(&provider);
        require_provider_key(var, var.and_then(|v| std::env::var(v).ok()), &provider)?;
        let settings =
            AgentSettings { provider, model, system_prompt: config.agent.system_prompt };

        let data_dir = config.storage.dir;
        let memory_dir = config.storage.memory_dir.unwrap_or_else(|| data_dir.clone());
        let repo_skills = skills_dir.filter(|p| p.is_dir());
        let skills_notice = repo_skills.as_deref().and_then(crate::tools::skills_notice);
        let tools = match config.agent.tools {
            ToolMode::Host => Tools::Host(Arc::new(HostTools::new(&data_dir, repo_skills))),
            ToolMode::None => Tools::None,
            #[cfg(feature = "sandbox")]
            ToolMode::Sandbox => {
                Tools::Sandbox(build_sandbox_manager(&config.sandbox, &data_dir, repo_skills))
            }
            #[cfg(not(feature = "sandbox"))]
            ToolMode::Sandbox => anyhow::bail!(
                "[agent].tools = \"sandbox\" needs the `sandbox` feature; \
                 rebuild tapir-bot with --features sandbox"
            ),
        };

        // Conversation history persists under <data_dir>/sessions, replayed per
        // conversation on each turn.
        let store = Arc::new(FileStore::new(data_dir.join("sessions"), data_dir.clone()));
        let rt = Runtime::builder().store(store).build();

        Ok(Self { rt, settings, memory_dir, data_dir, tools, skills_notice })
    }

    /// Log the resolved configuration and start background maintenance (the idle
    /// sandbox reaper). Call once before the backend's loop.
    pub fn start(&self) {
        let tools_mode = match self.tools {
            Tools::None => "none",
            Tools::Host(_) => "host",
            #[cfg(feature = "sandbox")]
            Tools::Sandbox(_) => "sandbox",
        };
        tracing::info!(
            provider = %self.settings.provider,
            model = %self.settings.model,
            data_dir = %self.data_dir.display(),
            memory_dir = %self.memory_dir.display(),
            tools = %tools_mode,
            "agent configured"
        );
        #[cfg(feature = "sandbox")]
        if let Tools::Sandbox(manager) = &self.tools {
            let manager = manager.clone();
            tracing::info!("tool sandbox enabled");
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    if let Err(error) = manager.reap_idle().await {
                        tracing::warn!(error = format!("{error:#}"), "sandbox reap failed");
                    }
                }
            });
        }
    }

    /// Decide whether to answer `inbound`: never the bot itself (`bot_id`), then
    /// the `access` policy, then — for a channel thread reply — the continuation
    /// gate (skip mentions, which the mention path handles, and threads the bot
    /// isn't in), and finally the per-thread bot loop cap (`loop_guard`). The
    /// backend owns the access config and the loop guard and passes them in.
    /// Drops are logged.
    pub async fn should_handle(
        &self,
        access: &Access,
        loop_guard: &mut LoopGuard,
        bot_id: &str,
        inbound: &Inbound,
    ) -> bool {
        if inbound.user == bot_id {
            return false;
        }
        if !crate::access::allows(access, inbound) {
            tracing::debug!(
                channel = %inbound.channel,
                user = %inbound.user,
                is_dm = inbound.is_dm,
                is_bot = inbound.is_bot,
                "dropped by access policy"
            );
            return false;
        }
        if inbound.continuation {
            // A mention is the mention path's job — skipping it here dedupes the
            // message echo.
            if mentions_bot(&inbound.text, bot_id) {
                return false;
            }
            // A `!`-command is explicit intent — handle it in any allowed
            // thread. Ordinary chatter only continues a thread the bot is
            // already in (has saved history for).
            let stripped = strip_mention(&inbound.text, bot_id);
            if commands::parse(&stripped).is_none() {
                let id = memory::conversation_id(inbound);
                if self.rt.store().load(&id).await.is_err() {
                    tracing::debug!(%id, "thread not known; ignoring continuation");
                    return false;
                }
            }
        }
        if inbound.is_bot && !loop_guard.allow_bot_turn(&inbound.thread) {
            tracing::info!(thread = %inbound.thread, "bot loop cap reached; dropping");
            return false;
        }
        true
    }

    /// Process one admitted message, posting the reply through `sink`: a
    /// `!`-command answered directly (no model turn, no memory), or a streamed
    /// model turn that is then persisted. `bot_id` strips the leading mention.
    pub async fn handle(
        &self,
        bot_id: &str,
        inbound: &Inbound,
        sink: &mut dyn ReplySink,
    ) -> anyhow::Result<()> {
        let stripped = strip_mention(&inbound.text, bot_id);

        // A `!`-command is answered directly — no model turn, no memory.
        if let Some(command) = commands::parse(&stripped) {
            return self.run_command(inbound, command, sink).await;
        }

        let id = memory::conversation_id(inbound);
        let prompt = if stripped.is_empty() {
            "Greet the user briefly and offer to help.".to_string()
        } else {
            stripped
        };

        // Record the creator on the first turn (the owner of `!model`/`!forget`),
        // and pick up this conversation's model override if one was set.
        let mut meta = meta::load(&self.data_dir, &id).await;
        if meta.owner.is_none() {
            meta.owner = Some(inbound.user.clone());
            let _ = meta::save(&self.data_dir, &id, &meta).await;
        }
        let (provider, model) = match &meta.model {
            Some(spec) => meta::split_model_spec(spec, &self.settings.provider),
            None => (self.settings.provider.clone(), self.settings.model.clone()),
        };

        // Inject durable facts (read fresh so edits apply without a restart).
        let (global, per_channel) = memory::read_facts(&self.memory_dir, &inbound.channel).await;
        let system =
            memory::assemble_prompt(self.settings.system_prompt.as_deref(), &global, &per_channel);

        let reply = self
            .run_turn(&provider, &model, &id, system.as_deref(), &prompt, &inbound.channel, sink)
            .await?;

        // Persist the turn so the next message in this conversation remembers
        // it. Best-effort: a storage hiccup must not fail a delivered reply.
        self.persist_turn(&id, &prompt, &reply).await;
        Ok(())
    }

    /// Run a `!`-command and post its reply. No model turn, no memory write.
    async fn run_command(
        &self,
        inbound: &Inbound,
        command: commands::Command,
        sink: &mut dyn ReplySink,
    ) -> anyhow::Result<()> {
        use commands::Command;

        let text = match command {
            Command::Providers => {
                let active: Vec<&str> = self
                    .rt
                    .providers()
                    .iter()
                    .map(|p| p.id())
                    .filter(|id| provider_active(id))
                    .collect();
                commands::render_providers(&active)
            }
            Command::Models => {
                // provider/model for every active provider.
                let rows: Vec<(String, String)> = self
                    .rt
                    .providers()
                    .iter()
                    .filter(|p| provider_active(p.id()))
                    .flat_map(|p| p.models().into_iter().map(|m| (p.id().to_string(), m.id)))
                    .collect();
                let refs: Vec<(&str, &str)> = rows
                    .iter()
                    .map(|(provider, model)| (provider.as_str(), model.as_str()))
                    .collect();
                commands::render_models(&refs)
            }
            Command::Help => commands::render_help(),
            Command::Model(arg) => self.handle_model(inbound, arg).await?,
            Command::Forget => self.handle_forget(inbound).await?,
            Command::Unknown(name) => commands::render_unknown(&name),
        };

        sink.update(&text, true).await?;
        Ok(())
    }

    /// `!model` — show or set this conversation's model (owner-only).
    async fn handle_model(
        &self,
        inbound: &Inbound,
        arg: Option<String>,
    ) -> anyhow::Result<String> {
        let id = memory::conversation_id(inbound);
        let mut meta = meta::load(&self.data_dir, &id).await;
        if let Some(denied) = owner_denied(&meta, &inbound.user) {
            return Ok(denied);
        }
        match arg {
            None => {
                let current = meta.model.clone().unwrap_or_else(|| {
                    format!("{}/{} (default)", self.settings.provider, self.settings.model)
                });
                Ok(format!(
                    "Model for this conversation: `{current}`.\nSet it with `!model provider/model`."
                ))
            }
            Some(spec) => {
                let (provider, model) = meta::split_model_spec(&spec, &self.settings.provider);
                if tapir::catalog::models::get(&provider, &model).is_none() {
                    return Ok(format!("Unknown model `{provider}/{model}` — see `!models`."));
                }
                meta.model = Some(format!("{provider}/{model}"));
                meta::save(&self.data_dir, &id, &meta).await?;
                Ok(format!("Model set to `{provider}/{model}` for this conversation."))
            }
        }
    }

    /// `!forget` — reset this conversation's history and model override
    /// (owner-only).
    async fn handle_forget(&self, inbound: &Inbound) -> anyhow::Result<String> {
        let id = memory::conversation_id(inbound);
        let meta = meta::load(&self.data_dir, &id).await;
        if let Some(denied) = owner_denied(&meta, &inbound.user) {
            return Ok(denied);
        }
        meta::forget(&self.data_dir, &id).await?;
        Ok("Done — this conversation's history and model override were reset.".into())
    }

    /// Append the user and assistant messages to the conversation's history.
    async fn persist_turn(&self, id: &str, user: &str, assistant: &str) {
        use tapir::message::Role;
        use tapir::store::Entry;

        let store = self.rt.store();
        for entry in [
            Entry::Message { role: Role::User, text: user.to_string() },
            Entry::Message { role: Role::Assistant, text: assistant.to_string() },
        ] {
            if let Err(error) = store.append(id, &entry).await {
                tracing::warn!(error = format!("{error:#}"), %id, "persisting the turn failed");
            }
        }
    }

    /// Run one model turn for conversation `id`, streaming the reply text into
    /// `sink` (each delta is the full accumulated text; the final update marks
    /// `done`), and return the final text. Prior history is replayed. Tools run
    /// per `self.tools`: not at all (text-only), in the pod (host), or in the
    /// channel's container (sandbox); `channel` keys the tool workspace.
    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        provider: &str,
        model: &str,
        id: &str,
        system: Option<&str>,
        prompt: &str,
        channel: &str,
        sink: &mut dyn ReplySink,
    ) -> anyhow::Result<String> {
        use tapir::agent::ModelRef;
        use tapir::prelude::{Input, TurnEvent};
        use tapir::runtime::SessionOptions;

        let tools_enabled = !matches!(self.tools, Tools::None);

        // Host mode needs its per-channel lock and prepared workspace before the
        // session is built (the workspace is the cwd). Holding the guard for the
        // whole turn serializes the channel's host turns, like the sandbox lease.
        let host_turn = match &self.tools {
            Tools::Host(host) => {
                let guard = host.lock(channel).lock_owned().await;
                let workspace = host.prepare(channel)?;
                Some((guard, workspace))
            }
            _ => None,
        };

        // cwd: text-only at `.`, sandbox at the container's /workspace, host at
        // the channel's prepared workspace. Tool-aware prompt when tools are on.
        let cwd = match &self.tools {
            Tools::None => std::path::PathBuf::from("."),
            Tools::Host(_) => host_turn.as_ref().expect("host turn prepared").1.clone(),
            #[cfg(feature = "sandbox")]
            Tools::Sandbox(_) => std::path::PathBuf::from(tapir_sandbox::GUEST_WORKSPACE),
        };
        let mut opts = SessionOptions::new(cwd)
            .prompt(prompt_spec_for(tools_enabled, system, self.skills_notice.as_deref()));
        if !tools_enabled {
            opts = opts.only(Vec::<String>::new()); // no tools
        }

        // Replay prior history into a fresh session when this conversation
        // already exists; a brand-new conversation simply starts empty.
        let mut agent = self.rt.session_with(opts);
        if let Ok(entries) = self.rt.store().load(id).await {
            tapir::store::replay(&mut agent, &entries);
        }
        agent.set_model(Some(ModelRef { provider: provider.to_string(), id: model.to_string() }));

        // Sandbox mode: point the agent's tools at the channel's container and
        // keep it from being reaped for the turn (the `_busy` guard lives until
        // the function returns). Host mode keeps the runtime's default local ops
        // (tools run in the pod); its lock guard in `host_turn` is held the same
        // way. Text-only has neither.
        #[cfg(feature = "sandbox")]
        let _busy = match &self.tools {
            Tools::Sandbox(manager) => {
                let handle = manager.channel(channel);
                let busy = handle.busy();
                let lease = handle.lease().await.context("leasing the channel sandbox")?;
                agent.set_boundary(Some(lease.boundary()));
                agent.set_exec_ops(lease.exec_ops());
                agent.set_fs_ops(lease.fs_ops());
                Some(busy)
            }
            _ => None,
        };

        let mut rx = agent.run(Input::text(prompt.to_string()));
        let mut text = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                TurnEvent::Text { delta } => {
                    text.push_str(&delta);
                    sink.update(&text, false).await?;
                }
                TurnEvent::Done => break,
                TurnEvent::Error { message } => anyhow::bail!("model turn failed: {message}"),
                _ => {}
            }
        }

        // A turn that produced no text still gets a visible reply.
        let final_text =
            if text.trim().is_empty() { "(the model returned no text)".to_string() } else { text };
        sink.update(&final_text, true).await?;
        Ok(final_text)
    }
}

/// A denial message when `user` isn't the conversation's owner, or `None` when
/// they may proceed (they are the owner, or no owner is recorded yet).
fn owner_denied(meta: &meta::ConversationMeta, user: &str) -> Option<String> {
    match &meta.owner {
        Some(owner) if owner != user => {
            Some(format!("Only <@{owner}>, who started this conversation, can do that."))
        }
        _ => None,
    }
}

/// Whether the provider's API key is present (and non-blank) in the
/// environment — i.e. the provider is usable.
fn provider_active(id: &str) -> bool {
    tapir::providers::env_var(id)
        .and_then(|var| std::env::var(var).ok())
        .is_some_and(|value| !value.trim().is_empty())
}

/// Whether `text` mentions the bot (`<@bot_id>` anywhere in it).
fn mentions_bot(text: &str, bot_id: &str) -> bool {
    text.contains(&format!("<@{bot_id}>"))
}

/// Strip a leading `<@bot_id>` mention (and the whitespace around it) from
/// `text`, leaving the user's actual message. A mention elsewhere in the text,
/// or its absence, leaves the text intact (just trimmed).
fn strip_mention(text: &str, bot_id: &str) -> String {
    let token = format!("<@{bot_id}>");
    match text.trim_start().strip_prefix(&token) {
        Some(rest) => rest.trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// The prompt spec for a turn. With tools, append the skills notice plus the
/// persona/memory to the default (tool-aware) prompt; without, replace the body
/// with the persona/memory (a text-only prompt).
fn prompt_spec_for(
    tools_enabled: bool,
    system: Option<&str>,
    skills_notice: Option<&str>,
) -> tapir::runtime::PromptSpec {
    use tapir::runtime::PromptSpec;
    if tools_enabled {
        let mut append: Vec<String> = Vec::new();
        append.extend(skills_notice.map(String::from));
        append.extend(system.map(String::from));
        PromptSpec { append: (!append.is_empty()).then_some(append), ..Default::default() }
    } else {
        PromptSpec { custom: system.map(String::from), ..Default::default() }
    }
}

/// Resolve the model id: the configured one, else the provider's catalog
/// default. A provider with neither is a clear error.
fn resolve_model(
    configured: Option<&str>,
    catalog_default: Option<&str>,
    provider: &str,
) -> anyhow::Result<String> {
    configured
        .or(catalog_default)
        .map(String::from)
        .with_context(|| format!("no model for provider {provider:?}: set [agent].model"))
}

/// Require the provider's API key to be present in the environment. An unknown
/// provider (no known env var) and a blank/missing key are both clear errors.
fn require_provider_key(
    var: Option<&str>,
    value: Option<String>,
    provider: &str,
) -> anyhow::Result<()> {
    let var = var.with_context(|| format!("unknown provider {provider:?}"))?;
    value.filter(|v| !v.trim().is_empty()).with_context(|| {
        format!("{var} must be set for provider {provider:?} (export it from your secret store)")
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mentions_bot, prompt_spec_for, require_provider_key, resolve_model, strip_mention};

    #[test]
    fn prompt_spec_is_tool_aware_when_tools_are_enabled() {
        // Tools on: append the skills notice + persona to the default prompt,
        // leaving the tool-aware body in place (no custom override).
        let spec = prompt_spec_for(true, Some("be nice"), Some("skills here"));
        let append = spec.append.expect("tool-aware prompts append");
        assert_eq!(append, vec!["skills here".to_string(), "be nice".to_string()]);
        assert!(spec.custom.is_none(), "tool-aware keeps the default body");

        // Tools off: replace the body with the persona (text-only).
        let spec = prompt_spec_for(false, Some("be nice"), Some("skills here"));
        assert_eq!(spec.custom.as_deref(), Some("be nice"));
        assert!(spec.append.is_none(), "text-only does not append the skills notice");
    }

    #[test]
    fn mentions_bot_detects_the_mention_anywhere() {
        assert!(mentions_bot("<@U0BOT> hi", "U0BOT"));
        assert!(mentions_bot("hey <@U0BOT> there", "U0BOT"), "mid-text counts");
        assert!(!mentions_bot("just talking", "U0BOT"));
        assert!(!mentions_bot("<@U0OTHER> hi", "U0BOT"), "another user's mention");
    }

    #[test]
    fn a_leading_mention_is_stripped() {
        assert_eq!(strip_mention("<@U0BOT> hello world", "U0BOT"), "hello world");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(strip_mention("   <@U0BOT>    hi  ", "U0BOT"), "hi");
    }

    #[test]
    fn a_bare_mention_strips_to_empty() {
        assert_eq!(strip_mention("<@U0BOT>", "U0BOT"), "");
    }

    #[test]
    fn text_without_a_leading_mention_is_left_intact() {
        assert_eq!(strip_mention("just talking", "U0BOT"), "just talking");
        // A mention in the middle is not the bot being addressed up front.
        assert_eq!(strip_mention("hey <@U0BOT> there", "U0BOT"), "hey <@U0BOT> there");
    }

    #[test]
    fn the_configured_model_wins_over_the_catalog_default() {
        assert_eq!(
            resolve_model(Some("claude-x"), Some("claude-default"), "anthropic").unwrap(),
            "claude-x"
        );
        assert_eq!(
            resolve_model(None, Some("claude-default"), "anthropic").unwrap(),
            "claude-default"
        );
    }

    #[test]
    fn no_model_anywhere_is_a_clear_error() {
        let err = resolve_model(None, None, "weird").expect_err("no model is an error");
        assert!(format!("{err:#}").contains("weird"), "{err:#}");
    }

    #[test]
    fn a_missing_provider_key_names_the_variable() {
        let err = require_provider_key(Some("ANTHROPIC_API_KEY"), None, "anthropic")
            .expect_err("missing key is an error");
        assert!(format!("{err:#}").contains("ANTHROPIC_API_KEY"), "{err:#}");

        let err = require_provider_key(Some("ANTHROPIC_API_KEY"), Some("  ".into()), "anthropic")
            .expect_err("blank key is an error");
        assert!(format!("{err:#}").contains("ANTHROPIC_API_KEY"), "{err:#}");
    }
}
