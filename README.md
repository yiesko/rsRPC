<div align=center>
  <h1>rsRPC</h1>

  <div align="center">
    <img src="https://img.shields.io/github/actions/workflow/status/SpikeHD/rsRPC/build.yml" />
    <img src="https://img.shields.io/github/actions/workflow/status/SpikeHD/rsRPC/code_quality.yml?label=code quality" />
    <img src="https://img.shields.io/github/repo-size/SpikeHD/rsRPC" />
  </div>
  <p>Alternative Discord RPC server CLI tool and Rust library, inspired by <a href="https://github.com/OpenAsar/arRPC">arRPC</a></p>
</div>

# Features

* Process detection (with 64-bit path normalization like arrpc/pog5-rsrpc, plus Steam AppId and conservative exe-stem fallback for DB entries with empty `executables`)
* IPC/Socket-based RPC detection
* Websocket-based RPC detection (loopback only)
* Bridge that forwards game activities to web clients via both a **JSON** port (`1337`) and a **MessagePack** port (`1338`)
* arRPC-shaped `SET_ACTIVITY` confirmation replies on both IPC and websocket transports
* Handshake validation (invalid versions / missing client IDs are rejected with close codes)
* `INVITE_BROWSER`, `DEEP_LINK`, `CONNECTIONS_CALLBACK` support
* Bundled offline detectable snapshot with optional automatic database refresh (fetch the detectable list every hour, like pog5-rsrpc)
* `plugin/rsrpc.js` - optional Vencord plugin / userscript / Node client that receives activity from the bridge (not embedded in the Rust binary)
* Custom overrides via `overrides.json` (added on the fly, bypass the OS filter so win32 entries work under Proton/Wine)
* Adding new processes on the fly
* Manually triggering scans
* Single-shot diagnostics via `--list-detected`

# Building

## Requirements

