# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Parse `reaction_added` Socket Mode events and surface them through the new
  `BackendObserver::reaction` hook (reactions on non-message items are acked
  and dropped).
- `SlackBackend::access_handle` — an `Arc<RwLock<Access>>` handle to swap the
  access allowlist at runtime; the read loop re-reads it per event (the
  per-turn loop cap stays fixed at startup).
- `SlackBackend::injector` and the `Injected` action enum — post a message or
  run an agent turn in a thread from outside the read loop (the seam an
  event-driven consumer such as a webhook handler needs). Serviced on a
  dedicated task across reconnects; injected actions bypass the access policy.

## [0.1.0] - 2026-06-14

### Added

- Initial release of the Slack backend: the Socket Mode runtime and read loop,
  the Web API client, CommonMark/GFM → Block Kit rendering with streaming
  edits, and the Slack-side config (`reactions`, `access`). Implements
  `ChatBackend` and a streaming `ReplySink`.
