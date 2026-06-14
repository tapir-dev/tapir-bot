# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-06-14

### Added

- Initial release of the Slack backend: the Socket Mode runtime and read loop,
  the Web API client, CommonMark/GFM → Block Kit rendering with streaming
  edits, and the Slack-side config (`reactions`, `access`). Implements
  `ChatBackend` and a streaming `ReplySink`.
