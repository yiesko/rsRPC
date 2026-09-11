# Systemd user units — complete guide (rsRPC)

Run `rsrpc-cli` as a user service — no root needed — with every
environment variable, flag interaction, and operational gotcha in one
place.

## 1. System vs user units

|  | System unit | User unit |
|---|---|---|
| Path | `/usr/lib/systemd/system/`, `/etc/systemd/system/` | `~/.config/systemd/user/` |
| Needs root | yes | **no** |
| Lifetime | whole machine | your login session (or boot, with linger) |

All commands are identical, with `--user` in the middle:

```bash
systemctl --user status rsrpc.service
```

## 2. Anatomy of `rsrpc.service`

```ini
[Unit]
Description=rsRPC - Discord Rich Presence bridge
After=graphical-session.target      # only start after the graphical session
PartOf=graphical-session.target     # if the session dies, it dies too

[Service]
Type=exec                           # "started" = binary running
ExecStart=%h/.local/bin/rsrpc-cli --enable-db-update
Environment=RSRPC_BRIDGE_PORT=1337
Restart=on-failure                  # died with an error? try again
RestartSec=5s                       # wait 5s between attempts
ExecStartPre=-/bin/sh -c 'rm -f %t/discord-ipc-*'   # stale socket sweep (- = ignore failure)

[Install]
WantedBy=graphical-session.target   # "start me with the graphical session"
```

Handy specifiers: `%h` home, `%t` runtime dir (`/run/user/1000`),
`%u` username. `graphical-session.target` (instead of `default.target`)
is deliberate: the bridge serves the desktop Discord client.

## 3. Complete environment reference

Every CLI flag also has an env var (clap `env`: flag wins when both
are given). Booleans accept `1`/`true`.

### Transports and ports

| Variable | Default | Meaning |
|---|---|---|
| `RSRPC_BRIDGE_PORT` | `1337` | JSON bridge range start (`--bridge-port`) |
| `RSRPC_BRIDGE_PORT_END` | `1347` | JSON bridge range end, inclusive |
| `RSRPC_MSGPACK_PORT` | `1338` | MessagePack bridge port (steps aside on collision) |
| `RSRPC_WS_PORT_START` / `RSRPC_WS_PORT_END` | `6463` / `6472` | Game websocket range, inclusive |
| `XDG_RUNTIME_DIR` / `TMPDIR` / `TMP` / `TEMP` | — | IPC socket dir, first set wins, else `/tmp` (`discord-ipc-0`…`9`) |

### Detection and database

| Variable | Default | Meaning |
|---|---|---|
| `RSRPC_SCAN_INTERVAL` | `5` | Process scan seconds (`--scan-interval-secs`) |
| `RSRPC_NO_PROCESS_SCAN` | unset | `1` disables process detection entirely |
| `RSRPC_DETECTABLE_FILE` | unset | Custom `detectable.json` path (`-d`) |
| `RSRPC_DB_URL` | unset | Fetch DB from this URL at startup |
| `RSRPC_ENABLE_DB_UPDATE` | unset | `1` = hourly background DB refresh (falls back to the official URL) |
| `RSRPC_OVERRIDES_FILE` | unset | `overrides.json` path (else `$XDG_CONFIG_HOME/rsrpc/overrides.json`, else `~/.config/rsrpc/overrides.json`) |
| `RSRPC_OVERRIDES_DIR` | unset | `overrides.d/` path (else `$XDG_CONFIG_HOME/rsrpc/overrides.d`, else `~/.config/rsrpc/overrides.d`); every `*.json` inside merges in |
| `RSRPC_EXCLUSIONS_URL` | unset | Discord detection exclusions source (defaults to the official endpoint with `--enable-db-update`) |
| `RSRPC_STEAM_ROOT` | unset | Exclusive Steam root override (nothing else is consulted) |
| `RSRPC_STEAM_LIBRARIES` | unset | Extra Steam library roots, `:`-separated, merged with auto-discovery |
| `RSRPC_IGNORE_IDS` | unset | Comma-separated app IDs the scanner never publishes (full silence; forwarded client frames still pass) |

> Bool flags accept `1/0/true/false/yes/no/on/off` (so `RSRPC_DEBUG=1` and friends work).
>
> Sandboxing note: if the unit restricts address families
> (`RestrictAddressFamilies`), add `AF_NETLINK` to the list — otherwise the
> event-driven process watcher cannot subscribe and the scanner silently
> stays on polling (diagnose via the missing `watcher live` line).

### Identity (`READY` user)