- [Cargo and Rust](https://www.rust-lang.org/) 1.88+ (edition 2024 + let-chains)

## Testing it out

1. Download a binary from [releases](https://github.com/SpikeHD/rsRPC/releases), [GitHub Actions](https://www.github.com/SpikeHD/rsRPC/actions) or build it yourself below!
2. If you just want to use the bundled detectable snapshot, just run the binary (works fully offline, no `detectable.json` needed)!
3. If you want to use your own detectable list, place a `detectable.json` file in the same directory as the binary (you can use [the arRPC one](https://raw.githubusercontent.com/OpenAsar/arrpc/main/src/process/detectable.json) as an example), then run the binary with `./rsrpc-cli -d ./detectable.json`

### CLI options

```
  -d, --detectable-file <FILE>    Path to a custom detectable games list
  -n, --no-process-scan           Disable process detection
                                  (alias: --no-process-scanning)
      --bridge-port <PORT>        Bridge JSON port (default: 1337)
      --msgpack-port <PORT>       Bridge MessagePack port (default: 1338)
      --ws-port-start <PORT>      First websocket port for games (default: 6463)
      --ws-port-end <PORT>        Last websocket port for games, inclusive (default: 6472)
      --scan-interval-secs <SECS> Process scan interval in seconds (default: 5)
      --db-url <URL>              Fetch the detectable list from this URL
                                  (with --enable-db-update and no --db-url,
                                  defaults to https://discord.com/api/v9/applications/detectable)
      --enable-db-update          Refresh the detectable list every hour
      --overrides-file <FILE>     Path to a JSON array of DetectableActivity
                                  used as custom overrides (default: $RSRPC_OVERRIDES_FILE,
                                  $XDG_CONFIG_HOME/rsrpc/overrides.json,
                                  ~/.config/rsrpc/overrides.json)
      --list-detected             Run a single process scan, print detected
                                  games and exit (main DB only; custom
                                  overrides require a running server)
  -D, --debug                     Print the resolved configuration
```

Every option also has a corresponding environment variable (e.g. `RSRPC_BRIDGE_PORT`, `RSRPC_MSGPACK_PORT`, `RSRPC_OVERRIDES_FILE`, `RSRPC_LIST_DETECTED`, `RSRPC_DEBUG`).

### Detectable database (offline snapshot & refresh)

* Without flags the CLI uses the bundled snapshot (`lib/resources/detectable.json`, embedded via `detection::BUNDLED_DETECTABLE`), so it works offline.
* `--db-url <URL>` fetches and trims the list at startup (keeps only `id/name/hook`, `executables{name,is_launcher,os,arguments}` and `third_party_skus{distributor,id}`), with fallback to the bundled snapshot on failure.
* `--enable-db-update` keeps refreshing that list every hour in the background. Without `--db-url` it defaults to `https://discord.com/api/v9/applications/detectable`.
* Regenerate the snapshot with: `cargo run --manifest-path tools/updater/Cargo.toml` (writes `lib/resources/detectable.json`).

### Process detection notes

* Executable matching uses Aho-Corasick over reversed paths with `64`/`.x64`/`x64`/`_64` variants (e.g. `wow64.exe` matches `wow.exe`).
* Entries with empty `executables` are still matched via Steam AppId (Linux reads `SteamAppId` from `/proc/<pid>/environ`) or via an exact exe-stem == multi-word game-name fallback (e.g. `how to fish.exe` → `How to Fish`; single-word names like `fish` never match).
* The main DB is filtered by executable OS (`win32`/`darwin`/`linux`); custom overrides from `overrides.json`/`append_detectables` bypass that filter so win32-only entries are detected under Proton/Wine, and always win over the main DB.

### Custom overrides (`overrides.json`)

File contains a JSON array of `DetectableActivity` objects. Resolution order: `--overrides-file` > `$RSRPC_OVERRIDES_FILE` > `$XDG_CONFIG_HOME/rsrpc/overrides.json` > `~/.config/rsrpc/overrides.json`. Missing file means no overrides. Loaded after `start()` via `append_detectables`.

### Diagnostics

```bash
./rsrpc-cli --list-detected
# How to Fish (id 4001890) pid 1234
```

Uses the main DB only; `overrides.json` diagnostics require a running server.

## Building the binary

1. Clone the repository
2. Generate the offline detectable snapshot (requires network once; the
   output `lib/resources/detectable.json` is gitignored by design):
   `cargo run --manifest-path tools/updater/Cargo.toml`
3. `cargo build -p rsrpc-cli --release`
4. Your file will be in `target/release/`

## Using as a library

1. Add the following to your `Cargo.toml` file:

```toml
[dependencies]
rsrpc = { git = "https://www.github.com/SpikeHD/rsRPC", tag = "VERSION_NUMBER_HERE" }
```

2. Use the library in your code:

```rust
use rsrpc::{RPCServer, RPCConfig};

fn main() {
  let mut server = RPCServer::from_file("./detectable.json", RPCConfig::default())
    .expect("Failed to create RPCServer");
  server.start();
}
```

You can also grab the `detectable.json` programmatically and pass it via string:
```rust
use rsrpc::{RPCServer, RPCConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let detectable = reqwest::blocking::get("https://raw.githubusercontent.com/OpenAsar/arrpc/main/src/process/detectable.json")?.text()?;
  let mut server = RPCServer::from_json_str(detectable, RPCConfig::default())?;

  server.start();
  Ok(())
}
```

Works fully offline with the bundled snapshot (no file/network needed):
```rust
let mut server = RPCServer::from_bundled(RPCConfig::default())
  .expect("Failed to create RPCServer");
server.start();
```

### `RPCConfig` fields (defaults)

| Field | Default | Meaning |
|---|---|---|
| `port` | `1337` | Bridge JSON port (`--bridge-port` / `RSRPC_BRIDGE_PORT`) |
| `msgpack_port` | `1338` | Bridge MessagePack port (`--msgpack-port` / `RSRPC_MSGPACK_PORT`) |
| `ws_port_start` / `ws_port_end` | `6463` / `6472` | Game websocket range, inclusive |
| `scan_interval_secs` | `5` | Process scan interval (`--scan-interval-secs` / `RSRPC_SCAN_INTERVAL`) |
| `db_url` / `enable_db_update` | `None` / `false` | Hourly DB refresh source + toggle |

### Runtime API

```rust
use rsrpc::DetectedGame;

// Single scan without threads (main DB only; returns id/name/pid).
let games: Vec<DetectedGame> = server.detect_once()?;

// Add/remove entries after start() (bypass the OS filter, win over main DB).
server.append_detectables(overrides);
server.remove_detectable_by_name("Game Name".to_string());

// Manual rescan after start().
server.scan_for_processes();

// OBS/streaming flag callback (must be set before start()).
server.on_process_scan_complete(|state| {
  println!("obs open: {}", state.obs_open);
});
```

## Web client (browser / Vencord, optional)

> `plugin/rsrpc.js` is **not** part of the Rust build: nothing is bundled via `include_str!`/`build.rs`, and the server works without it. Any arRPC-compatible websocket client can consume the bridge.

The bridge exposes two websocket endpoints:

| Port | Protocol | URL |
|------|----------|-----|
| 1337 | JSON | `ws://127.0.0.1:1337?format=json` |
| 1338 | MessagePack | `ws://127.0.0.1:1338?format=msgpack` |

The protocol is auto-detected, so connecting to either port works regardless of the `format` query parameter. Use `plugin/rsrpc.js` (Vencord plugin or userscript) or instantiate `RsRpcClient` directly:

```javascript
const client = new RsRpcClient(true); // true = use MessagePack
client.onActivity = (activity) => console.log('Activity:', activity);
client.connect();
```

Notes:

* Default is arRPC-compatible JSON (`new RsRpcClient(false)`); no extra dependency.
* MessagePack (`true`) needs `@msgpack/msgpack`: `<script src="https://unpkg.com/@msgpack/msgpack"></script>`.
* Ports are configurable (`--bridge-port` / `--msgpack-port`); the client accepts `options: { jsonPort, msgpackPort, reconnectInterval }`.

## Testing & benchmarks

* Unit and integration tests: `cargo test`
* Benchmarks (JSON vs MessagePack): `cargo bench`

Unit tests live in `lib/src/tests/` (one module per area), integration
tests in `lib/tests/`, benchmarks in `lib/benches/`. All of them need the
generated snapshot - run the updater once first (see "Building the binary").

## Credits

* [OpenAsar / arRPC](https://github.com/OpenAsar/arRPC) - the original project this work is inspired by. The `detectable.json` format, the executable-matching checks, and the arRPC-shaped bridge behavior (replies, websocket protocol) follow its design.
* [pog5 / rsrpc](https://github.com/pog5/rsrpc) - reference for process-detection parity: 64-bit executable path variants, executable argument checks, the hourly detectable-database refresh, and the integration test coverage.
