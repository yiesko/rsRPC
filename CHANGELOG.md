# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.31.0] - 2026-09-07

### Added
- Install-folder fallback in process detection (Hydra / non-Steam layouts
  with generic exes), cwd-joined probe for bare-exe Proton launches.
- Suspended (SIGSTOP'd) processes count as absent; republished on resume.
- Conditional database refresh (ETag + content hash): unchanged hours cost
  one header round trip, no parse or rebuild.
- Startup database ownership moves to the scanner (no pinned duplicate
  generation); refresh ETag seeded from the startup fetch.
- Fail-fast supervision: worker panics exit for systemd restart; hot-path
  channel sends are fail-soft.
- Bridge backpressure: dead websocket clients are pruned on failed send.
- `spectate`/`match` activity secrets preserved through the bridge.
- Empty database updates refused (keeps serving current data).
- CI: test + doc jobs, `--locked` everywhere, modern actions, Node 24
  runtimes, cross-platform (Linux/macOS/Windows) clippy.

### Fixed
- Stuck presence after game close (dual-keyspace clear: IPC pid-keyed vs
  process app-id-keyed desync).
- Server panic on malformed WS disconnect without args.
- macOS/Windows builds (Linux-only `malloc_trim`, `read_steam_app_id`,
  `parse_stat_state` gating; per-OS memory release).

### Changed
- Scan I/O: `/proc/<pid>/environ` read only on executable-pattern misses.
- Logging is lazy (no allocation when disabled); DB parses skip the JSON
  DOM (`serde` ignores unknown fields).
- Path-variant scratch buffers reused across processes/scans.

## [0.30.0] - 2026-09-06

### Added
- Dual-protocol bridge (JSON + MessagePack), bundled offline detectable
  snapshot with hourly refresh, custom overrides.
- Install-folder fallback in process detection.
- Single-shot `--list-detected` diagnostics.

### Fixed
- Stuck presence after game close.

[Unreleased]: https://github.com/yiesko/rsRPC/compare/v0.31.0...HEAD
[0.31.0]: https://github.com/yiesko/rsRPC/releases/tag/v0.31.0
[0.30.0]: https://github.com/yiesko/rsRPC/releases/tag/v0.30.0
