# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Proton-aware detection: `win32` executables from the main database are
  now indexed in a fallback automaton on Linux. Wine/Proton games whose
  Steam AppId is unreadable match by path instead of staying invisible.
  Precedence is native patterns, then user overrides, then the Steam
  AppId, then the Proton patterns, then the stem/folder heuristics.
- Alternative titles (`aliases`) are preserved by the trim and indexed
  for stem/folder matching (same conservative multi-word gate as
  canonical names, which win ties). The bundled snapshot was regenerated
  with aliases (1397 entries carry them).
- Discord detection exclusions (`GET /games/detectable/exclusions`):
  installer/crash-reporter basenames + regex patterns are dropped before
  any matching. Fetched hourly alongside the DB when `--enable-db-update`
  is set (`--exclusions-url`/`RSRPC_EXCLUSIONS_URL` overrides); empty set
  behaves exactly like before.
- `tools/updater` now uses the shared `rsrpc::detection::trim_detectable`
  instead of a hand-rolled copy (the copy had silently dropped aliases
  from the bundled snapshot).

## [0.32.2] - 2026-09-09

### Fixed
- Generic process payloads carried `timestamps.start` as a string;
  strict clients (Equibop) silently drop mistyped frames, so generic
  cards never displayed. Epoch millis are now emitted as a number end
  to end (`DetectableActivity.timestamp`, `ScannedGame.start`,
  `ProcessTimestamps.start`).
- The scan loop forwarded only the first detected game, starving
  co-running games (WuWa + NTE). Every detected game now gets its own
  per-slot event with per-id dedup and no cross-clearing; the null
  event drains every armed slot.
- App slots whose IPC/WS owner died without CLEAR stayed suppressed by
  a ghost owner forever (`send_empty` carries no app id). Abrupt closes
  now release every slot owned by the dead pid and resume their
  generics.
- Steam AppId fallback via command line (`reaper SteamLaunch
  AppId=4508340 ...`) when `/proc/<pid>/environ` is unreadable.
  Sandboxed Proton runtimes (pressure-vessel/bwrap) hide environ from
  service contexts while cmdline stays readable — without this, games
  like NTE are invisible to automatic detection.

## [0.32.1] - 2026-09-08

### Added
- `docs/systemd-user-units.md`: practical guide to user units
  (anatomy, drop-ins, lifecycle, logs, linger, gotchas, from-scratch
  recipe) with real examples from this setup.

### Fixed
- `party.privacy` (0 = private, 1 = public) and `status_display_type`
  (0 = name, 1 = state, 2 = details) were silently dropped on parse
  (serde ignores unknown fields) — real SDKs (pypresence, Tauon) send
  them. Both now cross the bridge untouched.

## [0.32.0] - 2026-09-08

### Added
- arRPC protocol parity: `SUBSCRIBE`/`UNSUBSCRIBE` blind ACKs, `Unknown
  command` / `CONNECTIONS_CALLBACK is not supported` ERROR replies
  (code 1000), invite-code validation (4011/4017), malformed-frame
  rejection (4005), 1 MiB IPC payload limit with `1003` close, refusal
  of unknown packet types (1003) instead of misreading them as frames.
- Microsecond/nanosecond timestamp normalization to milliseconds
  (seconds/milliseconds behavior unchanged).
- Bridge port-range scan (`--bridge-port`..`--bridge-port-end`,
  default 1337-1347 like arRPC) with MessagePack collision skip.
- Bounded bridge replay cache (50 entries, oldest-first eviction) with
  30s rebroadcast refresh for late/missed clients.
- Runtime identity: `RSRPC_USER_*` startup overrides plus bridge
  `SET_USER`/`RESET_USER` with ACKs (whitelisted keys only, blank
  strings ignored like the env overrides, huge integers clamped
  instead of wrapping); READY frames reflect the current identity on
  every transport.
- Presence state snapshot (`RSRPC_STATE_FILE=1` writes
  `<tmpdir>/rsrpc-state-{0..9}` with servers + activities, arRPC
  layout, `rsrpc-` prefix); `--list-database` summary diagnostics.
- Official-protocol gaps closed: `GET_USER` (current identity, or
  `null` for other ids) on IPC + websocket; `GIFT_CODE_BROWSER`
  forwarded like the other `*_BROWSER` commands (`4016` on missing
  code); clickable-asset URL fields (`details_url`, `state_url`,
  `large_url`, `small_url`) preserved through the bridge;
  `CURRENT_USER_UPDATE` DISPATCH fanned out to bridge clients on
  `SET_USER`/`RESET_USER`.
- Honest errors for known-but-unbacked commands (shared table):
  OAuth → `5000`, activity invites → `5006`, voice/guilds/overlay/
  store → "requires the real Discord client"; genuinely unknown
  commands still get `Unknown command`.
- IPC-wins handoff: generic process detection shows immediately and
  yields its slot when a game SDK publishes for the same app (last
  publisher wins across companions; stale closes ignored), resuming
  while the game process is still alive when the source clears.
  `--ignore-ids` stays as full-silence opt-in, no longer needed for
  companions.
- Stale IPC socket PING/PONG probe (1s): wedged holders are reclaimed,
  live holders (Discord/arRPC/rsRPC) are left alone.
- `--ignore-ids` / `RSRPC_IGNORE_IDS`: scan-only coexistence filter —
  ignored application IDs behave as absent (null event, clear) so a
  richer publisher owns those slots; forwarded client frames always
  pass, and `--list-detected` still shows ignored games.
- Log severity system (`DEBUG`/`INFO`/`WARN`/`ERROR`): `RSRPC_LOG_LEVEL`
  sets the floor (default `info`), `--debug` / `RSRPC_DEBUG=1` forces
  debug and prints the resolved config; `INFO` keeps the historical
  untagged shape, other levels are tagged.
- Presence log lines: `Published: {name} (app {id}, pid {pid})` via a
  `display_name()` fallback (name → details → state → `?`), `INFO` on
  change and `DEBUG` on duplicate republishes/clears.

### Changed
- Tree-wide log reclassification: per-tick chatter (scan ticks, match
  details, repeat sends) demoted to `DEBUG`, fallbacks/retries/prunes
  to `WARN`, so the default log shows one line per state change.
- Hourly DB refresh logs condensed to one line per check (`DB check:
  unchanged (etag …)` / `same bytes` / `updated: N entries`).
- `SUBSCRIBE`/`UNSUBSCRIBE` and unknown/unbacked commands are answered
  at the edge (ACK/ERROR) and no longer forwarded to bridge clients;
  only `SET_ACTIVITY` and the known secondary commands
  (`INVITE_BROWSER`, `GUILD_TEMPLATE_BROWSER`, `GIFT_CODE_BROWSER`,
  `DEEP_LINK`) reach the bridge.
- State snapshot is rewritten on every 30s refresh tick, so a
  live-but-idle daemon never looks stale to slot reuse.

### Fixed
- `SET_USER` patch can no longer wipe identity fields with blank
  strings or wrap `flags`/`premium_type` on out-of-range integers.

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

[Unreleased]: https://github.com/yiesko/rsRPC/compare/v0.32.2...HEAD
[0.32.2]: https://github.com/yiesko/rsRPC/releases/tag/v0.32.2
[0.32.1]: https://github.com/yiesko/rsRPC/releases/tag/v0.32.1
[0.32.0]: https://github.com/yiesko/rsRPC/releases/tag/v0.32.0
[0.31.0]: https://github.com/yiesko/rsRPC/releases/tag/v0.31.0
[0.30.0]: https://github.com/yiesko/rsRPC/releases/tag/v0.30.0
