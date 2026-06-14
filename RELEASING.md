# Releasing

tapir-bot is released from your machine through `just` — no CI yet. Each crate
is versioned and released **independently**, and releases are **GitHub-only**: a
`<crate>-vX.Y.Z` tag and a GitHub release per crate, with no `cargo publish`.
tapir-bot is consumed as a git dependency, not from crates.io.

You decide which crates a change touches. A change to `tapir-bot-core` bumps
`tapir-bot-core`; because the `tapir-bot` facade re-exports it, it usually bumps
`tapir-bot` too, but **not** `tapir-bot-slack` (whose own API is unchanged).

## Prerequisites

- `just` installed and logged in with `gh` — the recipes read the token via
  `gh auth token`. No crates.io token is needed.
- A clean working tree (the `release` recipe refuses to run otherwise).

## Steps

For each crate you're releasing (e.g. `tapir-bot-core`):

1. Bump `version` in `tapir-bot-core/Cargo.toml`.
2. If a dependent must require the new version, bump it in the root
   `[workspace.dependencies]` (e.g. `tapir-bot-core = { ..., version = "0.2.0" }`)
   and release that dependent too.
3. Add a `## [X.Y.Z] - YYYY-MM-DD` section to `tapir-bot-core/CHANGELOG.md`.
4. Commit and push:

   ```sh
   git add tapir-bot-core/Cargo.toml Cargo.lock tapir-bot-core/CHANGELOG.md
   git commit -m "feat(core): ..."
   git push
   ```

5. Preview the notes, then cut the release:

   ```sh
   just release-notes tapir-bot-core   # sanity-check the extracted notes
   just release tapir-bot-core         # tag tapir-bot-core-vX.Y.Z + GitHub release
   ```

`just release <crate>` reads the version from the crate's `Cargo.toml`, tags
`<crate>-vX.Y.Z`, pushes the tag, and creates a GitHub release whose body is the
matching `CHANGELOG.md` section.

## Why GitHub-only?

The project is consumed as a git dependency and lives under the `tapir` brand
regardless of any registry. Publishing only to GitHub keeps things simple;
consumers depend on it via git:

```toml
tapir-bot = { git = "https://github.com/tapir-dev/tapir-bot" }
```
