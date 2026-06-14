//! How a turn's tools execute, backend-neutral: text-only, in the bot's own
//! process (host), or — behind the `sandbox` feature — in an isolated
//! per-channel container. Also the skills provisioning shared by host and
//! sandbox, and the skills notice the engine appends to a tool-aware prompt.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;

/// How a turn's tools execute. The single thing the turn path branches on.
pub enum Tools {
    /// Text-only: no tools run anywhere.
    None,
    /// Tools run in the bot's own process at a per-channel workspace (the pod
    /// is the isolation boundary).
    Host(Arc<HostTools>),
    /// Tools run in the channel's isolated container (the `sandbox` feature).
    #[cfg(feature = "sandbox")]
    Sandbox(Arc<tapir_sandbox::SandboxManager>),
}

/// Host tool execution: tools run in the bot's own process at a per-channel
/// working dir under `<data_dir>/workspaces/<channel>`, serialized per channel
/// (a lock mirroring the sandbox's busy/lease). Skills are provisioned into the
/// workspace the same way the sandbox does.
pub struct HostTools {
    /// `<data_dir>/workspaces` — per-channel working dirs live under here.
    root: PathBuf,
    /// The repo `skills/` dir, provisioned into each workspace when present.
    repo_skills: Option<PathBuf>,
    /// `<data_dir>/skills` — per-channel skill overrides (`<dir>/<channel>`).
    skill_overrides: PathBuf,
    /// Per-channel locks serializing turns in the same channel.
    locks: std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl HostTools {
    pub fn new(data_dir: &std::path::Path, repo_skills: Option<PathBuf>) -> Self {
        Self {
            root: data_dir.join("workspaces"),
            repo_skills,
            skill_overrides: data_dir.join("skills"),
            locks: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The channel's lock (created on first use), serializing its host turns.
    pub(crate) fn lock(&self, channel: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().expect("host tools lock map");
        locks.entry(channel.to_string()).or_default().clone()
    }

    /// Create the channel's workspace, (re)provision its skills, and return its
    /// canonical path (the turn's cwd). Provisioning is best-effort — a hiccup
    /// must not fail the turn; the persisted workspace keeps what it had.
    pub(crate) fn prepare(&self, channel: &str) -> anyhow::Result<PathBuf> {
        let workspace = self.root.join(channel);
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("creating the host workspace {}", workspace.display()))?;
        if let Err(error) = provision_skills(
            self.repo_skills.as_deref(),
            Some(&self.skill_overrides.join(channel)),
            &workspace.join("skills"),
        ) {
            tracing::warn!(error = format!("{error:#}"), %channel, "provisioning skills failed");
        }
        std::fs::canonicalize(&workspace)
            .with_context(|| format!("resolving the host workspace {}", workspace.display()))
    }
}

/// Build the skills notice for the prompt by enumerating the skills under
/// `skills_dir` (name + the first line of each `SKILL.md`). Listing them by
/// name makes the model far likelier to use a matching skill than a "go look
/// in skills/" hint. `None` when there are no skills.
pub fn skills_notice(skills_dir: &std::path::Path) -> Option<String> {
    let mut entries: Vec<_> = std::fs::read_dir(skills_dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut items = Vec::new();
    for entry in entries {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let desc = std::fs::read_to_string(entry.path().join("SKILL.md")).ok().and_then(|text| {
            text.lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim_start_matches('#').trim().to_string())
        });
        match desc {
            Some(desc) => items.push(format!("- `{name}`: {desc}")),
            None => items.push(format!("- `{name}`")),
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(format!(
        "You're in a workspace and can run tools. These skills are \
         available — read `skills/<name>/SKILL.md` for how to use one and run \
         its scripts with bash. Prefer a matching skill over ad-hoc \
         commands:\n{}",
        items.join("\n")
    ))
}

/// Build the per-channel sandbox manager from the config. The factory creates
/// (but does not start) one container per channel, rooted at
/// `<data_dir>/sandboxes/<channel>/workspace` — the only path that persists —
/// and provisions skills into `<workspace>/skills` (repo skills, then
/// `<data_dir>/skills/<channel>` overrides on top). Requires the `sandbox`
/// feature.
#[cfg(feature = "sandbox")]
pub fn build_sandbox_manager(
    cfg: &crate::config::Sandbox,
    data_dir: &std::path::Path,
    repo_skills: Option<std::path::PathBuf>,
) -> Arc<tapir_sandbox::SandboxManager> {
    use std::time::Duration;

    use tapir_sandbox::{DockerSandbox, LifecyclePolicy, Sandbox, SandboxConfig, SandboxManager, SystemClock};

    let policy = LifecyclePolicy {
        idle_window: Duration::from_secs(cfg.idle_minutes * 60),
        ..LifecyclePolicy::default()
    };
    let sandboxes = data_dir.join("sandboxes");
    let skill_overrides = data_dir.join("skills");
    let aws_src = data_dir.join("aws");
    let image = cfg.image.clone();
    let memory = cfg.memory.clone();
    let cpus = cfg.cpus.clone();
    let pids = cfg.pids;
    let network = cfg.network.clone();
    let env_names = cfg.env.clone();
    // Run the container as the host user so files it writes under the
    // bind-mounted workspace are host-owned, not root.
    let user = format!("{}:{}", unsafe { libc::getuid() }, unsafe { libc::getgid() });

    SandboxManager::new(policy, Arc::new(SystemClock), move |channel| {
        let workspace = sandboxes.join(channel).join("workspace");
        std::fs::create_dir_all(&workspace)?;
        // docker bind mounts need an absolute host path (a relative one is read
        // as a named volume and rejected).
        let workspace = std::fs::canonicalize(&workspace)?;
        // Provisioning is best-effort: a hiccup (e.g. a file the container
        // wrote as root under rootful docker) must not fail sandbox creation;
        // the persisted workspace keeps whatever was there.
        if let Err(error) = provision_skills(
            repo_skills.as_deref(),
            Some(&skill_overrides.join(channel)),
            &workspace.join("skills"),
        ) {
            tracing::warn!(error = format!("{error:#}"), %channel, "provisioning skills failed");
        }
        // Seed AWS config (the dev SSO profile) from <data_dir>/aws; the SSO
        // token cache then lives in /workspace/.aws (HOME=/workspace) and
        // persists with the workspace.
        if aws_src.is_dir()
            && let Err(error) = copy_dir_all(&aws_src, &workspace.join(".aws"))
        {
            tracing::warn!(error = format!("{error:#}"), %channel, "seeding aws config failed");
        }
        let mut config = SandboxConfig::new(channel, workspace);
        config.image = image.clone();
        config.memory = memory.clone();
        config.cpus = cpus.clone();
        config.pids_limit = pids;
        config.network = network.clone();
        config.user = Some(user.clone());
        // Pass through configured env names, taking values from the bot's own
        // environment (so they never appear in argv).
        config.env = env_names
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        Ok(Arc::new(DockerSandbox::new(config)?) as Arc<dyn Sandbox>)
    })
}

/// Provision the skills tree into `dest`: the repo skills first, then the
/// per-channel overrides on top (channel files win). Missing sources are
/// skipped.
fn provision_skills(
    repo: Option<&std::path::Path>,
    channel: Option<&std::path::Path>,
    dest: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for src in [repo, channel].into_iter().flatten() {
        if src.is_dir() {
            copy_dir_all(src, dest)?;
        }
    }
    Ok(())
}

/// Recursively copy `src` into `dst`, overwriting files that already exist.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{HostTools, provision_skills, skills_notice};

    #[test]
    fn host_tools_prepare_provisions_skills_and_returns_the_workspace() {
        let base = std::env::temp_dir().join(format!("tapir-host-{}", std::process::id()));
        let data = base.join("data");
        let repo = base.join("repo-skills");
        std::fs::create_dir_all(repo.join("hello")).unwrap();
        std::fs::write(repo.join("hello/SKILL.md"), "repo version").unwrap();

        let host = HostTools::new(&data, Some(repo));
        let workspace = host.prepare("C123").expect("prepare succeeds");

        // The workspace is <data>/workspaces/C123 (canonicalized).
        assert_eq!(workspace, std::fs::canonicalize(data.join("workspaces/C123")).unwrap());
        assert_eq!(
            std::fs::read_to_string(workspace.join("skills/hello/SKILL.md")).unwrap(),
            "repo version",
            "repo skills are provisioned into the workspace"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn host_tools_lock_is_per_channel_and_stable() {
        let host = HostTools::new(std::path::Path::new("/tmp/unused"), None);
        let a1 = host.lock("C1");
        let a2 = host.lock("C1");
        let b = host.lock("C2");
        assert!(std::sync::Arc::ptr_eq(&a1, &a2), "same channel reuses its lock");
        assert!(!std::sync::Arc::ptr_eq(&a1, &b), "different channels get different locks");
    }

    #[test]
    fn skills_notice_enumerates_the_skills_by_name() {
        // Build a synthetic skills dir so this lib test stays independent of any
        // specific bot's skills/ content.
        let base = std::env::temp_dir().join(format!("tapir-notice-{}", std::process::id()));
        let skill = base.join("sample");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# sample — does a thing\n\nbody").unwrap();

        let notice = skills_notice(&base).expect("a skills dir with entries");
        assert!(notice.contains("`sample`"), "lists the skill name: {notice}");
        assert!(notice.contains("does a thing"), "includes the description: {notice}");
        assert!(skills_notice(std::path::Path::new("/no/such/dir")).is_none());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn provision_merges_repo_then_channel_overrides() {
        let base = std::env::temp_dir().join(format!("tapir-skills-{}", std::process::id()));
        let repo = base.join("repo");
        let channel = base.join("channel");
        let dest = base.join("dest");
        std::fs::create_dir_all(repo.join("hello")).unwrap();
        std::fs::write(repo.join("hello/SKILL.md"), "repo version").unwrap();
        std::fs::create_dir_all(channel.join("hello")).unwrap();
        std::fs::write(channel.join("hello/SKILL.md"), "channel version").unwrap();
        std::fs::write(channel.join("hello/extra.sh"), "echo hi").unwrap();

        provision_skills(Some(&repo), Some(&channel), &dest).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest.join("hello/SKILL.md")).unwrap(),
            "channel version",
            "the channel override wins"
        );
        assert!(dest.join("hello/extra.sh").exists(), "channel-only file is copied");

        // A missing source is skipped, not an error.
        let only_repo = base.join("only-repo-dest");
        provision_skills(Some(&repo), Some(&base.join("nope")), &only_repo).unwrap();
        assert_eq!(std::fs::read_to_string(only_repo.join("hello/SKILL.md")).unwrap(), "repo version");

        let _ = std::fs::remove_dir_all(&base);
    }
}
