# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-06-14

### Added

- Initial release of the backend-neutral bot engine: the neutral config
  (`agent`/`storage`/`sandbox`), the turn lifecycle, conversation memory and
  `MEMORY.md` facts, the `!`-commands, the reusable access policy
  (`Access`/`LoopGuard`), tool execution (text-only/host/sandbox) with skills,
  and the `ChatBackend` / `ReplySink` extension points.