| Variable | Default | Meaning |
|---|---|---|
| `RSRPC_USER_ID` | `1045800378228281345` | User id shown in `READY` |
| `RSRPC_USER_USERNAME` | `arRPC` | Username shown in `READY` |
| `RSRPC_USER_GLOBAL_NAME` | `arRPC` | Display name shown in `READY` |
| `RSRPC_USER_DISCRIMINATOR` | `0000` | Discriminator shown in `READY` |
| `RSRPC_USER_AVATAR` | `cfefa4…` | Avatar hash shown in `READY` |

Blank values are ignored (defaults kept). Bridge clients can patch at
runtime via `SET_USER` and restore via `RESET_USER`.

### Logging and diagnostics

| Variable | Default | Meaning |
|---|---|---|
| `RSRPC_LOGS_ENABLED` | unset (binary sets `1`) | Master switch: no logs at all unless `1` |
| `RSRPC_LOG_LEVEL` | `info` | Floor: `debug` / `info` / `warn` / `error` |
| `RSRPC_DEBUG` | unset | `1` forces debug + prints resolved config (`-D` does the same) |
| `RSRPC_STATE_FILE` | unset | Any value writes `<tmpdir>/rsrpc-state-{0..9}` snapshots |
| `RSRPC_LIST_DETECTED` | unset | One-shot: print detected games and exit |
| `RSRPC_LIST_DATABASE` | unset | One-shot: print DB summary and exit |

## 4. Drop-ins: configure without editing the unit

Instead of touching the `.service` file, create
`~/.config/systemd/user/rsrpc.service.d/override.conf`:

```ini
[Service]
Environment=RSRPC_LOG_LEVEL=debug
Environment=RSRPC_IGNORE_IDS=0000000000000000000
```

Only what is in the drop-in overrides/adds. Inspect the result with
`systemctl --user cat rsrpc.service`. Apply with:

```bash
systemctl --user daemon-reload   # ALWAYS after editing units or drop-ins
systemctl --user restart rsrpc.service
```

## 5. Lifecycle

```bash
systemctl --user daemon-reload        # ALWAYS after editing units or drop-ins
systemctl --user enable rsrpc.service # start at login
systemctl --user start|stop|restart rsrpc.service
systemctl --user status rsrpc.service # state + recent log lines
systemctl --user is-active rsrpc.service
systemctl --user cat rsrpc.service    # shows the unit + applied drop-ins
```

## 6. Logs (everything goes to the journal)

```bash
journalctl --user -u rsrpc.service -f              # live
journalctl --user -u rsrpc.service --since "10 min ago" | grep -iE "warn|error"
journalctl --user -u rsrpc.service -b              # since boot
```

`INFO` lines are one per state change (detects, clears, connects,
hourly DB checks); per-tick chatter needs debug.

## 7. Surviving logout and reboot (`linger`)

```bash
loginctl show-user $USER | grep Linger   # must say Linger=yes
sudo loginctl enable-linger $USER        # if not (needs sudo, once)
```

With linger, `enable`d units boot even without a graphical login.

## 8. Updating the binary safely

The service executes the file in place — overwriting a running binary
fails with `Text file busy`. Always:

```bash
systemctl --user stop rsrpc.service
cp ./target/release/rsrpc-cli ~/.local/bin/rsrpc-cli
systemctl --user start rsrpc.service
systemctl --user status rsrpc.service
```

## 9. Known gotchas

- **Restart with a busy IPC socket**: normal — the PING probe detects
  the live holder and takes `discord-ipc-1`; it goes back to `-0` on
  the next swap. WARN at that moment is expected, not an error.
- **SIGTERM vs SIGINT**: `stop` sends SIGTERM — file cleanup
  (socket/state) runs in the Ctrl+C handler; after `stop`, leftover
  files are reclaimed automatically on next boot. Known behavior,
  not a bug.
- **No `--ignore-ids` needed for companions**: rsRPC ≥ 0.32.0 hands the
  slot over (generic shows → IPC takes over → generic resumes on
  clear). The flag stays as full-silence opt-in.

## 10. Creating a unit from scratch (recipe)

```bash
nano ~/.config/systemd/user/my-service.service   # paste [Unit]/[Service]/[Install]
systemd-analyze --user verify my-service.service # validate syntax
systemctl --user daemon-reload
systemctl --user enable --now my-service.service  # enable + start in one go
systemctl --user status my-service.service
```

Rules of thumb: one concern per unit; `Type=exec` + `Restart=on-failure`
for daemons; prefer `Environment=`/drop-ins over wrapper scripts;
secrets go in `EnvironmentFile=` (mode `600`), never in the unit.
