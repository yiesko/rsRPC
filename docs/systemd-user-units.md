# Systemd user units - practical guide

How to operate user services (`rsrpc.service`, `wwrpc.service`, or any
other) without root, with real examples from this setup.

## 1. The concept

- **System unit** (`/usr/lib/systemd/...`, needs root) = machine services.
- **User unit** (`~/.config/systemd/user/`) = **your** services, running
  as your user, no root needed. That's what we use: rsrpc and wwrpc come
  up at your login.
- Commands are the same, with `--user` in the middle:
  `systemctl --user status/start/stop/restart/enable`.

## 2. Anatomy of a unit

Real example (`~/.config/systemd/user/rsrpc.service`, trimmed):

```ini
[Unit]
Description=rsRPC - Discord Rich Presence bridge
After=graphical-session.target      # only start after the graphical session
PartOf=graphical-session.target     # if the session dies, it dies too

[Service]
Type=exec                           # "started" = binary running
ExecStart=%h/.local/bin/rsrpc-cli --enable-db-update
Environment=RSRPC_BRIDGE_PORT=1337  # env var (same as exporting first)
Restart=on-failure                  # died with an error? try again
RestartSec=5s                       # wait 5s between attempts

[Install]
WantedBy=graphical-session.target   # "start me with the graphical session"
```

Handy specifiers: `%h` = home, `%t` = runtime dir (`/run/user/1000`),
`%u` = username.

## 3. Drop-ins: customize without editing the original

Instead of touching the `.service` file, create
`~/.config/systemd/user/rsrpc.service.d/override.conf`:

```ini
[Service]
Environment=RSRPC_LOG_LEVEL=debug
```

Only what is in the drop-in overrides/adds. That's how `RSRPC_IGNORE_IDS`
came and went - without touching the base unit.

## 4. Lifecycle

```bash
systemctl --user daemon-reload        # ALWAYS after editing/creating units or drop-ins
systemctl --user enable rsrpc.service # start at login
systemctl --user start|stop|restart rsrpc.service
systemctl --user status rsrpc.service # state + recent log lines
systemctl --user is-active rsrpc.service
systemctl --user cat rsrpc.service    # shows the unit + applied drop-ins
```

## 5. Logs (everything goes to the journal)

```bash
journalctl --user -u rsrpc.service -f              # live
journalctl --user -u rsrpc.service --since "10 min ago" | grep -iE "warn|error"
journalctl --user -u rsrpc.service -b              # since boot
```

## 6. Surviving logout/reboot (`linger`)

```bash
loginctl show-user $USER | grep Linger   # must say Linger=yes
sudo loginctl enable-linger $USER        # if not (needs sudo, once)
```

With linger, your units boot even without a graphical login.
