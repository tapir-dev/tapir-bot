# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Re-export `ReactionEvent` alongside the other core types.

## [0.1.0] - 2026-06-14

### Added

- Initial release of the facade: the config-driven `Bot` builder that builds the
  engine from a `Config` and runs it on a `ChatBackend`, re-exporting
  `tapir-bot-core` and (behind the default `slack` feature) the Slack backend.
  The container sandbox tool mode is behind the off-by-default `sandbox`
  feature. Ships a runnable `slack_bot` example.
